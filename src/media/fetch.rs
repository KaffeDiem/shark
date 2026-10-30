//! Document-supplied images and videos, downloaded with the system `curl`. HTTPS only, also
//! for redirects; no credentials, cookies or `.curlrc`; bounded in time and size; written
//! atomically into the media cache under a key derived from the URL.
use super::{Kind, Side, Source, cache_dir, kind, produce};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_BYTES: &str = "209715200";

/// A downloaded resource, ready for `MediaView`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Fetched {
    pub(crate) kind: Kind,
    /// A name whose extension says what the bytes are, for `prepare`.
    pub(crate) name: String,
    pub(crate) side: Side,
}

/// GitHub's upload URLs carry no extension; their type comes from the response.
pub(crate) fn attachment(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    rest.starts_with("github.com/user-attachments/assets/")
        || rest.starts_with("private-user-images.githubusercontent.com/")
        || rest.starts_with("user-images.githubusercontent.com/")
        || (rest.starts_with("github.com/") && rest.split('/').nth(3) == Some("assets"))
}

/// The extension of an image or video URL's path, if it names one.
pub(crate) fn url_kind(url: &str) -> Option<Kind> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    kind(path)
}

/// What a response's `Content-Type` makes of the bytes.
pub(crate) fn classify(content_type: &str) -> Option<(Kind, &'static str)> {
    let mime = content_type.split(';').next()?.trim().to_ascii_lowercase();
    Some(match mime.as_str() {
        "image/png" => (Kind::Image, "png"),
        "image/jpeg" | "image/jpg" => (Kind::Image, "jpg"),
        "image/gif" => (Kind::Image, "gif"),
        "image/webp" => (Kind::Image, "webp"),
        "image/svg+xml" => (Kind::Image, "svg"),
        "image/bmp" => (Kind::Image, "bmp"),
        "image/x-icon" | "image/vnd.microsoft.icon" => (Kind::Image, "ico"),
        "image/tiff" => (Kind::Image, "tiff"),
        "image/heic" => (Kind::Image, "heic"),
        "image/avif" => (Kind::Image, "avif"),
        "video/mp4" => (Kind::Video, "mp4"),
        "video/quicktime" => (Kind::Video, "mov"),
        "video/x-m4v" => (Kind::Video, "m4v"),
        "video/webm" => (Kind::Video, "webm"),
        _ => return None,
    })
}

fn key(url: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    format!("dl-{:016x}", h.finish())
}

fn curl() -> PathBuf {
    std::env::var_os("SHARK_CURL")
        .map(PathBuf::from)
        .unwrap_or_else(|| "curl".into())
}

/// Downloads `url` once; later calls reuse the cached file. Blocking: background only.
pub(crate) fn fetch(url: &str) -> Result<Fetched, String> {
    if !url.starts_with("https://") {
        return Err("only HTTPS resources load inline".into());
    }
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let key = key(url);
    let meta = dir.join(format!("{key}.type"));
    let fetched = |ext: &str, kind| {
        let name = format!("attachment.{ext}");
        let file = dir.join(format!("{key}.{ext}"));
        Fetched {
            kind,
            name,
            side: Side {
                sha: key.clone(),
                repo: PathBuf::new(),
                source: Source::Downloaded(file),
            },
        }
    };
    if let Some(ext) = std::fs::read_to_string(&meta).ok()
        && let Some(k) = kind(&format!("x.{}", ext.trim()))
        && dir.join(format!("{key}.{}", ext.trim())).exists()
    {
        return Ok(fetched(ext.trim(), k));
    }
    // Private until classified: a concurrent fetch of the same URL downloads its own copy,
    // and both publish identical bytes with an atomic rename.
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let body = dir.join(format!(".{key}-{}-{seq}.download", std::process::id()));
    let downloaded = download(url, &body, &dir);
    let content_type = match downloaded {
        Ok(t) => t,
        Err(e) => {
            let _ = std::fs::remove_file(&body);
            return Err(e);
        }
    };
    let classified = classify(&content_type)
        .or_else(|| url_kind(url).map(|k| (k, ext_of(url))))
        .filter(|(_, ext)| !ext.is_empty());
    let Some((kind, ext)) = classified else {
        let _ = std::fs::remove_file(&body);
        return Err(match content_type.is_empty() {
            true => "not an image or video".into(),
            false => format!("not an image or video ({content_type}); it may need sign-in"),
        });
    };
    let file = dir.join(format!("{key}.{ext}"));
    if let Err(e) = std::fs::rename(&body, &file) {
        let _ = std::fs::remove_file(&body);
        return Err(e.to_string());
    }
    let _ = produce(&meta, |tmp| {
        std::fs::write(tmp, ext).map_err(|e| e.to_string())
    });
    Ok(fetched(ext, kind))
}

fn ext_of(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let ext = Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    [
        "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg", "tif", "tiff", "heic", "heif",
        "avif", "psd", "mp4", "mov", "m4v", "webm", "mkv", "avi",
    ]
    .into_iter()
    .find(|e| *e == ext)
    .unwrap_or("")
}

/// Runs curl into `out`; returns the final response's content type.
fn download(url: &str, out: &Path, dir: &Path) -> Result<String, String> {
    let out = out.to_string_lossy();
    let args = [
        // Must come first: ignores ~/.curlrc, which could add credentials or headers.
        "-q",
        "--silent",
        "--show-error",
        "--fail",
        "--location",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--max-redirs",
        "5",
        "--connect-timeout",
        "10",
        "--max-time",
        "60",
        "--max-filesize",
        MAX_BYTES,
        "--user-agent",
        "Shark",
        "--header",
        "Accept: image/*,video/*;q=0.9,*/*;q=0.1",
        "--output",
        &out,
        "--write-out",
        "%{content_type}",
        "--",
        url,
    ];
    let mut c = Command::new(curl());
    // No credentials of the user's may reach a document-supplied host.
    c.env_remove("NETRC").env_remove("CURL_HOME");
    crate::git::wait_bounded(c, dir, "curl", &args, 75, false).map_err(|e| {
        match e.find("curl: (") {
            Some(at) => e[at + 6..].trim().to_string(),
            None if e.contains("timed out") => "download timed out".into(),
            None => e,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn classifies_attachments_and_rejects_insecure_urls() {
        assert!(attachment(
            "https://github.com/user-attachments/assets/0a1b2c3d-0000-4000-8000-000000000000"
        ));
        assert!(attachment("https://github.com/o/r/assets/12/0a1b"));
        assert!(!attachment("https://github.com/o/r/blob/main/a.png"));
        assert!(!attachment("http://github.com/user-attachments/assets/x"));
        assert_eq!(
            classify("image/png; charset=binary"),
            Some((Kind::Image, "png"))
        );
        assert_eq!(classify("video/quicktime"), Some((Kind::Video, "mov")));
        assert_eq!(classify("text/html; charset=utf-8"), None);
        assert_eq!(
            url_kind("https://x.test/a/clip.MP4?raw=1"),
            Some(Kind::Video)
        );
        assert_eq!(ext_of("https://x.test/a.JPG#f"), "jpg");
        assert!(
            fetch("http://example.com/a.png")
                .unwrap_err()
                .contains("HTTPS")
        );
        assert!(fetch("file:///etc/passwd").is_err());
    }
}

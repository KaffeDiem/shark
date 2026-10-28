//! Fixed, verified Apple Silicon artifacts. All functions here are blocking.
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::{
    fs,
    path::{Component, Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Clone, Deserialize)]
pub(super) struct Artifact {
    pub name: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub executable: String,
}

pub(super) fn run(root: &Path, program: &str, args: &[&str], seconds: u64) -> Result<String> {
    crate::git::wait_bounded(
        std::process::Command::new(program),
        root,
        program,
        args,
        seconds,
        true,
    )
    .map_err(anyhow::Error::msg)
}

pub(super) fn path(path: &Path) -> Result<&str> {
    path.to_str().context("Language tool path is not UTF-8")
}

pub(super) fn manifest() -> Result<Vec<Artifact>> {
    Ok(serde_json::from_str(include_str!("tool-manifest.json"))?)
}

/// No mutable `current` symlink: only a fully staged exact version can become visible.
pub(super) fn managed(name: &str) -> Result<PathBuf> {
    ensure!(
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
        "Managed language tools require Apple Silicon macOS"
    );
    static INSTALL: Mutex<()> = Mutex::new(());
    let _lock = INSTALL.lock().map_err(|_| {
        anyhow::anyhow!("Language tool installer stopped unexpectedly; restart Shark")
    })?;
    let artifact = manifest()?
        .into_iter()
        .find(|a| a.name == name)
        .context("Language tool is not in Shark's manifest")?;
    install(
        &artifact,
        &crate::state::support_dir().join("language-tools"),
    )
}

fn install(artifact: &Artifact, cache: &Path) -> Result<PathBuf> {
    let parent = cache.join(&artifact.name);
    let destination = parent.join(&artifact.version);
    let executable = destination.join(&artifact.executable);
    if executable.is_file()
        && fs::read_to_string(destination.join(".verified"))
            .ok()
            .as_deref()
            == Some(&artifact.sha256)
    {
        return Ok(executable);
    }
    // Never erase an existing installation, even when it needs repair.
    ensure!(
        !destination.exists(),
        "Incomplete {} installation at {}; move it aside and Retry",
        artifact.name,
        destination.display()
    );
    fs::create_dir_all(&parent)?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let stage = parent.join(format!(
        ".stage-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&stage)?;
    let result = (|| {
        let archive = stage.join("download.tar.gz");
        run(
            &stage,
            "/usr/bin/curl",
            &[
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--connect-timeout",
                "15",
                "--max-time",
                "180",
                "--max-filesize",
                "268435456",
                "--output",
                path(&archive)?,
                &artifact.url,
            ],
            190,
        )?;
        verify(&archive, &artifact.sha256)?;
        let listing = run(&stage, "/usr/bin/tar", &["-tzf", path(&archive)?], 20)?;
        ensure!(
            listing.lines().all(safe_member),
            "Unsafe path in {} archive",
            artifact.name
        );
        run(
            &stage,
            "/usr/bin/tar",
            &["-xzf", path(&archive)?, "-C", path(&stage)?],
            60,
        )?;
        let program = stage.join(&artifact.executable);
        ensure!(
            program.is_file(),
            "{} archive did not contain {}",
            artifact.name,
            artifact.executable
        );
        ensure!(
            program.canonicalize()?.starts_with(stage.canonicalize()?),
            "Language tool leaves its installation directory"
        );
        fs::remove_file(archive)?;
        fs::write(stage.join(".verified"), &artifact.sha256)?;
        fs::rename(&stage, &destination)?;
        Ok(executable)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

pub(super) fn verify(file: &Path, expected: &str) -> Result<()> {
    ensure!(
        expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid artifact checksum"
    );
    let output = run(
        file.parent().context("Missing artifact directory")?,
        "/usr/bin/shasum",
        &["-a", "256", path(file)?],
        30,
    )?;
    if output.split_whitespace().next() != Some(expected) {
        bail!("Language tool checksum mismatch; retry the download");
    }
    Ok(())
}

fn safe_member(member: &str) -> bool {
    !member.is_empty()
        && Path::new(member)
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_has_exact_https_artifacts_and_safe_paths() {
        let artifacts = manifest().unwrap();
        assert_eq!(artifacts.len(), 7);
        for a in artifacts {
            assert!(a.url.starts_with("https://") && a.url.contains(&a.version));
            assert_eq!(a.sha256.len(), 64);
            assert!(a.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(safe_member(&a.executable));
        }
        for path in ["/etc/passwd", "../escape", "a/../../escape", ""] {
            assert!(!safe_member(path));
        }
    }
    #[test]
    fn offline_cache_and_failed_install_preserve_previous_version() {
        let root = std::env::temp_dir().join(format!("shark-install-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut a = manifest().unwrap().remove(0);
        let version = root.join(&a.name).join(&a.version);
        fs::create_dir_all(version.join(Path::new(&a.executable).parent().unwrap())).unwrap();
        fs::write(version.join(&a.executable), "working").unwrap();
        fs::write(version.join(".verified"), &a.sha256).unwrap();
        let interrupted = root.join(&a.name).join(".stage-interrupted");
        fs::create_dir(&interrupted).unwrap();
        fs::write(interrupted.join("download.tar.gz"), "partial").unwrap();
        a.url = "https://127.0.0.1:1/not-available".into();
        assert_eq!(install(&a, &root).unwrap(), version.join(&a.executable));
        a.version = "failed-upgrade".into();
        assert!(install(&a, &root).is_err());
        assert_eq!(
            fs::read_to_string(version.join(&a.executable)).unwrap(),
            "working"
        );
        assert!(!root.join(&a.name).join(&a.version).exists());
        assert_eq!(
            fs::read_to_string(interrupted.join("download.tar.gz")).unwrap(),
            "partial"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

//! Changed-files tree and "viewed" fingerprints for the diff.
use std::collections::{BTreeMap, HashSet};

/// FNV-1a: stable across Rust releases, unlike `DefaultHasher`, so stored fingerprints stay valid.
pub fn fingerprint(bytes: impl AsRef<[u8]>) -> u64 {
    bytes.as_ref().iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x100000001b3)
    })
}

/// Writes `text` unless the file no longer has fingerprint `expected` (then only with `force`);
/// `Ok(None)` when someone else changed it. Returns the new fingerprint. Blocking.
pub fn write_checked(
    abs: &std::path::Path,
    expected: u64,
    text: &str,
    force: bool,
) -> std::io::Result<Option<u64>> {
    write_staged(abs, expected, text, force, || {})
}

fn write_staged(
    abs: &std::path::Path,
    expected: u64,
    text: &str,
    force: bool,
    before_replace: impl FnOnce(),
) -> std::io::Result<Option<u64>> {
    use objc2_foundation::{NSFileManager, NSFileManagerItemReplacementOptions, NSString, NSURL};
    use std::{
        fs,
        io::{self, Write},
        os::unix::fs::{MetadataExt, OpenOptionsExt},
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = abs
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let (target, original) = match fs::canonicalize(abs) {
        Ok(path) => {
            let original = fs::metadata(&path)?;
            (path, Some(original))
        }
        Err(e) if force && e.kind() == io::ErrorKind::NotFound => {
            match fs::symlink_metadata(abs) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
                Ok(_) => return Err(e), // A dangling symlink is not a deleted file.
            }
            let name = abs
                .file_name()
                .ok_or_else(|| io::Error::other("Save requires a file name"))?;
            (fs::canonicalize(parent)?.join(name), None)
        }
        Err(_) if !force => return Ok(None),
        Err(e) => return Err(e),
    };
    if original.as_ref().is_some_and(|m| !m.is_file()) {
        return Err(io::Error::other("Save requires a regular file"));
    }
    if original.as_ref().is_some_and(|m| m.nlink() > 1) {
        return Err(io::Error::other(
            "This file has hard links. Use an external editor to save it; your buffer has been retained.",
        ));
    }
    target
        .to_str()
        .ok_or_else(|| io::Error::other("File path is not UTF-8"))?;
    let before = match original.as_ref().map(|_| fs::read(&target)).transpose() {
        Ok(None) => Vec::new(),
        Ok(Some(bytes)) => bytes,
        Err(_) if !force => return Ok(None),
        Err(e) => return Err(e),
    };
    if !force && fingerprint(&before) != expected {
        return Ok(None);
    }
    let (stage, mut file) = loop {
        let path = target.with_file_name(format!(
            ".shark-save-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => break (path, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let staged = file
        .write_all(text.as_bytes())
        .and_then(|_| {
            original
                .as_ref()
                .map_or(Ok(()), |m| file.set_permissions(m.permissions()))
        })
        .and_then(|_| file.sync_all());
    if let Err(e) = staged {
        let _ = fs::remove_file(&stage);
        return Err(e);
    }
    before_replace();
    let Some(original) = original else {
        if !fs::canonicalize(parent).is_ok_and(|p| Some(p.as_path()) == target.parent()) {
            let _ = fs::remove_file(&stage);
            return Ok(None);
        }
        // Linking publishes the complete file without replacing a concurrent writer's file.
        let created = fs::hard_link(&stage, &target);
        let _ = fs::remove_file(&stage);
        return match created {
            Ok(()) => Ok(Some(fingerprint(text.as_bytes()))),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(e),
        };
    };
    let unchanged = fs::canonicalize(abs).is_ok_and(|p| p == target)
        && fs::metadata(&target).is_ok_and(|m| {
            m.dev() == original.dev() && m.ino() == original.ino() && m.nlink() == 1
        })
        && fs::read(&target).is_ok_and(|bytes| bytes == before);
    if !unchanged {
        let _ = fs::remove_file(&stage);
        return Ok(None);
    }
    let url = |p: &std::path::Path| -> io::Result<_> {
        Ok(NSURL::fileURLWithPath(&NSString::from_str(
            p.to_str()
                .ok_or_else(|| io::Error::other("File path is not UTF-8"))?,
        )))
    };
    let result = NSFileManager::defaultManager()
        .replaceItemAtURL_withItemAtURL_backupItemName_options_resultingItemURL_error(
            &*url(&target)?,
            &*url(&stage)?,
            None,
            NSFileManagerItemReplacementOptions::empty(),
            None,
        );
    if let Err(error) = result {
        // Foundation can move the original to a recovery URL; never delete files after a native failure.
        return Err(replacement_error(&error, &stage));
    }
    Ok(Some(fingerprint(text.as_bytes())))
}

fn replacement_error(error: &objc2_foundation::NSError, stage: &std::path::Path) -> std::io::Error {
    use objc2_foundation::{NSString, NSURL};
    let recovery = error
        .userInfo()
        .objectForKey(&NSString::from_str("NSFileOriginalItemLocationKey"))
        .and_then(|value| value.downcast_ref::<NSURL>().and_then(NSURL::path))
        .map(|path| format!("; original recovery location: {path}"))
        .unwrap_or_default();
    std::io::Error::other(format!(
        "{}; staged contents: {}{recovery}",
        error.localizedDescription(),
        stage.display()
    ))
}

/// Splits a combined patch into one chunk per file, each starting at its `diff --git` line.
pub fn split_patch(patch: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = patch
        .match_indices("diff --git ")
        .map(|(i, _)| i)
        .filter(|&i| i == 0 || patch.as_bytes()[i - 1] == b'\n')
        .collect();
    starts.push(patch.len());
    starts.windows(2).map(|w| &patch[w[0]..w[1]]).collect()
}

/// One visible row of the tree.
#[derive(Debug, PartialEq)]
pub struct Row {
    pub depth: usize,
    /// Display name; single-child folder chains are joined (`src/app`).
    pub name: String,
    /// Folder path (`api/src/app`) or file path.
    pub path: String,
    /// Index into the file list for files, `None` for folders.
    pub file: Option<usize>,
}

#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: BTreeMap<String, usize>,
}

/// File indices in tree order (folders first, then files, each alphabetical), so the
/// diff, `}`/`{` and the tree all agree on what "next file" means.
pub fn tree_order(paths: &[String]) -> Vec<usize> {
    let root = build(paths);
    let mut out = Vec::with_capacity(paths.len());
    fn walk(d: &Dir, out: &mut Vec<usize>) {
        d.dirs.values().for_each(|c| walk(c, out));
        out.extend(d.files.values());
    }
    walk(&root, &mut out);
    out
}

/// Visible rows; folders whose path is in `collapsed` hide their contents.
pub fn rows(paths: &[String], collapsed: &HashSet<String>) -> Vec<Row> {
    fn walk(d: &Dir, prefix: &str, depth: usize, collapsed: &HashSet<String>, out: &mut Vec<Row>) {
        for (name, mut child) in &d.dirs {
            let mut name = name.clone();
            let mut path = format!("{prefix}{name}");
            // A repo (depth 0) shows as "repo / dir".
            let sep = if depth == 0 { " / " } else { "/" };
            while child.files.is_empty() && child.dirs.len() == 1 {
                let (n, c) = child.dirs.iter().next().unwrap();
                name = format!("{name}{sep}{n}");
                path = format!("{path}/{n}");
                child = c;
            }
            // A folder holding one file is that file's row ("app/sidebar.rs"); repos keep
            // their own row so each stays visible.
            if let (true, true, Some((f, &ix))) =
                (depth > 0, child.dirs.is_empty(), child.files.iter().next())
                && child.files.len() == 1
            {
                out.push(Row {
                    depth,
                    name: format!("{name}/{f}"),
                    path: format!("{path}/{f}"),
                    file: Some(ix),
                });
                continue;
            }
            let open = !collapsed.contains(&path);
            out.push(Row {
                depth,
                name,
                path: path.clone(),
                file: None,
            });
            if open {
                walk(child, &format!("{path}/"), depth + 1, collapsed, out);
            }
        }
        for (name, &ix) in &d.files {
            out.push(Row {
                depth,
                name: name.clone(),
                path: format!("{prefix}{name}"),
                file: Some(ix),
            });
        }
    }
    let mut out = Vec::new();
    walk(&build(paths), "", 0, collapsed, &mut out);
    out
}

fn build(paths: &[String]) -> Dir {
    let mut root = Dir::default();
    for (ix, p) in paths.iter().enumerate() {
        let mut parts: Vec<&str> = p.split('/').collect();
        let file = parts.pop().unwrap_or_default();
        let dir = parts.into_iter().fold(&mut root, |d, part| {
            d.dirs.entry(part.to_string()).or_default()
        });
        dir.files.insert(file.to_string(), ix);
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    fn show(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.name))
            .collect()
    }

    #[test]
    fn tree_compacts_folders_and_orders_folders_first() {
        let p = paths(&[
            "api/README.md",
            "api/src/deep/only/x.rs",
            "api/src/a.rs",
            "web/index.ts",
            "shark/src/app/sidebar.rs",
            "shark/src/app.rs",
            "shark/src/chat.rs",
        ]);
        let r = rows(&p, &HashSet::new());
        assert_eq!(
            show(&r),
            [
                "api",
                "  src",
                "    deep/only/x.rs",
                "    a.rs",
                "  README.md",
                "shark / src",
                "  app/sidebar.rs",
                "  app.rs",
                "  chat.rs",
                "web",
                "  index.ts"
            ]
        );
        assert_eq!(
            (r[2].path.as_str(), r[2].file),
            ("api/src/deep/only/x.rs", Some(1))
        );
        assert_eq!(r[5].path, "shark/src");
        assert_eq!(tree_order(&p), [1, 2, 0, 4, 5, 6, 3]);

        let collapsed = HashSet::from(["api/src".to_string()]);
        assert_eq!(
            show(&rows(&p[..4], &collapsed)),
            ["api", "  src", "  README.md", "web", "  index.ts"]
        );
    }

    #[test]
    fn split_and_fingerprint() {
        let patch = "diff --git a/x b/x\n+a\n diff --git inline\ndiff --git a/y b/y\n+b\n";
        let chunks = split_patch(patch);
        assert_eq!(
            chunks,
            [
                "diff --git a/x b/x\n+a\n diff --git inline\n",
                "diff --git a/y b/y\n+b\n"
            ]
        );
        assert!(split_patch("").is_empty());
        assert_eq!(fingerprint("abc"), fingerprint("abc"));
        assert_ne!(fingerprint("abc"), fingerprint("abd"));
        assert_eq!(fingerprint(""), 0xcbf29ce484222325);
    }

    /// 20k files: building rows must stay well inside a frame budget.
    #[test]
    fn rows_scale() {
        let p: Vec<String> = (0..20_000)
            .map(|i| format!("repo{}/src/m{}/f{i}.rs", i % 3, i % 200))
            .collect();
        let t = std::time::Instant::now();
        let r = rows(&p, &HashSet::new());
        let ms = t.elapsed().as_millis();
        println!("rows(20k files): {ms} ms");
        assert_eq!(r.iter().filter(|r| r.file.is_some()).count(), 20_000);
        assert!(ms < 150, "{ms} ms");
    }

    #[test]
    fn confirmed_save_restores_deleted_file_without_replacing_new_entries() {
        use std::{
            fs,
            os::unix::fs::{MetadataExt, symlink},
        };
        let dir = std::env::temp_dir().join(format!("shark-restore-save-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("file");
        fs::write(&file, "old").unwrap();
        let fp = fingerprint(b"old");
        fs::remove_file(&file).unwrap();
        assert_eq!(write_checked(&file, fp, "ours", false).unwrap(), None);
        assert!(!file.exists());
        assert_eq!(
            write_checked(&file, fp, "ours", true).unwrap(),
            Some(fingerprint(b"ours"))
        );
        assert_eq!(fs::read(&file).unwrap(), b"ours");
        assert_eq!(fs::metadata(&file).unwrap().nlink(), 1);
        fs::remove_file(&file).unwrap();

        let other = dir.join("other");
        fs::write(&other, "theirs").unwrap();
        for linked in [false, true] {
            assert_eq!(
                write_staged(&file, fp, "ours", true, || {
                    if linked {
                        symlink(&other, &file).unwrap();
                    } else {
                        fs::write(&file, "theirs").unwrap();
                    }
                })
                .unwrap(),
                None
            );
            assert_eq!(fs::read(&file).unwrap(), b"theirs");
            assert_eq!(
                fs::symlink_metadata(&file)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                linked
            );
            fs::remove_file(&file).unwrap();
        }
        let missing = dir.join("missing");
        symlink(&missing, &file).unwrap();
        assert!(write_checked(&file, fp, "ours", true).is_err());
        assert!(
            fs::symlink_metadata(&file)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!missing.exists());
        assert!(!fs::read_dir(&dir).unwrap().any(|p| {
            p.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".shark-save-")
        }));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_recovery_error_shows_the_file_location() {
        use objc2_foundation::{NSDictionary, NSError, NSString, NSURL};
        let key = NSString::from_str("NSFileOriginalItemLocationKey");
        let recovery = NSURL::fileURLWithPath(&NSString::from_str("/tmp/shark-original-recovery"));
        let info = NSDictionary::<NSString, objc2::runtime::AnyObject>::from_slices(
            &[&*key],
            &[&*recovery],
        );
        let error = unsafe {
            NSError::errorWithDomain_code_userInfo(
                &NSString::from_str("NSCocoaErrorDomain"),
                512,
                Some(&info),
            )
        };
        let message =
            super::replacement_error(&error, std::path::Path::new("/tmp/stage")).to_string();
        assert!(message.contains("original recovery location: /tmp/shark-original-recovery"));
        assert!(message.contains("staged contents: /tmp/stage"));
    }

    #[test]
    fn replacement_rechecks_identity_and_preserves_failed_native_stage() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let dir = std::env::temp_dir().join(format!("shark-save-race-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("file");
        fs::write(&file, "old").unwrap();
        let fp = fingerprint(b"old");
        for force in [false, true] {
            fs::write(&file, "old").unwrap();
            assert_eq!(
                write_staged(&file, fp, "ours", force, || {
                    fs::write(&file, "theirs").unwrap();
                })
                .unwrap(),
                None
            );
            assert_eq!(fs::read(&file).unwrap(), b"theirs");
        }
        fs::write(&file, "old").unwrap();
        assert_eq!(
            write_staged(&file, fp, "ours", false, || {
                fs::rename(&file, dir.join("previous")).unwrap();
                fs::write(&file, "old").unwrap();
            })
            .unwrap(),
            None,
            "identical bytes on a new inode still conflict"
        );
        let failed = write_staged(&file, fp, "ours", false, || {
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
        });
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(failed.unwrap_err().to_string().contains("staged contents:"));
        assert_eq!(fs::read(&file).unwrap(), b"old");
        assert!(fs::read_dir(&dir).unwrap().any(|p| {
            p.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".shark-save-")
        }));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replacement_preserves_links_mode_metadata_and_conflicts() {
        use std::{
            fs,
            os::unix::fs::{PermissionsExt, symlink},
        };
        let dir = std::env::temp_dir().join(format!("shark-safe-save-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("script");
        let link = dir.join("link");
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o751)).unwrap();
        symlink(&file, &link).unwrap();
        let attribute = "com.apple.metadata:kMDItemFinderComment";
        assert!(
            std::process::Command::new("xattr")
                .args(["-w", attribute, "shark metadata"])
                .arg(&file)
                .status()
                .unwrap()
                .success()
        );
        let fp = fingerprint(b"old");
        assert_eq!(write_checked(&link, fp + 1, "new", false).unwrap(), None);
        assert_eq!(fs::read(&file).unwrap(), b"old");
        assert!(write_checked(&link, fp + 1, "new", true).unwrap().is_some());
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&file).unwrap(), b"new");
        let attr = std::process::Command::new("xattr")
            .args(["-p", attribute])
            .arg(&file)
            .output()
            .unwrap();
        assert!(attr.status.success());
        assert_eq!(
            String::from_utf8(attr.stdout).unwrap().trim(),
            "shark metadata"
        );
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o751
        );
        let hard = dir.join("hard");
        fs::hard_link(&file, &hard).unwrap();
        assert!(
            write_checked(&file, fingerprint(b"new"), "bad", true)
                .unwrap_err()
                .to_string()
                .contains("external editor")
        );
        assert_eq!(fs::read(&hard).unwrap(), b"new");
        fs::remove_file(&hard).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
        let failed = write_checked(&file, fingerprint(b"new"), "bad", false);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(failed.is_err());
        assert_eq!(fs::read(&file).unwrap(), b"new");
        fs::remove_dir_all(&dir).unwrap();
    }
}

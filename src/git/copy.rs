//! Seeding a new worktree with the source's gitignored files (`.env` and the like).
use super::{git, ls_files};
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

/// Copies the gitignored files worth keeping into the new worktree; returns the ones created.
pub(super) fn copy_included(src: &Path, wt: &Path) -> Result<Vec<String>, String> {
    let mut copied_files = Vec::new();
    for f in included_files(src)? {
        if copy_nofollow(src, wt, &f)? {
            copied_files.push(f);
        }
    }
    Ok(copied_files)
}

/// The gitignored files to copy into a new worktree. Patterns come from `.worktreeinclude`
/// (gitignore syntax), defaulting to `.env*`.
fn included_files(src: &Path) -> Result<Vec<String>, String> {
    let mut files = Vec::new();
    let custom = src.join(".worktreeinclude").exists();
    let exclude = match custom {
        true => "--exclude-from=.worktreeinclude",
        false => "--exclude=.env*",
    };
    let ignored = ls_files(src, &["-o", "-i", "--exclude-standard"])?;
    // The default `.env*` would otherwise take a whole ignored `.env/` venv or
    // `node_modules/x/.env.example`: only files outside ignored directories qualify.
    let mut dir_ignored = std::collections::HashMap::<PathBuf, bool>::new();
    for f in ls_files(src, &["-o", "-i", exclude])?
        .iter()
        .filter(|f| ignored.contains(f))
    {
        let mut parent = PathBuf::new();
        let under_ignored = !custom
            && (Path::new(f).parent().into_iter())
                .flat_map(Path::components)
                .any(|c| {
                    parent.push(c);
                    *dir_ignored.entry(parent.clone()).or_insert_with(|| {
                        let p = parent.to_string_lossy();
                        git(src, &["check-ignore", "-q", "--", &p]).is_ok()
                    })
                });
        if !under_ignored {
            files.push(f.clone());
        }
    }
    Ok(files)
}

/// Copies `f` from `src` into `wt` without following symlinks anywhere on the way; false when
/// the destination exists, isn't ignored, is tracked, or `f` isn't a regular file.
fn copy_nofollow(src: &Path, wt: &Path, f: &str) -> Result<bool, String> {
    use std::{
        ffi::CString,
        os::fd::{AsRawFd, FromRawFd},
        os::unix::ffi::OsStrExt,
    };
    unsafe extern "C" {
        fn openat(fd: i32, path: *const std::ffi::c_char, flags: i32, ...) -> i32;
        fn mkdirat(fd: i32, path: *const std::ffi::c_char, mode: u16) -> i32;
        fn unlinkat(fd: i32, path: *const std::ffi::c_char, flags: i32) -> i32;
    }
    // Darwin flags, from sys/fcntl.h. Directory descriptors pin parents across renames.
    const DIRECTORY_NOFOLLOW: i32 = 0x00100000 | 0x00000100;
    let to = wt.join(f);
    if fs::symlink_metadata(&to).is_ok() {
        return Ok(false);
    }
    let mut parent = fs::OpenOptions::new()
        .read(true)
        .custom_flags(DIRECTORY_NOFOLLOW)
        .open(wt)
        .map_err(|e| format!("copy {f}: {e}"))?;
    for part in Path::new(f).parent().into_iter().flat_map(Path::components) {
        if !matches!(part, std::path::Component::Normal(_)) {
            return Err(format!("copy {f}: invalid parent"));
        }
        let name = CString::new(part.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        // SAFETY: valid directory fd and NUL-terminated single component, no borrowed fd is closed.
        let fd = unsafe {
            mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700);
            openat(parent.as_raw_fd(), name.as_ptr(), DIRECTORY_NOFOLLOW)
        };
        if fd < 0 {
            return Err(format!(
                "copy {f}: unsafe or inaccessible parent: {}",
                io::Error::last_os_error()
            ));
        }
        parent = unsafe { fs::File::from_raw_fd(fd) };
    }
    if git(wt, &["check-ignore", "-q", "--", f]).is_err()
        || !git(wt, &["ls-files", "--", f])?.is_empty()
    {
        return Ok(false);
    }
    let leaf = CString::new(
        Path::new(f)
            .file_name()
            .ok_or("Missing file name")?
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    let mut source = fs::File::open(src.join(f)).map_err(|e| format!("copy {f}: {e}"))?;
    let metadata = source.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Ok(false);
    }
    // O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, initially private.
    let fd = unsafe {
        openat(
            parent.as_raw_fd(),
            leaf.as_ptr(),
            0x0001 | 0x0200 | 0x0800 | 0x0100,
            0o600u32,
        )
    };
    if fd < 0 {
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::AlreadyExists {
            return Ok(false);
        }
        return Err(format!("copy {f}: {e}"));
    }
    let mut dest = unsafe { fs::File::from_raw_fd(fd) };
    let identity = dest.metadata().map_err(|e| e.to_string())?;
    let copied = io::copy(&mut source, &mut dest)
        .and_then(|_| dest.flush())
        .and_then(|_| dest.set_permissions(metadata.permissions()));
    if let Err(e) = copied {
        let fd = unsafe { openat(parent.as_raw_fd(), leaf.as_ptr(), 0x0100) };
        if fd >= 0 {
            let current = unsafe { fs::File::from_raw_fd(fd) };
            if current
                .metadata()
                .is_ok_and(|m| m.dev() == identity.dev() && m.ino() == identity.ino())
            {
                unsafe {
                    unlinkat(parent.as_raw_fd(), leaf.as_ptr(), 0);
                }
            }
        }
        return Err(format!("copy {f}: {e}"));
    }
    Ok(true)
}

pub(super) fn untracked_fingerprint(wt: &Path, copied: &[String]) -> Result<u64, String> {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    let paths = ls_files(wt, &["--others"])?;
    if paths.len() != copied.len() || paths.iter().any(|p| !copied.iter().any(|c| c == p)) {
        return Err("The worktree contains files this operation did not create".into());
    }
    for path in paths {
        path.hash(&mut hash);
        let path = wt.join(path);
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        (metadata.dev(), metadata.ino(), metadata.mode()).hash(&mut hash);
        if metadata.file_type().is_symlink() {
            fs::read_link(path)
                .map_err(|e| e.to_string())?
                .hash(&mut hash);
        } else if metadata.is_file() {
            fs::read(path).map_err(|e| e.to_string())?.hash(&mut hash);
        } else {
            return Err(format!("Unexpected untracked file: {}", path.display()));
        }
    }
    Ok(hash.finish())
}

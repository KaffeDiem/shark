//! File-level Git status and discarding, independent of the UI and diff comparison base.
use super::{git, run};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path},
};

/// Repo-relative paths, with whether each file is untracked. Renames are separate paths,
/// like the diff, so discarding one row never changes a second file.
pub fn uncommitted_files(wt: &Path) -> Result<BTreeMap<String, bool>, String> {
    let out = run(
        wt,
        "git",
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
            "--ignore-submodules=none",
        ],
    )?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().into());
    }
    let text = std::str::from_utf8(&out.stdout).map_err(|e| e.to_string())?;
    text.split('\0')
        .filter(|s| !s.is_empty())
        .map(|record| {
            let path = record
                .get(3..)
                .filter(|p| !p.is_empty())
                .ok_or_else(|| "Invalid git status path".to_string())?;
            Ok((path.to_string(), record.starts_with("?? ")))
        })
        .collect()
}

/// Discard only this file, including staged changes. Untracked files are removed;
/// tracked files restore HEAD, never the workspace's diff base.
pub fn discard_file(wt: &Path, path: &str) -> Result<(), String> {
    let rel = Path::new(path);
    if path.is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err("Discard requires a repo-relative file path".into());
    }
    // Never follow a replaced parent directory outside the worktree.
    let mut parent = wt.to_path_buf();
    let parts: Vec<_> = rel.components().collect();
    for part in &parts[..parts.len() - 1] {
        parent.push(part.as_os_str());
        if fs::symlink_metadata(&parent).is_ok_and(|m| m.is_symlink()) {
            return Err("Can't discard through a symlinked directory".into());
        }
    }
    let untracked = uncommitted_files(wt)?
        .get(path)
        .copied()
        .ok_or_else(|| "This file has no uncommitted changes".to_string())?;
    let abs = wt.join(rel);
    if fs::symlink_metadata(&abs).is_ok_and(|m| m.is_dir()) {
        return Err("Discard applies to files, not directories or submodules".into());
    }
    let indexed = git(
        wt,
        &["--literal-pathspecs", "ls-files", "--stage", "--", path],
    )?;
    if indexed.lines().any(|l| l.starts_with("160000 ")) {
        return Err("Discard submodule changes inside the submodule".into());
    }
    if untracked {
        fs::remove_file(abs).map_err(|e| e.to_string())
    } else {
        let original = git(wt, &["--literal-pathspecs", "ls-tree", "HEAD", "--", path])?;
        if original.starts_with("160000 ") {
            return Err("Discard submodule changes inside the submodule".into());
        }
        git(
            wt,
            &[
                "--literal-pathspecs",
                "restore",
                "--source=HEAD",
                "--staged",
                "--worktree",
                "--",
                path,
            ],
        )
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discard_restores_head_and_is_limited_to_the_selected_file() {
        let root = std::env::temp_dir().join(format!("shark-discard-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]).unwrap();
        let commit = || {
            git(
                &root,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "-qm",
                    "files",
                ],
            )
            .unwrap()
        };
        for path in [
            "file.txt",
            "deleted.txt",
            "other.txt",
            "[x].txt",
            "x.txt",
            "white space\nfile",
        ] {
            fs::write(root.join(path), "base\n").unwrap();
        }
        git(&root, &["add", "."]).unwrap();
        commit();
        fs::write(root.join("file.txt"), "committed feature\n").unwrap();
        git(&root, &["add", "."]).unwrap();
        commit();
        for path in [
            "file.txt",
            "other.txt",
            "[x].txt",
            "x.txt",
            "white space\nfile",
        ] {
            fs::write(root.join(path), "staged\n").unwrap();
        }
        fs::remove_file(root.join("deleted.txt")).unwrap();
        fs::write(root.join("added.txt"), "new\n").unwrap();
        git(&root, &["add", "."]).unwrap();
        fs::write(root.join("file.txt"), "unstaged\n").unwrap();
        fs::create_dir(root.join("new-dir")).unwrap();
        fs::write(root.join("new-dir/new.txt"), "untracked\n").unwrap();
        let changes = uncommitted_files(&root).unwrap();
        assert_eq!(changes.get("white space\nfile"), Some(&false));
        assert_eq!(changes.get("new-dir/new.txt"), Some(&true));
        discard_file(&root, "file.txt").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("file.txt")).unwrap(),
            "committed feature\n"
        );
        assert_eq!(
            git(&root, &["show", ":file.txt"]).unwrap(),
            "committed feature"
        );
        discard_file(&root, "deleted.txt").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("deleted.txt")).unwrap(),
            "base\n"
        );
        discard_file(&root, "added.txt").unwrap();
        discard_file(&root, "new-dir/new.txt").unwrap();
        assert!(!root.join("added.txt").exists() && !root.join("new-dir/new.txt").exists());
        discard_file(&root, "[x].txt").unwrap();
        assert_eq!(fs::read_to_string(root.join("[x].txt")).unwrap(), "base\n");
        assert_eq!(fs::read_to_string(root.join("x.txt")).unwrap(), "staged\n");
        assert_eq!(
            fs::read_to_string(root.join("other.txt")).unwrap(),
            "staged\n"
        );
        assert!(discard_file(&root, "../other.txt").is_err());
        assert!(discard_file(&root, "new-dir").is_err());
        assert!(
            discard_file(&root, "file.txt").is_err(),
            "clean files are refused"
        );
        // A staged rename has independent old/new rows; selecting the new path leaves the old deletion.
        git(&root, &["mv", "deleted.txt", "renamed.txt"]).unwrap();
        discard_file(&root, "renamed.txt").unwrap();
        assert!(!root.join("renamed.txt").exists() && !root.join("deleted.txt").exists());
        discard_file(&root, "deleted.txt").unwrap();
        assert!(root.join("deleted.txt").exists());
        // Removing a symlink never removes its target.
        std::os::unix::fs::symlink("other.txt", root.join("link")).unwrap();
        discard_file(&root, "link").unwrap();
        assert!(root.join("other.txt").exists());
        std::os::unix::fs::symlink(&root, root.join("alias")).unwrap();
        assert!(discard_file(&root, "alias/other.txt").is_err());
        let head = git(&root, &["rev-parse", "HEAD"]).unwrap();
        git(
            &root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{head},sub"),
            ],
        )
        .unwrap();
        assert!(
            discard_file(&root, "sub")
                .unwrap_err()
                .contains("submodule")
        );
        fs::remove_dir_all(root).unwrap();
    }
}

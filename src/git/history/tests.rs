use super::*;
use std::fs;
use std::path::PathBuf;

fn g(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@e",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("shark-history-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    g(&dir, &["init", "-q", "-b", "main"]);
    dir
}

fn commit_all(dir: &Path, msg: &str) -> String {
    g(dir, &["add", "-A"]);
    g(dir, &["commit", "-q", "--allow-empty", "-m", msg]);
    g(dir, &["rev-parse", "HEAD"])
}

#[test]
fn root_ordinary_empty_merge_and_rename_commits() {
    let dir = repo("shapes");
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    let root = commit_all(&dir, "root");
    let r = commit(&dir, &root).unwrap();
    assert_eq!(r.compared_to, None);
    assert_eq!(r.files.len(), 1);
    assert_eq!((r.files[0].status, r.files[0].old.is_none()), ('A', true));
    assert!(patch(&dir, &r, "a.txt").unwrap().contains("+one"));

    g(&dir, &["tag", "base"]);
    g(&dir, &["mv", "a.txt", "b c.txt"]);
    fs::write(dir.join("ünï\tcode.md"), "# hi\n").unwrap();
    let renamed = commit_all(&dir, "rename\n\nbody line");
    let c = commit(&dir, &renamed).unwrap();
    assert_eq!(c.message, "rename\n\nbody line");
    let paths: Vec<_> = c
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.status))
        .collect();
    assert!(paths.contains(&("a.txt", 'D')), "{paths:?}");
    assert!(paths.contains(&("b c.txt", 'A')));
    assert!(paths.contains(&("ünï\tcode.md", 'A')));
    let md = c.files.iter().find(|f| f.path.starts_with("ün")).unwrap();
    assert_eq!(blob(&dir, &md.new.as_ref().unwrap().id).unwrap(), b"# hi\n");
    assert!(patch(&dir, &c, "ünï\tcode.md").unwrap().contains("+# hi"));

    let empty = commit_all(&dir, "empty");
    assert!(commit(&dir, &empty).unwrap().files.is_empty());

    g(&dir, &["checkout", "-q", "-b", "side", "base"]);
    fs::write(dir.join("side.txt"), "side\n").unwrap();
    let side = commit_all(&dir, "side work");
    g(&dir, &["checkout", "-q", "main"]);
    g(
        &dir,
        &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
    );
    let merge = g(&dir, &["rev-parse", "HEAD"]);
    let m = commit(&dir, &merge).unwrap();
    assert!(m.merge());
    assert_eq!(m.compared_to.as_deref(), Some(empty.as_str()));
    assert_eq!(
        m.files.len(),
        1,
        "first-parent comparison shows the side's files"
    );

    let range = resolve(&dir, "base").unwrap();
    let (list, more) = commits(&dir, &range, 0).unwrap();
    assert!(!more);
    let shas: Vec<_> = list.iter().map(|s| s.sha.as_str()).collect();
    assert_eq!(shas[0], merge, "newest first");
    assert!(
        shas.contains(&side.as_str()),
        "merged side commits are included"
    );
    assert!(shas.contains(&renamed.as_str()) && !shas.contains(&root.as_str()));
    assert_eq!(
        list.iter().find(|s| s.sha == merge).unwrap().parents.len(),
        2
    );
}

#[test]
fn mode_symlink_and_binary_blobs_are_exact() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = repo("modes");
    fs::write(dir.join("run.sh"), "echo\n").unwrap();
    let bytes: Vec<u8> = (0..=255u8).cycle().take(4000).collect();
    fs::write(dir.join("img.png"), &bytes).unwrap();
    commit_all(&dir, "one");
    fs::set_permissions(dir.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("/etc/passwd", dir.join("link")).unwrap();
    let two = commit_all(&dir, "two");
    let c = commit(&dir, &two).unwrap();
    let run = c.files.iter().find(|f| f.path == "run.sh").unwrap();
    assert!(run.mode_only());
    let link = c.files.iter().find(|f| f.path == "link").unwrap();
    let link_blob = link.new.as_ref().unwrap();
    assert!(link_blob.symlink());
    assert_eq!(
        blob(&dir, &link_blob.id).unwrap(),
        b"/etc/passwd",
        "the target, never followed"
    );

    // The working file changes after committing; history still reads the commit.
    fs::write(dir.join("img.png"), b"changed").unwrap();
    let at = blob_at(&dir, &two, "img.png").unwrap().unwrap();
    assert_eq!(blob(&dir, &at.id).unwrap(), bytes);
    assert_eq!(blob_size(&dir, &at.id).unwrap(), 4000);
    assert_eq!(blob_at(&dir, &two, "missing").unwrap(), None);
    assert_eq!(fs::read(dir.join("img.png")).unwrap(), b"changed");
    assert!(g(&dir, &["status", "--porcelain"]).contains("img.png"));
}

#[test]
fn pagination_unpushed_and_missing_base() {
    let dir = repo("pages");
    commit_all(&dir, "base");
    g(&dir, &["tag", "base"]);
    for i in 0..(PAGE + 5) {
        commit_all(&dir, &format!("c{i}"));
    }
    let range = resolve(&dir, "base").unwrap();
    let (first, more) = commits(&dir, &range, 0).unwrap();
    assert_eq!((first.len(), more), (PAGE, true));
    assert_eq!(first[0].subject, format!("c{}", PAGE + 4));
    let (second, more) = commits(&dir, &range, PAGE).unwrap();
    assert_eq!((second.len(), more), (5, false));
    assert_eq!(second.last().unwrap().subject, "c0");

    // A new local commit is included after the next resolve; the old range stays fixed.
    commit_all(&dir, "unpushed");
    assert_eq!(
        commits(&dir, &range, 0).unwrap().0[0].subject,
        first[0].subject
    );
    let fresh = resolve(&dir, "base").unwrap();
    assert_eq!(commits(&dir, &fresh, 0).unwrap().0[0].subject, "unpushed");

    let err = resolve(&dir, "origin/missing").unwrap_err();
    assert!(err.contains("origin/missing"), "{err}");
    assert!(resolve(&dir, "").is_err());
    assert!(commit(&dir, "--all").is_err());
}

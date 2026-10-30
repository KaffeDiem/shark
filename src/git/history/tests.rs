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
    fs::create_dir(dir.join("sub")).unwrap();
    fs::write(dir.join("sub/n.txt"), "n\n").unwrap();
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
    assert!(patch(&dir, &c, "sub/n.txt").unwrap().contains("+n"));
    assert!(paths.contains(&("sub/n.txt", 'A')));

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

    let list = commits(&dir, "base").unwrap();
    let shas: Vec<_> = list.iter().map(|s| s.sha.as_str()).collect();
    assert_eq!(shas[0], merge, "newest first");
    assert!(
        shas.contains(&side.as_str()),
        "merged side commits are included"
    );
    assert!(shas.contains(&renamed.as_str()) && !shas.contains(&root.as_str()));
    assert!(list.iter().find(|s| s.sha == merge).unwrap().merge);
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
    assert_eq!(run.mode_only(), Some(("100644", "100755")));
    let link = c.files.iter().find(|f| f.path == "link").unwrap();
    let link_blob = link.new.as_ref().unwrap();
    assert!(link_blob.symlink());
    assert_eq!(
        blob(&dir, &link_blob.id).unwrap(),
        b"/etc/passwd",
        "the target, never followed"
    );

    let at = blob_at(&dir, &two, "img.png").unwrap().unwrap();
    assert_eq!(blob(&dir, &at.id).unwrap(), bytes);
    assert_eq!(blob_size(&dir, &at.id).unwrap(), 4000);
    assert_eq!(blob_at(&dir, &two, "missing").unwrap(), None);
}

#[test]
fn unpushed_commits_and_missing_base() {
    let dir = repo("unpushed");
    commit_all(&dir, "base");
    g(&dir, &["tag", "base"]);
    assert!(commits(&dir, "base").unwrap().is_empty());
    commit_all(&dir, "unpushed");
    assert_eq!(commits(&dir, "base").unwrap()[0].subject, "unpushed");

    let err = commits(&dir, "origin/missing").unwrap_err();
    assert!(err.contains("origin/missing"), "{err}");
    assert!(commits(&dir, "").is_err());
    assert!(commit(&dir, "--all").is_err());
}

#[test]
fn parse_raw_records() {
    type Row = (
        &'static str,
        char,
        Option<&'static str>,
        Option<&'static str>,
    );
    let (f, d) = (Some("100644"), None);
    let cases: &[(&[u8], &[Row])] = &[
        (
            b":000000 160000 0 a A\0sub\0:040000 000000 b 0 D\0dir\0",
            &[
                ("sub", 'A', d, Some("160000")),
                ("dir", 'D', Some("040000"), d),
            ],
        ),
        (
            b":100644 120000 a b T\0l\0",
            &[("l", 'T', f, Some("120000"))],
        ),
        // --no-renames: a rename is a delete and an add.
        (
            b":100644 000000 a 0 D\0x\0:000000 100644 0 b A\0y\0",
            &[("x", 'D', f, d), ("y", 'A', d, f)],
        ),
        // Junk, a short record and its path, an empty status, then a record cut off before its path.
        (
            b"junk\0:100644 100644 a M\0skipped\0:100644 100644 a b \0p\0:100644 100644 a b M",
            &[("p", 'M', f, f)],
        ),
    ];
    for (raw, want) in cases {
        let got: Vec<_> = parse_raw(raw)
            .iter()
            .map(|c| {
                let mode = |b: &Option<Blob>| b.as_ref().map(|b| b.mode.clone());
                (c.path.clone(), c.status, mode(&c.old), mode(&c.new))
            })
            .collect();
        let want: Vec<_> = want
            .iter()
            .map(|(p, s, o, n)| (p.to_string(), *s, o.map(String::from), n.map(String::from)))
            .collect();
        assert_eq!(got, want, "{}", String::from_utf8_lossy(raw));
    }
}

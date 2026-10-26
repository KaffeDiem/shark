//! Changed-files tree and "viewed" fingerprints for the diff.
use std::collections::{BTreeMap, HashSet};

/// FNV-1a: stable across Rust releases, unlike `DefaultHasher`, so stored fingerprints stay valid.
pub fn fingerprint(bytes: impl AsRef<[u8]>) -> u64 {
    bytes.as_ref().iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x100000001b3)
    })
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
}

//! Read-only branch history addressed by immutable SHAs. Nothing here reads, writes or checks
//! out the working tree, the index or HEAD.
use super::git_bytes;
use std::path::Path;

// ponytail: one unpaged `git log`; branches beyond this show the newest N, page it if that bites.
pub const MAX_COMMITS: usize = 1000;
const SECS: u64 = 30;

#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub sha: String,
    pub author: String,
    /// Author date, ISO 8601.
    pub date: String,
    pub merge: bool,
    pub subject: String,
}

/// One side of a changed file, as stored in Git.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blob {
    pub id: String,
    /// Octal Git mode: `100644`, `100755`, `120000` (symlink) or `160000` (submodule).
    pub mode: String,
}

impl Blob {
    pub fn symlink(&self) -> bool {
        self.mode == "120000"
    }

    pub fn submodule(&self) -> bool {
        self.mode == "160000"
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileChange {
    pub path: String,
    /// `A`, `D`, `M` or `T` (type change); renames are a delete plus an add.
    pub status: char,
    pub old: Option<Blob>,
    pub new: Option<Blob>,
}

impl FileChange {
    /// The old and new modes when both sides exist and differ.
    pub fn mode_change(&self) -> Option<(&str, &str)> {
        match (&self.old, &self.new) {
            (Some(o), Some(n)) if o.mode != n.mode => Some((&o.mode, &n.mode)),
            _ => None,
        }
    }

    /// `mode_change` when the contents are identical.
    pub fn mode_only(&self) -> Option<(&str, &str)> {
        let same = matches!((&self.old, &self.new), (Some(o), Some(n)) if o.id == n.id);
        self.mode_change().filter(|_| same)
    }

    /// `submodule` or `symlink` when either side is one: neither has file contents to show.
    pub fn special(&self) -> Option<&'static str> {
        let sides = || [&self.old, &self.new].into_iter().flatten();
        if sides().any(Blob::submodule) {
            Some("submodule")
        } else {
            sides().any(Blob::symlink).then_some("symlink")
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub date: String,
    pub message: String,
    /// What the files are compared with: the first parent, or `None` (the empty tree) for a
    /// root commit.
    pub compared_to: Option<String>,
    pub files: Vec<FileChange>,
}

impl Commit {
    pub fn merge(&self) -> bool {
        self.parents.len() > 1
    }
}

/// `git_bytes`, not `git`: a partial clone may fetch missing objects here, so the timeout and
/// the no-prompt environment matter.
fn text(wt: &Path, args: &[&str]) -> Result<String, String> {
    git_bytes(wt, args, SECS).map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn commit_sha(wt: &Path, rev: &str) -> Option<String> {
    let rev = format!("{rev}^{{commit}}");
    text(
        wt,
        &["rev-parse", "--verify", "--quiet", "--end-of-options", &rev],
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

/// `base..HEAD`, newest first in topological order, merged side branches included, at most
/// `MAX_COMMITS`. A missing base is an error, never all history.
pub fn commits(wt: &Path, base: &str) -> Result<Vec<Summary>, String> {
    if base.trim().is_empty() {
        return Err("This repository has no comparison base".into());
    }
    let base_sha = commit_sha(wt, base).ok_or_else(|| {
        format!("Comparison base {base} is not available locally. Fetch it, then refresh.")
    })?;
    let head = commit_sha(wt, "HEAD").ok_or("HEAD has no commits yet")?;
    let spec = format!("{base_sha}..{head}");
    let max = format!("--max-count={MAX_COMMITS}");
    let out = text(
        wt,
        &[
            "log",
            "--topo-order",
            "--no-color",
            "--no-show-signature",
            "--format=%H%x1f%an%x1f%aI%x1f%P%x1f%s%x1e",
            &max,
            &spec,
            "--",
        ],
    )?;
    Ok(out
        .split('\x1e')
        .filter_map(|record| {
            let f: Vec<_> = record.trim_start_matches('\n').split('\x1f').collect();
            (f.len() == 5).then(|| Summary {
                sha: f[0].into(),
                author: f[1].into(),
                date: f[2].into(),
                merge: f[3].split_whitespace().count() > 1,
                subject: f[4].into(),
            })
        })
        .collect())
}

fn sha_like(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A commit's full message and its changes against its first parent (or the empty tree).
pub fn commit(wt: &Path, sha: &str) -> Result<Commit, String> {
    if !sha_like(sha) {
        return Err("Invalid commit".into());
    }
    let out = text(
        wt,
        &[
            "show",
            "-s",
            "--no-color",
            "--no-show-signature",
            "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%aI%x1f%B",
            sha,
            "--",
        ],
    )?;
    let f: Vec<_> = out.splitn(6, '\x1f').collect();
    if f.len() != 6 {
        return Err("Unexpected git show output".into());
    }
    let parents: Vec<String> = f[1].split_whitespace().map(Into::into).collect();
    let compared_to = parents.first().cloned();
    let raw = diff_tree(
        wt,
        compared_to.as_deref(),
        sha,
        &["-r", "-z", "--raw", "--no-abbrev"],
        &[],
    )?;
    Ok(Commit {
        sha: f[0].trim().into(),
        parents,
        author: f[2].into(),
        email: f[3].into(),
        date: f[4].into(),
        message: f[5].trim_end().into(),
        compared_to,
        files: parse_raw(&raw),
    })
}

/// `diff-tree` of a commit against `parent`, or the empty tree for a root commit.
fn diff_tree(
    wt: &Path,
    parent: Option<&str>,
    sha: &str,
    flags: &[&str],
    paths: &[&str],
) -> Result<Vec<u8>, String> {
    let mut args = vec![
        "--literal-pathspecs",
        "diff-tree",
        "--no-commit-id",
        "--no-renames",
        "--no-ext-diff",
    ];
    args.extend(flags);
    args.push(parent.unwrap_or("--root"));
    args.extend([sha, "--"]);
    args.extend(paths);
    git_bytes(wt, &args, SECS)
}

/// `diff-tree -z --raw` records: `:old new oldsha newsha S\0path\0`.
fn parse_raw(raw: &[u8]) -> Vec<FileChange> {
    let mut parts = raw.split(|b| *b == 0);
    let mut files = vec![];
    while let Some(head) = parts.next() {
        let head = String::from_utf8_lossy(head);
        let Some(meta) = head.strip_prefix(':') else {
            continue;
        };
        let Some(path) = parts.next() else { break };
        let f: Vec<_> = meta.split(' ').collect();
        if f.len() != 5 {
            continue;
        }
        let side = |mode: &str, id: &str| {
            (mode != "000000").then(|| Blob {
                id: id.into(),
                mode: mode.into(),
            })
        };
        files.push(FileChange {
            path: String::from_utf8_lossy(path).into_owned(),
            status: f[4].chars().next().unwrap_or('M'),
            old: side(f[0], f[2]),
            new: side(f[1], f[3]),
        });
    }
    files
}

/// One file's unified patch for the commit, as `git diff` writes it for Diff.
pub fn patch(wt: &Path, commit: &Commit, path: &str) -> Result<String, String> {
    let flags = ["-p", "--no-color", "--no-textconv", "--full-index"];
    let out = diff_tree(
        wt,
        commit.compared_to.as_deref(),
        &commit.sha,
        &flags,
        &[path],
    )?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// The size of a blob, so huge files can be refused before reading them.
pub fn blob_size(wt: &Path, id: &str) -> Result<u64, String> {
    if !sha_like(id) {
        return Err("Invalid blob".into());
    }
    text(wt, &["cat-file", "-s", id])?
        .trim()
        .parse()
        .map_err(|e| format!("git cat-file -s: {e}"))
}

/// A blob's exact bytes.
pub fn blob(wt: &Path, id: &str) -> Result<Vec<u8>, String> {
    if !sha_like(id) {
        return Err("Invalid blob".into());
    }
    git_bytes(wt, &["cat-file", "blob", id], SECS)
}

/// The blob at `path` in `rev`'s tree, without following symlinks. `None` when absent.
pub fn blob_at(wt: &Path, rev: &str, path: &str) -> Result<Option<Blob>, String> {
    if !sha_like(rev) {
        return Err("Invalid revision".into());
    }
    let out = git_bytes(
        wt,
        &[
            "--literal-pathspecs",
            "ls-tree",
            "-z",
            "--full-tree",
            rev,
            "--",
            path,
        ],
        SECS,
    )?;
    let entry = out.split(|b| *b == 0).next().unwrap_or_default();
    let entry = String::from_utf8_lossy(entry);
    let Some((meta, name)) = entry.split_once('\t') else {
        return Ok(None);
    };
    let f: Vec<_> = meta.split(' ').collect();
    Ok(
        (f.len() == 3 && f[1] == "blob" && name == path).then(|| Blob {
            id: f[2].into(),
            mode: f[0].into(),
        }),
    )
}

#[cfg(test)]
mod tests;

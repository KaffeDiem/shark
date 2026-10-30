//! Read-only branch history addressed by immutable SHAs. Nothing here reads, writes or checks
//! out the working tree, the index or HEAD.
use super::git_bytes;
use std::path::Path;

/// Commits per page.
pub const PAGE: usize = 100;
const SECS: u64 = 30;

/// `base..head`, both resolved to full commit SHAs once per refresh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Range {
    pub base: String,
    pub head: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub sha: String,
    pub short: String,
    pub author: String,
    /// Author date, ISO 8601.
    pub date: String,
    pub parents: Vec<String>,
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
    pub fn mode_only(&self) -> bool {
        matches!((&self.old, &self.new), (Some(o), Some(n)) if o.id == n.id && o.mode != n.mode)
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

/// Resolves the comparison base and local HEAD. A missing base is an error, never all history.
pub fn resolve(wt: &Path, base: &str) -> Result<Range, String> {
    if base.trim().is_empty() {
        return Err("This repository has no comparison base".into());
    }
    let base_sha = commit_sha(wt, base).ok_or_else(|| {
        format!("Comparison base {base} is not available locally. Fetch it, then refresh.")
    })?;
    let head = commit_sha(wt, "HEAD").ok_or("HEAD has no commits yet")?;
    Ok(Range {
        base: base_sha,
        head,
    })
}

/// One page of `base..head`, newest first in topological order, merged side branches included.
/// Returns whether more pages follow.
pub fn commits(wt: &Path, range: &Range, skip: usize) -> Result<(Vec<Summary>, bool), String> {
    let spec = format!("{}..{}", range.base, range.head);
    let (skip_arg, max) = (
        format!("--skip={skip}"),
        format!("--max-count={}", PAGE + 1),
    );
    let out = text(
        wt,
        &[
            "log",
            "--topo-order",
            "--no-color",
            "--no-show-signature",
            "--format=%H%x1f%h%x1f%an%x1f%aI%x1f%P%x1f%s%x1e",
            &skip_arg,
            &max,
            &spec,
            "--",
        ],
    )?;
    let mut list: Vec<Summary> = out
        .split('\x1e')
        .filter_map(|record| {
            let f: Vec<_> = record.trim_start_matches('\n').split('\x1f').collect();
            (f.len() == 6).then(|| Summary {
                sha: f[0].into(),
                short: f[1].into(),
                author: f[2].into(),
                date: f[3].into(),
                parents: f[4].split_whitespace().map(Into::into).collect(),
                subject: f[5].into(),
            })
        })
        .collect();
    let more = list.len() > PAGE;
    list.truncate(PAGE);
    Ok((list, more))
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
    let mut args = vec![
        "diff-tree",
        "-r",
        "-z",
        "--raw",
        "--no-renames",
        "--no-abbrev",
        "--no-ext-diff",
    ];
    match &compared_to {
        Some(p) => args.extend([p.as_str(), sha]),
        None => args.extend(["--root", sha]),
    }
    let raw = git_bytes(wt, &args, SECS)?;
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

/// `diff-tree -z --raw` records: `:old new oldsha newsha S\0path\0`.
fn parse_raw(raw: &[u8]) -> Vec<FileChange> {
    let mut parts = raw.split(|b| *b == 0);
    let mut files = vec![];
    // The single-commit `--root` form prints the commit SHA first.
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
    let empty;
    let from = match &commit.compared_to {
        Some(p) => p.as_str(),
        None => {
            empty = empty_tree(wt)?;
            empty.as_str()
        }
    };
    text(
        wt,
        &[
            "--literal-pathspecs",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--full-index",
            "--no-renames",
            from,
            &commit.sha,
            "--",
            path,
        ],
    )
}

fn empty_tree(wt: &Path) -> Result<String, String> {
    // Hashed, not hard-coded: SHA-256 repositories have a different empty tree.
    let out = text(wt, &["hash-object", "-t", "tree", "/dev/null"])?;
    Ok(out.trim().to_string())
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

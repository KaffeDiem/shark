//! Composer completions: `@` files across the task's repos, `/` skills and commands, `#` PRs.
//! Sources load in the background; ranking runs per keystroke over the cached lists.
use crate::prs::PrStatus;
pub use crate::prs::{Pr, cached_repo_prs, fetch_repo_prs};
use crate::state::support_dir;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Write},
    ops::Range,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    sync::mpsc,
    time::Duration,
};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    File,
    Dir,
    Command,
    Pr(PrStatus),
    Issue,
}

#[derive(Clone, Debug)]
pub(crate) enum Reference {
    Issue(crate::linear::Hit),
    Pr {
        repo: String,
        label: String,
        pr: crate::git::PullRequest,
    },
}

#[derive(Clone, Debug)]
pub struct Item {
    pub kind: Kind,
    pub(crate) reference: Option<Reference>,
    /// Text that goes into the prompt, e.g. `@api/README.md`, `/review` or a PR's URL.
    pub insert: String,
    pub label: String,
    pub detail: String,
    /// Repo name for files and PRs, argument hint for commands.
    pub badge: String,
    /// Lowercased match target, with the name part starting at `name_at`.
    key: String,
    name_at: usize,
}

/// Every tracked and untracked-but-not-ignored path in each repo, plus their folders.
/// Paths are task-relative (`api/src/a.rs`), which is how Claude Code resolves `@`.
/// `task` is the task dir; its `.context` completes as `@.context/...`, with or without repos.
pub fn files(task: &Path, repos: &[(String, PathBuf)]) -> Vec<Item> {
    // -z: unquoted names; --deduplicate: one line per conflicted file, not per stage.
    let mut lists = crate::proc::par_map(repos, |(name, dir)| {
        let out = crate::git::ls_files(dir, &["-co", "--exclude-standard", "--deduplicate"])
            .unwrap_or_default();
        (name.clone(), out)
    });
    lists.push((".context".into(), context_paths(task)));
    file_items(&lists)
}

/// Every visible file under `<task>/.context`, relative to it. Dotfiles and dot-dirs (an
/// agent's cloned `.git`) are skipped, like the Artifacts list; nothing else is ignored.
fn context_paths(task: &Path) -> Vec<String> {
    let root = task.join(".context");
    ignore::WalkBuilder::new(&root)
        .standard_filters(false)
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| Some(e.path().strip_prefix(&root).ok()?.to_str()?.to_string()))
        .collect()
}

/// `title` as a file name: path separators become `-`, no leading dots or spaces (the
/// Artifacts list hides dotfiles), at most 200 bytes (room for a suffix under the 255 limit).
pub fn file_name(title: &str) -> String {
    let name: String = (title.chars())
        .map(|c| {
            if matches!(c, '/' | ':' | '\\') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let mut name = name.trim_start_matches(|c: char| c == '.' || c.is_whitespace());
    if name.len() > 200 {
        name = &name[..name.floor_char_boundary(200)];
    }
    match name.trim_end() {
        "" => "Untitled".into(),
        n => n.into(),
    }
}

/// Committed paths only: a new worktree never contains the checkout's local-only files.
#[cfg(test)]
pub fn committed_files(repos: &[(String, PathBuf, String, bool)]) -> Result<Vec<Item>, String> {
    committed_paths(repos).map(|repos| file_items(&repos.files))
}

pub(crate) struct CommittedPaths {
    pub files: Vec<(String, Vec<String>)>,
    pub skills: Vec<(String, Vec<String>)>,
}

/// Resolve each source once, keeping its committed paths available to both completion menus.
pub(crate) fn committed_paths(
    repos: &[(String, PathBuf, String, bool)],
) -> Result<CommittedPaths, String> {
    let lists = crate::proc::par_map(repos, |(name, dir, start, fresh)| {
        let reference = if *fresh || start.is_empty() {
            crate::git::resolve_base(dir, start)
        } else {
            let local = format!("refs/heads/{start}");
            let remote = format!("refs/remotes/origin/{start}");
            let has = |reference: &str| {
                crate::git::git(
                    dir,
                    &[
                        "rev-parse",
                        "--verify",
                        "--end-of-options",
                        &format!("{reference}^{{commit}}"),
                    ],
                )
                .is_ok()
            };
            match (has(&local), has(&remote)) {
                (true, true)
                    if crate::git::git(dir, &["merge-base", "--is-ancestor", &local, &remote])
                        .is_ok() =>
                {
                    remote
                }
                (true, _) => local,
                (false, true) => remote,
                _ => start.clone(),
            }
        };
        let commit = crate::git::git(
            dir,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{reference}^{{commit}}"),
            ],
        )?;
        let out = crate::git::git_bytes(dir, &["ls-tree", "-r", "-z", &commit, "--"])?;
        let mut paths = vec![];
        let mut links = vec![];
        let mut skills = vec![];
        let mut non_files = BTreeSet::new();
        for record in out.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let Some(tab) = record.iter().position(|b| *b == b'\t') else {
                continue;
            };
            let path = String::from_utf8_lossy(&record[tab + 1..]).into_owned();
            if record.starts_with(b"120000 ") {
                let header = String::from_utf8_lossy(&record[..tab]);
                if let Some(oid) = header.split_whitespace().nth(2) {
                    links.push((path.clone(), oid.to_string()));
                }
            }
            if record.starts_with(b"100644 ") || record.starts_with(b"100755 ") {
                if path.ends_with("/SKILL.md") {
                    skills.push(path.clone());
                }
            } else {
                non_files.insert(path.clone());
            }
            paths.push(path);
        }
        let definitions: BTreeSet<_> = skills.iter().cloned().collect();
        for (path, oid) in links {
            let Some((folder, skill)) = path.rsplit_once('/') else {
                continue;
            };
            let marker = skill == "SKILL.md";
            let skill_folder = if marker {
                folder.rsplit_once('/').map(|(folder, _)| folder)
            } else {
                Some(folder)
            };
            if !matches!(skill_folder, Some(".agents/skills" | ".claude/skills")) {
                continue;
            }
            let target = crate::git::git_bytes(dir, &["cat-file", "blob", &oid])?;
            let target = String::from_utf8_lossy(&target);
            let mut parts: Vec<_> = folder.split('/').collect();
            let within_repo = !target.starts_with('/')
                && target.split('/').all(|part| match part {
                    "" | "." => true,
                    ".." => parts.pop().is_some(),
                    part => {
                        parts.push(part);
                        true
                    }
                });
            // shortcut: resolve direct in-tree skill links; extend if chained or external links are needed.
            let target = parts.join("/");
            if within_repo && marker && paths.contains(&target) && !non_files.contains(&target) {
                skills.push(path);
            } else if within_repo && !marker && definitions.contains(&format!("{target}/SKILL.md"))
            {
                skills.push(format!("{path}/SKILL.md"));
            }
        }
        Ok::<_, String>((name.clone(), paths, skills))
    });
    let mut result = CommittedPaths {
        files: vec![],
        skills: vec![],
    };
    for list in lists {
        let (name, files, skills) = list?;
        result.files.push((name.clone(), files));
        result.skills.push((name, skills));
    }
    Ok(result)
}

pub(crate) fn file_items(lists: &[(String, Vec<String>)]) -> Vec<Item> {
    let mut items = vec![];
    for (repo, out) in lists {
        let mut dirs = BTreeSet::new();
        for path in out {
            let mut p = path.as_str();
            while let Some(i) = p.rfind('/') {
                p = &p[..i];
                if !dirs.insert(p) {
                    break;
                }
            }
            items.push(item(Kind::File, repo, path));
        }
        items.push(item(Kind::Dir, repo, ""));
        items.extend(dirs.into_iter().map(|d| item(Kind::Dir, repo, d)));
    }
    // Shallow paths first, so a bare `@` and ties favor README.md over a/b/c/README.md.
    items.sort_by_key(|i| i.key.len());
    items
}

fn item(kind: Kind, repo: &str, path: &str) -> Item {
    let full = match (kind, path) {
        (Kind::Dir, "") => format!("{repo}/"),
        (Kind::Dir, _) => format!("{repo}/{path}/"),
        _ => format!("{repo}/{path}"),
    };
    let name_at = |s: &str| s.trim_end_matches('/').rfind('/').map_or(0, |i| i + 1);
    let at = name_at(&full);
    // Lowercasing can change byte lengths (Kelvin sign, `İ`), so the key gets its own offset.
    let key = full.to_lowercase();
    Item {
        kind,
        reference: None,
        label: full[at..].to_string(),
        detail: full[..at].trim_end_matches('/').to_string(),
        badge: repo.to_string(),
        insert: mention(&full),
        name_at: name_at(&key),
        key,
    }
}

/// `@path`, quoted when it has whitespace: Claude Code silently drops an unquoted `@a b.png`.
pub fn mention(path: &str) -> String {
    match path.contains(char::is_whitespace) {
        true => format!("@\"{path}\""),
        false => format!("@{path}"),
    }
}

/// Where dropped and pasted files land. The task root isn't a repo, so they stay out of
/// diffs, and archive removes them with the task (with the rest of `.context`).
pub const ATTACHMENTS: &str = ".context/attachments";

/// The path to mention for a dropped or pasted path. A file inside the task is referenced
/// where it is; one outside is copied into [`ATTACHMENTS`] (a folder is referenced by absolute path).
pub fn attach(ws: &Path, src: &Path) -> Result<String, String> {
    let err = |e: std::io::Error| format!("{}: {e}", src.display());
    // Resolves symlinks too, so the copy holds the target's bytes.
    let src = src.canonicalize().map_err(err)?;
    let root = ws.canonicalize().map_err(err)?;
    // ponytail: outside folders aren't copied (size, nested .git); the agent can read them in place
    if src.is_dir() || src.starts_with(&root) {
        let shown = src.strip_prefix(&root).unwrap_or(&src);
        // The task itself is `./`; a bare `/` would be the filesystem root.
        let shown = if shown.as_os_str().is_empty() {
            Path::new(".")
        } else {
            shown
        };
        let slash = if src.is_dir() { "/" } else { "" };
        return Ok(format!("{}{slash}", shown.display()));
    }
    let name = src.file_name().unwrap_or_default().to_string_lossy();
    let ext = src.extension().unwrap_or_default().to_string_lossy();
    // Claude can't read HEIC, and a bad image block can break the session; sips ships with macOS.
    if ext.eq_ignore_ascii_case("heic") || ext.eq_ignore_ascii_case("heif") {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        // Converted to a temp file first, so the same HEIC dropped twice dedupes by JPEG bytes.
        let tmp = std::env::temp_dir().join(format!("shark-heic-{}-{n}.jpg", std::process::id()));
        let ok = Command::new("sips")
            .args(["-s", "format", "jpeg"])
            .arg(&src)
            .arg("--out")
            .arg(&tmp)
            .output()
            .is_ok_and(|o| o.status.success());
        let jpg = std::fs::read(&tmp);
        let _ = std::fs::remove_file(&tmp);
        let (true, Ok(jpg)) = (ok, jpg) else {
            return Err(format!("{}: couldn't convert to JPEG", src.display()));
        };
        let stem = src.file_stem().unwrap_or_default().to_string_lossy();
        return attach_bytes(ws, &format!("{stem}.jpg"), &jpg);
    }
    // Right away: screenshots dragged from the floating thumbnail are deleted within seconds.
    std::fs::read(&src)
        .map_err(err)
        .and_then(|b| attach_bytes(ws, &name, &b))
}

/// Saves `bytes` as attachment `name`, reusing an identical one, and returns its path.
pub fn attach_bytes(ws: &Path, name: &str, bytes: &[u8]) -> Result<String, String> {
    let err = |e: std::io::Error| format!("{name}: {e}");
    let same = |p: &Path| std::fs::read(p).is_ok_and(|b| b == bytes);
    let (dest, found) = slot(ws, name, same).map_err(err)?;
    if !found {
        std::fs::write(&dest, bytes).map_err(err)?;
    }
    Ok(rel(&dest))
}

fn rel(dest: &Path) -> String {
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    format!("{ATTACHMENTS}/{name}")
}

/// The attachment path for `name`: an existing file `same` accepts, else the first free one of
/// `name`, `name (1)`, … like Finder, created empty. `true` when it already exists.
fn slot(ws: &Path, name: &str, same: impl Fn(&Path) -> bool) -> std::io::Result<(PathBuf, bool)> {
    let dir = ws.join(ATTACHMENTS);
    std::fs::create_dir_all(&dir)?;
    // macOS screenshot names have U+202F before AM/PM, which tools often turn into a space and
    // then can't find the file; a `"` would end the quoted mention.
    let name: String = name
        .chars()
        .map(|c| match c {
            '\u{202F}' | '\u{00A0}' => ' ',
            '"' => '\'',
            c => c,
        })
        .collect();
    let p = Path::new(&name);
    let stem = p.file_stem().unwrap_or_default().to_string_lossy();
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 0.. {
        let cand = match n {
            0 => dir.join(&name),
            _ => dir.join(format!("{stem} ({n}){ext}")),
        };
        // create_new reserves the name, so two drops at once can't both pick it and overwrite.
        match std::fs::File::create_new(&cand) {
            Ok(_) => return Ok((cand, false)),
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
            // A copy still being written reads as different, so it's never wrongly reused.
            Err(_) if same(&cand) => return Ok((cand, true)),
            Err(_) => {}
        }
    }
    unreachable!()
}

#[derive(Serialize, Deserialize, Clone)]
struct Cmd {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default, rename = "argumentHint")]
    argument_hint: String,
}

fn cache_path(ws: &str) -> PathBuf {
    support_dir().join("commands").join(format!("{ws}.json"))
}

/// Commands from the last probe of this task, so `/` is instant after a restart.
pub fn cached_commands(ws: &str) -> Vec<Item> {
    std::fs::read(cache_path(ws))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<Cmd>>(&b).ok())
        .map(commands)
        .unwrap_or_default()
}

/// Asks `claude` for its commands (skills, plugin skills, built-ins) with the stream-json
/// `initialize` control request. Takes about a second, costs no tokens, starts no session.
pub fn probe_commands(ws: &str, dir: &Path, skills: &[String]) -> Option<Vec<Item>> {
    let mut child = Command::new("claude")
        .args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
        ])
        .args(skills)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let req =
        r#"{"type":"control_request","request_id":"commands","request":{"subtype":"initialize"}}"#;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = writeln!(stdin, "{req}");
        let line = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .find(|l| l.contains(r#""control_response""#));
        let _ = tx.send(line);
        drop(stdin); // EOF ends the process
    });
    let line = rx.recv_timeout(Duration::from_secs(20)).ok().flatten();
    let _ = child.kill();
    let _ = child.wait();
    let v: serde_json::Value = serde_json::from_str(&line?).ok()?;
    let cmds: Vec<Cmd> =
        serde_json::from_value(v["response"]["response"]["commands"].clone()).ok()?;
    save_commands(ws, &cmds);
    Some(commands(cmds))
}

fn save_commands(ws: &str, cmds: &[Cmd]) {
    if let Ok(json) = serde_json::to_vec(cmds) {
        let _ = crate::state::write(&cache_path(ws), &json);
    }
}

fn commands(cmds: Vec<Cmd>) -> Vec<Item> {
    cmds.into_iter()
        .map(|c| {
            let key = c.name.to_lowercase();
            Item {
                kind: Kind::Command,
                reference: None,
                insert: format!("/{}", c.name),
                label: format!("/{}", c.name),
                detail: c.description.lines().next().unwrap_or_default().to_string(),
                badge: c.argument_hint,
                // `plugin:skill` also matches on `skill`, with the name bonus.
                name_at: key.find(':').map_or(0, |i| i + 1),
                key,
            }
        })
        .collect()
}

/// Match the skill names the selected harness receives, keeping repository collisions visible.
pub(crate) fn draft_skill_names(
    agent: &[String],
    repos: &[(String, Vec<String>)],
    codex: bool,
) -> Vec<String> {
    let mut names = agent.to_vec();
    let mut used: std::collections::HashSet<_> = agent.iter().cloned().collect();
    let folders: &[&str] = if codex {
        &[".agents/skills", ".claude/skills"]
    } else {
        &[".claude/skills"]
    };
    for (repo, paths) in repos {
        let mut own = BTreeSet::new();
        for folder in folders {
            let prefix = format!("{folder}/");
            let skills: BTreeSet<_> = paths
                .iter()
                .filter_map(|path| path.strip_prefix(&prefix)?.strip_suffix("/SKILL.md"))
                .filter(|skill| !skill.is_empty() && !skill.contains('/'))
                .collect();
            for skill in skills {
                let skill = skill.to_string();
                if !own.insert(skill.clone()) {
                    continue;
                }
                let name = if codex {
                    crate::git::skill_link_name(&skill, repo, &mut used)
                } else if used.contains(&skill) {
                    let name = format!("{repo}:{skill}");
                    used.insert(name.clone());
                    name
                } else {
                    used.insert(skill.clone());
                    skill
                };
                names.push(name);
            }
        }
    }
    names
}

/// Skills explicitly attached to the agent or discovered in its selected repositories.
pub fn skill_items(names: Vec<String>) -> Vec<Item> {
    let names: BTreeSet<_> = names.into_iter().collect();
    commands(
        names
            .into_iter()
            .map(|name| Cmd {
                name,
                description: String::new(),
                argument_hint: String::new(),
            })
            .collect(),
    )
}

/// Menu rows, most recently updated first across repos. The key is `repo/number title author
/// branch`, so `api/` scopes to a repo as it does for files.
pub fn pr_items(prs: &[Pr]) -> Vec<Item> {
    let mut prs: Vec<&Pr> = prs.iter().collect();
    prs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    prs.into_iter()
        .map(|p| {
            let author = p.author.as_ref().map_or("", |a| a.login.as_str());
            let status = p.status();
            let state = match status {
                PrStatus::Open => String::new(),
                _ => format!(" · {}", status.label()),
            };
            let key = format!(
                "{}/{} {} {author} {}",
                p.repo, p.number, p.title, p.head_ref_name
            )
            .to_lowercase();
            Item {
                kind: Kind::Pr(status),
                reference: Some(Reference::Pr {
                    repo: p.repo.clone(),
                    label: format!("#{} {}{}", p.number, p.title, state),
                    pr: crate::git::PullRequest {
                        number: p.number,
                        url: p.url.clone(),
                        base: p.base_ref_name.clone(),
                    },
                }),
                insert: p.url.clone(),
                label: format!("#{}", p.number),
                detail: format!("{} · {author}{state}", p.title),
                badge: p.repo.clone(),
                name_at: key.find('/').map_or(0, |i| i + 1),
                key,
            }
        })
        .collect()
}

/// Issues precede PRs in the shared reference menu.
pub(crate) fn issue_items(issues: &[crate::linear::Hit]) -> Vec<Item> {
    issues
        .iter()
        .map(|issue| Item {
            kind: Kind::Issue,
            reference: Some(Reference::Issue(issue.clone())),
            insert: format!("{} {}", issue.identifier, issue.url),
            label: issue.identifier.clone(),
            detail: issue.title.clone(),
            badge: issue.team.clone(),
            key: format!(
                "{} {} {}",
                issue.identifier,
                issue.title,
                issue.assignee.as_deref().unwrap_or_default()
            )
            .to_lowercase(),
            name_at: 0,
        })
        .collect()
}

/// An issue identifier or URL pasted alone on a line can become a reference without swallowing prose.
pub(crate) fn pasted_reference(text: &str) -> Option<String> {
    let text = text.trim();
    if text.contains(char::is_whitespace) {
        return None;
    }
    if let Some(issue) = crate::linear::parse_issue(text) {
        return Some(issue.id);
    }
    let url = url::Url::parse(text).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    let parts: Vec<_> = url.path_segments()?.collect();
    if parts.len() != 4
        || parts[0].is_empty()
        || parts[1].is_empty()
        || parts[2] != "pull"
        || parts[3].is_empty()
        || !parts[3].bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some(format!("{} #{}", parts[1], parts[3]))
}

/// Like `rank`, but an all-digit query puts that exact number first, so `#12` beats `#120`.
pub fn rank_prs(items: &[Item], query: &str, limit: usize) -> Vec<usize> {
    let mut ix = rank(items, query, items.len());
    ix.sort_by_key(|&i| items[i].kind != Kind::Issue);
    let number = query.rsplit('/').next().unwrap_or(query);
    if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) {
        ix.sort_by_key(|&i| items[i].label.strip_prefix('#') != Some(number));
    }
    ix.truncate(limit);
    ix
}

/// Whether `query` names this PR outright rather than fuzzily: its exact number, or the start of
/// a word in its title, author or branch. Only then does enter pick it, so `#endif` or `#[test]`
/// typed in code stays text.
pub fn names_pr(item: &Item, query: &str) -> bool {
    let q = query.to_lowercase();
    let (scope, q) = q.split_once('/').unwrap_or(("", &q));
    let name = &item.key[item.name_at..];
    if item.kind == Kind::Issue
        && q.strip_suffix('-').is_some_and(|scope| {
            item.label
                .rsplit_once('-')
                .is_some_and(|(team, _)| team.eq_ignore_ascii_case(scope))
        })
    {
        return false;
    }
    if q.is_empty() || !item.key[..item.name_at].starts_with(scope) {
        return false;
    }
    if q.bytes().all(|b| b.is_ascii_digit()) {
        return item.label[1..] == *q;
    }
    name.match_indices(q)
        .any(|(i, _)| i == 0 || name[..i].ends_with([' ', '-', '_', '/', '.']))
}

/// The `@query`, `#query`, or a `/query` that starts the message, ending at the cursor:
/// its byte range and its trigger character.
pub fn trigger(text: &str, cursor: usize) -> Option<(Range<usize>, char)> {
    let before = text.get(..cursor)?;
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    match before[start..].chars().next()? {
        '@' => Some((start..cursor, '@')),
        // Claude Code only expands a slash command at the start of the prompt.
        '/' if before[..start].trim().is_empty() => Some((start..cursor, '/')),
        // `##` starts a heading, not a PR.
        '#' if !before[start + 1..].starts_with('#') => Some((start..cursor, '#')),
        _ => None,
    }
}

/// Indices of the best `limit` matches for `query`, best first.
// ponytail: linear scan per keystroke, ~1ms per 10k paths; move to a background matcher past ~200k.
pub fn rank(items: &[Item], query: &str, limit: usize) -> Vec<usize> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let mut hits: Vec<(i32, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(ix, it)| Some((score(&q, &it.key, it.name_at)?, ix)))
        .collect();
    hits.sort_by_key(|&(s, ix)| (-s, ix));
    hits.into_iter().take(limit).map(|(_, ix)| ix).collect()
}

pub(crate) fn score(q: &[char], key: &str, name_at: usize) -> Option<i32> {
    if q.is_empty() {
        return Some(0);
    }
    let in_name = greedy(q, &key[name_at..]).map(|s| s + 20);
    Some(in_name.max(greedy(q, key))? - key.len() as i32 / 8)
}

/// Subsequence match, rewarding runs and starts of words.
fn greedy(q: &[char], s: &str) -> Option<i32> {
    let (mut qi, mut score, mut run, mut prev) = (0, 0, false, '/');
    for c in s.chars() {
        if qi == q.len() {
            break;
        }
        let hit = c == q[qi];
        if hit {
            qi += 1;
            let start = matches!(prev, '/' | '_' | '-' | '.' | ' ' | ':');
            score += 1 + 4 * run as i32 + 3 * start as i32;
        }
        (run, prev) = (hit, c);
    }
    (qi == q.len()).then_some(score)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn inserts(items: &[Item], q: &str) -> Vec<String> {
        rank(items, q, 3)
            .into_iter()
            .map(|i| items[i].insert.clone())
            .collect()
    }

    #[test]
    fn file_name_replaces_separators_and_caps_length() {
        assert_eq!(file_name("a/b:c\\d"), "a-b-c-d");
        assert_eq!(file_name(&"x".repeat(300)).len(), 200);
    }

    #[test]
    fn file_name_is_never_hidden_and_never_empty() {
        assert_eq!(file_name("..env: setup"), "env- setup");
        assert_eq!(file_name("  . x"), "x");
        assert_eq!(file_name(""), "Untitled");
        assert_eq!(file_name(" ..  "), "Untitled");
    }

    #[test]
    fn file_name_caps_bytes_on_a_char_boundary() {
        let n = file_name(&"\u{65e5}".repeat(80));
        assert!(n.len() <= 200 && n.chars().all(|c| c == '\u{65e5}'), "{}", n.len());
        assert!(file_name(&"\u{1f988}".repeat(80)).len() <= 200);
    }

    #[test]
    fn context_walk_skips_dotfiles_and_dot_dirs() {
        let root = std::env::temp_dir().join(format!("shark-ctx-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".context/plans")).unwrap();
        fs::create_dir_all(root.join(".context/scratch/.git/objects")).unwrap();
        fs::write(root.join(".context/plans/a plan.md"), "x").unwrap();
        fs::write(root.join(".context/.hidden"), "x").unwrap();
        fs::write(root.join(".context/scratch/readme.md"), "x").unwrap();
        fs::write(root.join(".context/scratch/.git/objects/ab"), "x").unwrap();
        let mut got = context_paths(&root);
        got.sort();
        assert_eq!(got, ["plans/a plan.md", "scratch/readme.md"]);
        let items = files(&root, &[("api".into(), root.join("api"))]);
        assert!(
            items
                .iter()
                .any(|i| i.insert == "@\".context/plans/a plan.md\"")
        );
        assert!(context_paths(&root.join("none")).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn context_completes_for_a_task_without_repos() {
        let root = std::env::temp_dir().join(format!("shark-ctx-norepo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".context")).unwrap();
        fs::write(root.join(".context/notes.md"), "x").unwrap();
        let items = files(&root, &[]);
        assert!(items.iter().any(|i| i.insert == "@.context/notes.md"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn attach_copies_dedupes_and_references_in_place() {
        let root = std::env::temp_dir().join(format!("shark-attach-{}", std::process::id()));
        let (ws, out) = (root.join("ws"), root.join("out"));
        fs::create_dir_all(ws.join("web/src")).unwrap();
        fs::create_dir_all(&out).unwrap();
        fs::write(ws.join("web/src/a.rs"), "x").unwrap();
        let shot = out.join("Screenshot 2026-10-25 at 10.22.33\u{202F}AM.png");
        fs::write(&shot, "png").unwrap();
        let want = ".context/attachments/Screenshot 2026-10-25 at 10.22.33 AM.png";
        assert_eq!(attach(&ws, &shot).unwrap(), want);
        // Same bytes again: the copy is reused.
        assert_eq!(attach(&ws, &shot).unwrap(), want);
        // Same name, other bytes: Finder-style suffix.
        let other = root.join("Screenshot 2026-10-25 at 10.22.33\u{202F}AM.png");
        fs::write(&other, "other").unwrap();
        let got = attach(&ws, &other).unwrap();
        assert_eq!(
            got,
            ".context/attachments/Screenshot 2026-10-25 at 10.22.33 AM (1).png"
        );
        assert_eq!(fs::read_dir(ws.join(ATTACHMENTS)).unwrap().count(), 2);
        // Inside the task: referenced, not copied.
        assert_eq!(
            attach(&ws, &ws.join("web/src/a.rs")).unwrap(),
            "web/src/a.rs"
        );
        assert_eq!(attach(&ws, &ws.join("web")).unwrap(), "web/");
        assert_eq!(attach(&ws, &ws).unwrap(), "./");
        assert_eq!(
            attach_bytes(&ws, "Pasted image.png", b"img").unwrap(),
            ".context/attachments/Pasted image.png"
        );
        assert_eq!(
            attach_bytes(&ws, "Pasted image.png", b"img").unwrap(),
            ".context/attachments/Pasted image.png"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn ranks_names_and_scopes_by_repo() {
        let root = std::env::temp_dir().join(format!("shark-complete-{}", std::process::id()));
        let mut repos = vec![];
        for name in ["api", "web"] {
            let dir = root.join(name);
            fs::create_dir_all(dir.join("docs/deep")).unwrap();
            for f in [
                "README.md",
                "docs/deep/README.md",
                "src_reader.rs",
                "q\"uote.md",
            ] {
                fs::write(dir.join(f), "").unwrap();
            }
            fs::write(dir.join(".gitignore"), "target/\n").unwrap();
            fs::create_dir_all(dir.join("target")).unwrap();
            fs::write(dir.join("target/README.md"), "").unwrap();
            crate::git::git(&dir, &["init", "-q"]).unwrap();
            repos.push((name.to_string(), dir));
        }
        let items = files(&root, &repos);
        assert!(
            !items.iter().any(|i| i.insert.contains("target")),
            "gitignored"
        );
        assert_eq!(
            inserts(&items, "readme")[..2],
            ["@api/README.md", "@web/README.md"]
        );
        assert_eq!(inserts(&items, "web/readme")[0], "@web/README.md");
        assert_eq!(
            inserts(&items, "quote")[0],
            "@api/q\"uote.md",
            "not C-quoted"
        );
        assert_eq!(inserts(&items, "api/deep")[0], "@api/docs/deep/");
        let readme = items
            .iter()
            .find(|i| i.insert == "@web/docs/deep/README.md")
            .unwrap();
        assert_eq!(
            (readme.label.as_str(), readme.detail.as_str()),
            ("README.md", "web/docs/deep")
        );
        // 20k paths: well under a frame.
        let many: Vec<Item> = (0..20_000)
            .map(|i| item(Kind::File, "r", &format!("d{i}/f{i}.rs")))
            .collect();
        let t = std::time::Instant::now();
        rank(&many, "f19", 50);
        assert!(t.elapsed() < Duration::from_millis(40), "{:?}", t.elapsed());
        // The Kelvin sign lowercases to a shorter `k`; slicing the key must not panic.
        let odd = [item(Kind::File, "r", "\u{212A}\u{212A}\u{212A}/é.rs")];
        assert_eq!(rank(&odd, "é", 1), [0]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn draft_files_use_selected_commit_and_exclude_local_files() {
        let root =
            std::env::temp_dir().join(format!("shark-committed-complete-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| crate::git::git(&root, args).unwrap();
        git(&["init", "-q"]);
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/a b.md"), "base").unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "base",
        ]);
        let base = git(&["rev-parse", "HEAD"]);
        fs::write(root.join("head-only.md"), "head").unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "head",
        ]);
        fs::write(root.join("local-only.md"), "local").unwrap();
        let at_base = committed_files(&[("api".into(), root.clone(), base.clone(), true)]).unwrap();
        assert!(
            at_base
                .iter()
                .any(|item| item.insert == "@\"api/docs/a b.md\"")
        );
        assert!(at_base.iter().any(|item| item.insert == "@api/docs/"));
        assert!(!at_base.iter().any(|item| item.insert.contains("only")));
        let at_head =
            committed_files(&[("api".into(), root.clone(), "HEAD".into(), false)]).unwrap();
        assert!(
            at_head
                .iter()
                .any(|item| item.insert == "@api/head-only.md")
        );
        assert!(
            !at_head
                .iter()
                .any(|item| item.insert.contains("local-only"))
        );
        assert!(
            committed_files(&[("api".into(), root.clone(), "missing-ref".into(), false)]).is_err()
        );
        let default =
            committed_files(&[("api".into(), root.clone(), String::new(), true)]).unwrap();
        assert!(
            default
                .iter()
                .any(|item| item.insert == "@api/head-only.md")
        );
        git(&["update-ref", "refs/remotes/origin/remote-only", "HEAD"]);
        let remote =
            committed_files(&[("api".into(), root.clone(), "remote-only".into(), false)]).unwrap();
        assert!(remote.iter().any(|item| item.insert == "@api/head-only.md"));
        git(&["update-ref", "refs/heads/behind", &base]);
        git(&["update-ref", "refs/remotes/origin/behind", "HEAD"]);
        let caught_up =
            committed_files(&[("api".into(), root.clone(), "behind".into(), false)]).unwrap();
        assert!(
            caught_up
                .iter()
                .any(|item| item.insert == "@api/head-only.md")
        );
        let new = committed_files(&[("api".into(), root.clone(), "behind".into(), true)]).unwrap();
        assert!(new.iter().any(|item| item.insert == "@api/head-only.md"));
        assert_eq!(
            git(&["rev-parse", "refs/heads/behind"]),
            base,
            "listing must not move a branch"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn draft_skills_follow_selected_commit_instead_of_source_checkout() {
        let root =
            std::env::temp_dir().join(format!("shark-selected-skills-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".claude/skills/old")).unwrap();
        let git = |args: &[&str]| crate::git::git(&root, args).unwrap();
        git(&["init", "-q"]);
        fs::write(root.join(".claude/skills/old/SKILL.md"), "old").unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "old",
        ]);
        let old = git(&["rev-parse", "HEAD"]);
        fs::remove_dir_all(root.join(".claude/skills/old")).unwrap();
        fs::create_dir_all(root.join(".claude/skills/new")).unwrap();
        fs::write(root.join(".claude/skills/new/SKILL.md"), "new").unwrap();
        std::os::unix::fs::symlink("new", root.join(".claude/skills/linked")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/review.md"), "linked definition").unwrap();
        fs::create_dir_all(root.join(".claude/skills/file-link")).unwrap();
        std::os::unix::fs::symlink(
            "../../../docs/review.md",
            root.join(".claude/skills/file-link/SKILL.md"),
        )
        .unwrap();
        std::os::unix::fs::symlink(&root, root.join(".claude/skills/external")).unwrap();
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "new",
        ]);
        let selected = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "--detach", &old]);
        fs::create_dir_all(root.join(".claude/skills/local-only")).unwrap();
        fs::write(root.join(".claude/skills/local-only/SKILL.md"), "local").unwrap();
        let sources = vec![("api".into(), root.clone(), selected, false)];
        assert!(
            committed_files(&sources)
                .unwrap()
                .iter()
                .any(|i| i.insert.ends_with("new/SKILL.md"))
        );
        let repos = committed_paths(&sources).unwrap();
        assert_eq!(
            draft_skill_names(&["attached".into()], &repos.skills, false),
            ["attached", "file-link", "linked", "new"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn draft_skill_collisions_match_each_harness() {
        let repos = vec![
            (
                "api".into(),
                vec![
                    ".agents/skills/review/SKILL.md".into(),
                    ".claude/skills/review/SKILL.md".into(),
                    ".claude/skills/deploy/SKILL.md".into(),
                    ".claude/skills/nested/invalid/SKILL.md".into(),
                ],
            ),
            ("web".into(), vec![".claude/skills/review/SKILL.md".into()]),
        ];
        let agent = vec!["review".into(), "api-review".into()];
        assert_eq!(
            draft_skill_names(&agent, &repos, true),
            [
                "review",
                "api-review",
                "api-review-2",
                "deploy",
                "web-review"
            ]
        );
        assert_eq!(
            draft_skill_names(&agent, &repos, false),
            ["review", "api-review", "deploy", "api:review", "web:review"]
        );
        assert_eq!(
            draft_skill_names(&[], &repos, false),
            ["deploy", "review", "web:review"]
        );
        assert_eq!(
            draft_skill_names(&agent, &[], true),
            agent,
            "disabled repository skills retain agent skills"
        );
    }

    #[test]
    fn agent_and_repository_skills_are_deduplicated() {
        let skills = skill_items(vec!["review".into(), "test".into(), "review".into()]);
        assert_eq!(skills.len(), 2);
        assert_eq!(inserts(&skills, "rev"), ["/review"]);
    }

    #[test]
    fn finds_triggers() {
        assert_eq!(trigger("see @api/RE", 11), Some((4..11, '@')));
        assert_eq!(trigger("@", 1), Some((0..1, '@')));
        assert_eq!(trigger("mail me@x.com", 13), None);
        assert_eq!(trigger("  /rev", 6), Some((2..6, '/')));
        assert_eq!(trigger("fix #1", 6), Some((4..6, '#')));
        assert_eq!(trigger("#", 1), Some((0..1, '#')));
        assert_eq!(trigger("## h", 2), None, "a heading");
        assert_eq!(trigger("C#", 2), None);
        assert_eq!(trigger("a /rev", 6), None, "slash only starts a prompt");
        assert_eq!(trigger("@a done", 7), None);
        let cmds = commands(vec![Cmd {
            name: "ponytail:review".into(),
            description: "Review\nmore".into(),
            argument_hint: String::new(),
        }]);
        assert_eq!(cmds[0].detail, "Review");
        assert_eq!(rank(&cmds, "review", 1), [0]);
    }

    #[test]
    fn lists_and_ranks_prs() {
        let pr = |n: u64, title: &str, updated: &str| {
            format!(
                r#"{{"number":{n},"title":"{title}","author":{{"login":"kim"}},"headRefName":"b{n}","url":"https://github.com/o/api/pull/{n}","state":"OPEN","isDraft":false,"updatedAt":"{updated}"}}"#
            )
        };
        let api = format!(
            "[{},{}]",
            pr(120, "Fix endpoint", "1"),
            pr(12, "Login page", "3")
        );
        let mut prs = crate::prs::parse_prs("api", &api).unwrap();
        prs.extend(
            crate::prs::parse_prs("web", &format!("[{}]", pr(7, "Merged thing", "2"))).unwrap(),
        );
        let items = pr_items(&prs);
        // Newest first across repos.
        assert_eq!(
            inserts(&items, ""),
            [
                "https://github.com/o/api/pull/12",
                "https://github.com/o/api/pull/7",
                "https://github.com/o/api/pull/120"
            ]
        );
        assert_eq!(items[0].label, "#12");
        assert_eq!(items[1].badge, "web");
        let top = |q: &str| items[rank_prs(&items, q, 50)[0]].label.clone();
        assert_eq!(top("12"), "#12");
        assert_eq!(top("120"), "#120");
        assert_eq!(top("web/"), "#7");
        // Enter only picks what the query names outright.
        let named =
            |q: &str| (rank_prs(&items, q, 50).first()).is_some_and(|&i| names_pr(&items[i], q));
        assert!(named("12") && named("login") && named("kim") && named("api/end"));
        assert!(!named("") && !named("1") && !named("web/"));
        assert!(
            !named("fe") && !rank_prs(&items, "fe", 50).is_empty(),
            "fuzzy hit in Fix endpoint"
        );
    }

    #[test]
    fn cached_commands_round_trip() {
        let ws = format!("cmds-round-trip-{}", std::process::id());
        assert!(cached_commands(&ws).is_empty());
        save_commands(
            &ws,
            &[Cmd {
                name: "ponytail:review".into(),
                description: "Review\nmore".into(),
                argument_hint: "[pr]".into(),
            }],
        );
        let items = cached_commands(&ws);
        assert_eq!(items.len(), 1);
        assert_eq!(
            (
                items[0].insert.as_str(),
                items[0].detail.as_str(),
                items[0].badge.as_str()
            ),
            ("/ponytail:review", "Review", "[pr]")
        );
        fs::remove_file(cache_path(&ws)).unwrap();
    }

    /// Real CLI, no tokens: `cargo test live_probe -- --ignored`.
    #[test]
    #[ignore]
    fn live_probe() {
        let cmds = probe_commands("live-probe-test", &std::env::temp_dir(), &[]).expect("probe");
        let _ = fs::remove_file(cache_path("live-probe-test"));
        assert!(
            cmds.iter().any(|c| c.insert == "/compact"),
            "{} commands",
            cmds.len()
        );
    }
    #[test]
    fn issue_references_share_pr_arming() {
        let issue = Item {
            kind: Kind::Issue,
            reference: None,
            insert: "SHA-42 https://linear.app/acme/issue/SHA-42/test".into(),
            label: "SHA-42".into(),
            detail: "Fix tray".into(),
            badge: "SHA".into(),
            key: "sha-42 fix tray alice".into(),
            name_at: 0,
        };
        assert!(names_pr(&issue, "SHA-42"));
        assert!(names_pr(&issue, "SHA-4"));
        assert!(!names_pr(&issue, "SHA-"));
        assert!(names_pr(&issue, "tray"));
        assert!(!names_pr(&issue, "endif"));
    }
    #[test]
    fn pasted_reference_accepts_issue_identifiers_and_reference_urls() {
        assert_eq!(
            pasted_reference("https://github.com/org/shark/pull/93"),
            Some("shark #93".into())
        );
        assert_eq!(
            pasted_reference("https://linear.app/org/issue/SHA-42/fix"),
            Some("SHA-42".into())
        );
        assert_eq!(pasted_reference("SHA-42"), Some("SHA-42".into()));
        for text in [
            "see SHA-42",
            "SHA-no",
            "see https://github.com/org/shark/pull/93",
            "https://github.com/org/shark/issues/93",
            "https://github.com.evil/org/shark/pull/93",
            "https://github.com/org/shark/pull/93/files",
        ] {
            assert!(pasted_reference(text).is_none(), "{text}");
        }
    }
}

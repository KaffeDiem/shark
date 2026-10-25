//! Composer completions: `@` files across the workspace's repos, `/` skills and commands.
//! Sources load in the background; ranking runs per keystroke over the cached lists.
use crate::state::support_dir;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Write},
    ops::Range,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    File,
    Dir,
    Command,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub kind: Kind,
    /// Text that goes into the prompt, e.g. `@api/README.md` or `/review`.
    pub insert: String,
    pub label: String,
    pub detail: String,
    /// Repo name for files, argument hint for commands.
    pub badge: String,
    /// Lowercased match target, with the name part starting at `name_at`.
    key: String,
    name_at: usize,
}

/// Every tracked and untracked-but-not-ignored path in each repo, plus their folders.
/// Paths are workspace-relative (`api/src/a.rs`), which is how Claude Code resolves `@`.
pub fn files(repos: &[(String, PathBuf)]) -> Vec<Item> {
    let lists: Vec<(String, String)> = std::thread::scope(|s| {
        let jobs: Vec<_> = repos
            .iter()
            .map(|(name, dir)| {
                s.spawn(move || {
                    let out = crate::git::git(
                        dir,
                        &["-c", "core.quotepath=off", "ls-files", "-co", "--exclude-standard"],
                    );
                    (name.clone(), out.unwrap_or_default())
                })
            })
            .collect();
        jobs.into_iter().filter_map(|j| j.join().ok()).collect()
    });
    let mut items = vec![];
    for (repo, out) in &lists {
        let mut dirs = BTreeSet::new();
        for path in out.lines() {
            let mut p = path;
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
    let trimmed = full.trim_end_matches('/');
    let name_at = trimmed.rfind('/').map_or(0, |i| i + 1);
    Item {
        kind,
        label: full[name_at..].to_string(),
        detail: full[..name_at].trim_end_matches('/').to_string(),
        badge: repo.to_string(),
        insert: format!("@{full}"),
        key: full.to_lowercase(),
        name_at,
    }
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

/// Commands from the last probe of this workspace, so `/` is instant after a restart.
pub fn cached_commands(ws: &str) -> Vec<Item> {
    std::fs::read(cache_path(ws))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<Cmd>>(&b).ok())
        .map(commands)
        .unwrap_or_default()
}

/// Asks `claude` for its commands (skills, plugin skills, built-ins) with the stream-json
/// `initialize` control request. Takes about a second, costs no tokens, starts no session.
pub fn probe_commands(ws: &str, dir: &Path) -> Option<Vec<Item>> {
    let mut child = Command::new("claude")
        .args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let req = r#"{"type":"control_request","request_id":"commands","request":{"subtype":"initialize"}}"#;
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
    let cmds: Vec<Cmd> = serde_json::from_value(v["response"]["response"]["commands"].clone()).ok()?;
    let path = cache_path(ws);
    let _ = std::fs::create_dir_all(path.parent()?);
    let _ = std::fs::write(&path, serde_json::to_vec(&cmds).ok()?);
    Some(commands(cmds))
}

fn commands(cmds: Vec<Cmd>) -> Vec<Item> {
    cmds.into_iter()
        .map(|c| Item {
            kind: Kind::Command,
            insert: format!("/{}", c.name),
            label: format!("/{}", c.name),
            detail: c.description.lines().next().unwrap_or_default().to_string(),
            badge: c.argument_hint,
            // `plugin:skill` also matches on `skill`, with the name bonus.
            name_at: c.name.find(':').map_or(0, |i| i + 1),
            key: c.name.to_lowercase(),
        })
        .collect()
}

/// The `@query`, or a `/query` that starts the message, ending at the cursor:
/// its byte range and whether it's a command.
pub fn trigger(text: &str, cursor: usize) -> Option<(Range<usize>, bool)> {
    let before = text.get(..cursor)?;
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    match before[start..].chars().next()? {
        '@' => Some((start..cursor, false)),
        // Claude Code only expands a slash command at the start of the prompt.
        '/' if before[..start].trim().is_empty() => Some((start..cursor, true)),
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

fn score(q: &[char], key: &str, name_at: usize) -> Option<i32> {
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
        if c == q[qi] {
            qi += 1;
            score += 1 + if run { 4 } else { 0 };
            score += if matches!(prev, '/' | '_' | '-' | '.' | ' ' | ':') { 3 } else { 0 };
            run = true;
        } else {
            run = false;
        }
        prev = c;
    }
    (qi == q.len()).then_some(score)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn inserts(items: &[Item], q: &str) -> Vec<String> {
        rank(items, q, 3).into_iter().map(|i| items[i].insert.clone()).collect()
    }

    #[test]
    fn ranks_names_and_scopes_by_repo() {
        let root = std::env::temp_dir().join(format!("shark-complete-{}", std::process::id()));
        let mut repos = vec![];
        for name in ["api", "web"] {
            let dir = root.join(name);
            fs::create_dir_all(dir.join("docs/deep")).unwrap();
            for f in ["README.md", "docs/deep/README.md", "src_reader.rs"] {
                fs::write(dir.join(f), "").unwrap();
            }
            fs::write(dir.join(".gitignore"), "target/\n").unwrap();
            fs::create_dir_all(dir.join("target")).unwrap();
            fs::write(dir.join("target/README.md"), "").unwrap();
            crate::git::git(&dir, &["init", "-q"]).unwrap();
            repos.push((name.to_string(), dir));
        }
        let items = files(&repos);
        assert!(!items.iter().any(|i| i.insert.contains("target")), "gitignored");
        assert_eq!(inserts(&items, "readme")[..2], ["@api/README.md", "@web/README.md"]);
        assert_eq!(inserts(&items, "web/readme")[0], "@web/README.md");
        assert_eq!(inserts(&items, "api/deep")[0], "@api/docs/deep/");
        let readme = items.iter().find(|i| i.insert == "@web/docs/deep/README.md").unwrap();
        assert_eq!((readme.label.as_str(), readme.detail.as_str()), ("README.md", "web/docs/deep"));
        // 20k paths: well under a frame.
        let many: Vec<Item> = (0..20_000).map(|i| item(Kind::File, "r", &format!("d{i}/f{i}.rs"))).collect();
        let t = std::time::Instant::now();
        rank(&many, "f19", 50);
        assert!(t.elapsed() < Duration::from_millis(40), "{:?}", t.elapsed());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_triggers() {
        assert_eq!(trigger("see @api/RE", 11), Some((4..11, false)));
        assert_eq!(trigger("@", 1), Some((0..1, false)));
        assert_eq!(trigger("mail me@x.com", 13), None);
        assert_eq!(trigger("  /rev", 6), Some((2..6, true)));
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

    /// Real CLI, no tokens: `cargo test live_probe -- --ignored`.
    #[test]
    #[ignore]
    fn live_probe() {
        let cmds = probe_commands("live-probe-test", &std::env::temp_dir()).expect("probe");
        let _ = fs::remove_file(cache_path("live-probe-test"));
        assert!(cmds.iter().any(|c| c.insert == "/compact"), "{} commands", cmds.len());
    }
}

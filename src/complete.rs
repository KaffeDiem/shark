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
    sync::atomic::{AtomicUsize, Ordering},
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
    let lists = crate::git::par_map(repos, |(name, dir)| {
        // -z: unquoted names; --deduplicate: one line per conflicted file, not per stage.
        let out = crate::git::ls_files(dir, &["-co", "--exclude-standard", "--deduplicate"]);
        (name.clone(), out)
    });
    let mut items = vec![];
    for (repo, out) in &lists {
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

/// Where dropped and pasted files land. The workspace root isn't a repo, so they stay out of
/// diffs, and archive removes them with the workspace.
pub const ATTACHMENTS: &str = ".context/attachments";

/// The path to mention for a dropped or pasted path. A file inside the workspace is referenced
/// where it is; one outside is copied into [`ATTACHMENTS`] (a folder is referenced by absolute path).
pub fn attach(ws: &Path, src: &Path) -> Result<String, String> {
    let err = |e: std::io::Error| format!("{}: {e}", src.display());
    // Resolves symlinks too, so the copy holds the target's bytes.
    let src = src.canonicalize().map_err(err)?;
    let root = ws.canonicalize().map_err(err)?;
    // ponytail: outside folders aren't copied (size, nested .git); the agent can read them in place
    if src.is_dir() || src.starts_with(&root) {
        let shown = src.strip_prefix(&root).unwrap_or(&src);
        // The workspace itself is `./`; a bare `/` would be the filesystem root.
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
        .args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
        ])
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
    let path = cache_path(ws);
    let _ = std::fs::create_dir_all(path.parent()?);
    let _ = std::fs::write(&path, serde_json::to_vec(&cmds).ok()?);
    Some(commands(cmds))
}

fn commands(cmds: Vec<Cmd>) -> Vec<Item> {
    cmds.into_iter()
        .map(|c| {
            let key = c.name.to_lowercase();
            Item {
                kind: Kind::Command,
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
        // Inside the workspace: referenced, not copied.
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
        let items = files(&repos);
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
        assert!(
            cmds.iter().any(|c| c.insert == "/compact"),
            "{} commands",
            cmds.len()
        );
    }
}

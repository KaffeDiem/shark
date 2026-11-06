//! Server-requested file watching, scoped to one worktree and polled off the UI thread.
use super::client::Client;
use anyhow::{Result, anyhow, bail};
use globset::{GlobBuilder, GlobMatcher};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

const POLL: Duration = Duration::from_millis(750);

#[derive(Clone)]
struct Pattern {
    base: PathBuf,
    glob: GlobMatcher,
    absolute: bool,
    kind: u64,
}

impl Pattern {
    fn matches(&self, path: &Path) -> bool {
        path.strip_prefix(&self.base).is_ok_and(|relative| {
            self.glob
                .is_match(if self.absolute { path } else { relative })
        })
    }
}

type Stamp = (u64, Option<SystemTime>);
type Snapshot = HashMap<PathBuf, Stamp>;
/// Registrations by server id; the key changes whenever the id is registered again. The third
/// field is the key and patterns of the registration this one replaced, whose baseline it inherits.
type Registrations = HashMap<String, (u64, Vec<Pattern>, Option<(u64, Vec<Pattern>)>)>;

#[derive(Default)]
struct Tree {
    files: Snapshot,
    dirs: HashSet<PathBuf>,
}

struct PendingRegistration {
    ids: Vec<String>,
    ready: async_channel::Sender<std::result::Result<(), String>>,
}

pub struct Watcher {
    root: PathBuf,
    registrations: Arc<Mutex<Registrations>>,
    pending: Arc<Mutex<Vec<PendingRegistration>>>,
    stop: Arc<AtomicBool>,
    wake: mpsc::Sender<()>,
    errors: async_channel::Receiver<String>,
}

impl Watcher {
    /// Root must be the resolved worktree/project root used for this client's initialization.
    pub fn new(root: &Path, client: Client) -> Result<Self> {
        validate_absolute(root)?;
        let root = root.to_path_buf();
        let registrations = Arc::new(Mutex::new(Registrations::new()));
        let pending = Arc::new(Mutex::new(Vec::<PendingRegistration>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (wake, wait) = mpsc::channel();
        let (error_tx, errors) = async_channel::bounded(1);
        let worker_root = root.clone();
        let worker_registrations = registrations.clone();
        let worker_stop = stop.clone();
        let worker_pending = pending.clone();
        thread::spawn(move || {
            let mut previous: HashMap<u64, Snapshot> = HashMap::new();
            while !worker_stop.load(Ordering::Relaxed) {
                let registrations = worker_registrations.lock().unwrap().clone();
                // A re-registered id keeps the replaced registration's baseline, so changes made
                // before the next poll are still reported. ponytail: a second replacement within
                // one poll has no baseline to inherit and starts fresh.
                let mut inherited = HashMap::new();
                for (key, _, replaced) in registrations.values() {
                    if let Some((old, old_patterns)) = replaced
                        && !previous.contains_key(key)
                        && let Some(baseline) = previous.remove(old)
                    {
                        previous.insert(*key, baseline);
                        inherited.insert(*key, old_patterns);
                    }
                }
                previous.retain(|key, _| registrations.values().any(|(k, ..)| k == key));
                if !registrations.is_empty() {
                    // A new baseline must postdate its registration, so it cannot reuse a shared walk.
                    let fresh = registrations
                        .values()
                        .any(|(key, ..)| !previous.contains_key(key));
                    match snapshot(&worker_root, &registrations, fresh, &worker_stop) {
                        _ if worker_stop.load(Ordering::Relaxed) => break,
                        Ok(trees) => {
                            let changes =
                                poll_changes(&registrations, &mut previous, &trees, &inherited);
                            // The server may assume watching is active after registration succeeds.
                            // Acknowledge only once every pattern in the batch has its baseline.
                            let current = worker_registrations.lock().unwrap();
                            worker_pending.lock().unwrap().retain(|pending| {
                                if pending.ids.iter().all(|id| {
                                    current
                                        .get(id)
                                        .is_some_and(|(key, ..)| previous.contains_key(key))
                                }) {
                                    let _ = pending.ready.try_send(Ok(()));
                                    false
                                } else {
                                    true
                                }
                            });
                            drop(current);
                            if !changes.is_empty() {
                                let changes: Vec<_> = changes
                                    .into_iter()
                                    .filter_map(|(path, bit)| {
                                        // Watch-kind bits 1, 2, 4 are LSP change types 1, 2, 3.
                                        let kind = bit.trailing_zeros() + 1;
                                        url::Url::from_file_path(path)
                                            .ok()
                                            .map(|uri| json!({"uri":uri.as_str(),"type":kind}))
                                    })
                                    .collect();
                                if client
                                    .notify(
                                        "workspace/didChangeWatchedFiles",
                                        json!({"changes":changes}),
                                    )
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                        Err(error) => {
                            let failed = std::mem::take(&mut *worker_pending.lock().unwrap());
                            let mut current = worker_registrations.lock().unwrap();
                            for pending in failed {
                                for id in pending.ids {
                                    current.remove(&id);
                                }
                                let _ = pending.ready.try_send(Err(error.to_string()));
                            }
                            let _ = error_tx.try_send(error.to_string());
                        }
                    }
                }
                // Wake promptly when the last editor closes or registrations change.
                let _ = wait.recv_timeout(POLL);
            }
            // The last walk can hold the whole tree; nothing frees it once watching stops.
            (RECENT.lock().unwrap_or_else(|poison| poison.into_inner()))
                .retain(|(path, started, _)| *path != worker_root && started.elapsed() < POLL);
        });
        Ok(Self {
            root,
            registrations,
            pending,
            stop,
            wake,
            errors,
        })
    }

    pub fn errors(&self) -> async_channel::Receiver<String> {
        self.errors.clone()
    }

    /// Validates the complete batch before installing any registration.
    pub fn register(
        &self,
        params: &Value,
    ) -> Result<async_channel::Receiver<std::result::Result<(), String>>> {
        let registrations = params["registrations"]
            .as_array()
            .ok_or_else(|| anyhow!("Missing capability registrations"))?;
        let mut parsed = HashMap::new();
        for registration in registrations {
            let id = registration["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| anyhow!("Missing registration id"))?;
            if registration["method"] != "workspace/didChangeWatchedFiles" {
                bail!(
                    "Unsupported dynamic registration: {}",
                    registration["method"]
                );
            }
            let watchers = registration["registerOptions"]["watchers"]
                .as_array()
                .ok_or_else(|| anyhow!("Missing file watchers"))?;
            let patterns = watchers
                .iter()
                .filter_map(|watcher| pattern(&self.root, watcher).transpose())
                .collect::<Result<Vec<_>>>()?;
            if parsed.insert(id.to_string(), patterns).is_some() {
                bail!("Duplicate registration id: {id}");
            }
        }
        if parsed.is_empty() {
            let (ready, receiver) = async_channel::bounded(1);
            let _ = ready.try_send(Ok(()));
            return Ok(receiver);
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut current = self.registrations.lock().unwrap();
        let ids = parsed.keys().cloned().collect();
        // rust-analyzer re-registers its fixed id to replace its watchers, without unregistering.
        for (id, patterns) in parsed {
            let replaced = current
                .get(&id)
                .map(|(key, patterns, _)| (*key, patterns.clone()));
            let key = NEXT.fetch_add(1, Ordering::Relaxed);
            current.insert(id, (key, patterns, replaced));
        }
        let (ready, receiver) = async_channel::bounded(1);
        self.pending
            .lock()
            .unwrap()
            .push(PendingRegistration { ids, ready });
        let _ = self.wake.send(());
        Ok(receiver)
    }

    pub fn unregister(&self, params: &Value) -> Result<()> {
        // The protocol's field name intentionally retains its historical misspelling.
        let registrations = params["unregisterations"]
            .as_array()
            .ok_or_else(|| anyhow!("Missing capability unregistrations"))?;
        let mut ids = HashSet::new();
        for registration in registrations {
            if registration["method"] != "workspace/didChangeWatchedFiles" {
                bail!("Unsupported dynamic unregistration");
            }
            let id = registration["id"]
                .as_str()
                .ok_or_else(|| anyhow!("Missing unregistration id"))?;
            if !ids.insert(id) {
                bail!("Duplicate unregistration id");
            }
        }
        let mut current = self.registrations.lock().unwrap();
        if ids.iter().any(|id| !current.contains_key(*id)) {
            bail!("Unknown file watcher registration");
        }
        for id in &ids {
            current.remove(*id);
        }
        self.pending.lock().unwrap().retain(|pending| {
            if pending.ids.iter().any(|id| ids.contains(id.as_str())) {
                let _ = pending.ready.try_send(Err(
                    "File watcher was unregistered before it became ready".into(),
                ));
                false
            } else {
                true
            }
        });
        let _ = self.wake.send(());
        Ok(())
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.wake.send(());
    }
}

fn validate_absolute(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        bail!("File watcher root must be an absolute normalized path");
    }
    Ok(())
}

/// `None` for a base outside the worktree, which this watcher does not serve.
fn pattern(root: &Path, watcher: &Value) -> Result<Option<Pattern>> {
    let kind = watcher
        .get("kind")
        .map(|value| value.as_u64().ok_or_else(|| anyhow!("Invalid watch kind")))
        .transpose()?
        .unwrap_or(7);
    if kind == 0 || kind & !7 != 0 {
        bail!("Invalid watch kind");
    }
    let value = &watcher["globPattern"];
    let (base, glob) = if let Some(glob) = value.as_str() {
        (root.to_path_buf(), glob)
    } else {
        let uri = value["baseUri"]
            .as_str()
            .or_else(|| value["baseUri"]["uri"].as_str())
            .ok_or_else(|| anyhow!("Missing relative watch base URI"))?;
        let base = super::document::uri_path(uri)
            .map_err(|_| anyhow!("Watcher base URI must be a local file"))?;
        validate_absolute(&base)?;
        if !base.starts_with(root) {
            return Ok(None);
        }
        (
            base,
            value["pattern"]
                .as_str()
                .ok_or_else(|| anyhow!("Missing relative watch pattern"))?,
        )
    };
    if glob.split('/').any(|part| part == "..") {
        bail!("File watcher pattern leaves its base");
    }
    let absolute = glob.starts_with('/');
    let glob = GlobBuilder::new(glob)
        .literal_separator(true)
        .backslash_escape(true)
        .build()?
        .compile_matcher();
    Ok(Some(Pattern {
        base,
        glob,
        absolute,
        kind,
    }))
}

/// The shared worktree walk, plus each registered base that walk skipped as ignored.
fn snapshot(
    root: &Path,
    registrations: &Registrations,
    fresh: bool,
    stop: &AtomicBool,
) -> Result<Vec<Arc<Tree>>> {
    let tree = shared_walk(root, fresh, stop)?;
    let real_root = std::fs::canonicalize(root)?;
    let bases: HashSet<_> = registrations
        .values()
        .flat_map(|(_, patterns, _)| patterns.iter().map(|p| p.base.as_path()))
        .filter(|base| !tree.dirs.contains(*base))
        // A base reached through a symlink would lead the walk out of the worktree.
        .filter(|base| {
            base.strip_prefix(root).is_ok_and(|relative| {
                std::fs::canonicalize(base).is_ok_and(|real| real == real_root.join(relative))
            })
        })
        .collect();
    let mut trees = vec![tree];
    for base in bases {
        // The server asked for this base explicitly, so its ignore files do not apply.
        match walk(base, false, stop) {
            Ok(tree) => trees.push(Arc::new(tree)),
            // A base that does not exist (yet), such as a build script's OUT_DIR, has nothing to watch.
            Err(_) if !base.is_dir() => {}
            // Dropping it silently would report every file under it as deleted.
            Err(error) => return Err(error),
        }
    }
    Ok(trees)
}

// ponytail: one lock serializes walks of different worktrees; lock per root if that shows up.
static RECENT: Mutex<Vec<(PathBuf, Instant, Arc<Tree>)>> = Mutex::new(Vec::new());

/// Watchers of one worktree (a server and its companion, or several sessions) share a walk per poll.
fn shared_walk(root: &Path, fresh: bool, stop: &AtomicBool) -> Result<Arc<Tree>> {
    let mut recent = RECENT.lock().unwrap_or_else(|poison| poison.into_inner());
    recent.retain(|(path, started, _)| !(fresh && path == root) && started.elapsed() < POLL);
    if let Some((_, _, tree)) = recent.iter().find(|(path, ..)| path == root) {
        return Ok(tree.clone());
    }
    let started = Instant::now();
    let tree = Arc::new(walk(root, true, stop)?);
    recent.push((root.to_path_buf(), started, tree.clone()));
    Ok(tree)
}

fn walk(base: &Path, ignores: bool, stop: &AtomicBool) -> Result<Tree> {
    // Only the base must be readable; descendants can vanish or deny access during normal use.
    std::fs::read_dir(base)?;
    let mut tree = Tree::default();
    // Git-ignored build and dependency trees would dominate the walk. Symlinks are not followed,
    // and repository internals do not belong to the language workspace.
    let entries = ignore::WalkBuilder::new(base)
        .standard_filters(ignores)
        .hidden(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    for (visited, entry) in entries.enumerate() {
        if stop.load(Ordering::Relaxed) {
            bail!("File watching stopped");
        }
        // shortcut: polling is capped at 200k entries; use native events if large-worktree measurements require it.
        if visited > 200_000 {
            bail!(
                "Language file watching exceeded 200,000 entries in {}",
                base.display()
            );
        }
        let Ok(entry) = entry else { continue };
        match entry.file_type() {
            Some(kind) if kind.is_dir() => {
                tree.dirs.insert(entry.into_path());
            }
            Some(kind) if kind.is_file() => {
                if let Ok(metadata) = entry.metadata() {
                    tree.files.insert(
                        entry.into_path(),
                        (metadata.len(), metadata.modified().ok()),
                    );
                }
            }
            _ => {}
        }
    }
    Ok(tree)
}

/// Records each registration's new baseline and returns the changed paths with their watch-kind
/// bit (create 1, change 2, delete 4), for registrations whose patterns subscribe to it.
fn poll_changes(
    registrations: &Registrations,
    previous: &mut HashMap<u64, Snapshot>,
    trees: &[Arc<Tree>],
    inherited: &HashMap<u64, &Vec<Pattern>>,
) -> HashMap<PathBuf, u32> {
    let mut changes = HashMap::new();
    for (key, patterns, _) in registrations.values() {
        let current: Snapshot = trees
            .iter()
            .flat_map(|tree| &tree.files)
            .filter(|(path, _)| patterns.iter().any(|p| p.matches(path)))
            .map(|(path, stamp)| (path.clone(), *stamp))
            .collect();
        if let Some(mut old) = previous.insert(*key, current.clone()) {
            // Files only the new patterns cover have no old stamp; they existed, they were not created.
            for (path, stamp) in &current {
                if inherited
                    .get(key)
                    .is_some_and(|ps| !ps.iter().any(|p| p.matches(path)))
                {
                    old.entry(path.clone()).or_insert(*stamp);
                }
            }
            for (path, bit) in diff(&old, &current) {
                if patterns
                    .iter()
                    .any(|p| p.kind & u64::from(bit) != 0 && p.matches(&path))
                {
                    changes.insert(path, bit);
                }
            }
        }
    }
    changes
}

fn diff(before: &Snapshot, after: &Snapshot) -> Vec<(PathBuf, u32)> {
    let mut changes = Vec::new();
    for (path, stamp) in after {
        match before.get(path) {
            None => changes.push((path.clone(), 1)),
            Some(old) if old != stamp => changes.push((path.clone(), 2)),
            _ => {}
        }
    }
    changes.extend(
        before
            .keys()
            .filter(|path| !after.contains_key(*path))
            .map(|path| (path.clone(), 4)),
    );
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::test_support;

    #[test]
    fn globs_respect_relative_roots_kinds_and_worktree_boundaries() {
        let root = Path::new("/work/project");
        let p = pattern(root, &json!({"globPattern":"**/*.{rs,toml}","kind":3}))
            .unwrap()
            .unwrap();
        assert!(p.matches(Path::new("/work/project/src/lib.rs")));
        assert!(p.matches(Path::new("/work/project/Cargo.toml")));
        assert!(!p.matches(Path::new("/work/other/src/lib.rs")));
        assert!(!p.matches(Path::new("/work/project/src/main.py")));
        let p = pattern(
            root,
            &json!({"globPattern":{"baseUri":"file:///work/project/src","pattern":"*.rs"}}),
        )
        .unwrap()
        .unwrap();
        assert!(p.matches(Path::new("/work/project/src/lib.rs")));
        assert!(!p.matches(Path::new("/work/project/src/nested/lib.rs")));
        assert!(
            pattern(
                root,
                &json!({"globPattern":{"baseUri":"file:///work/other","pattern":"**"}})
            )
            .unwrap()
            .is_none()
        );
        assert!(pattern(root, &json!({"globPattern":"../*"})).is_err());
        assert!(pattern(root, &json!({"globPattern":"**","kind":8})).is_err());
    }

    #[test]
    fn snapshots_detect_create_change_and_delete() {
        let before = HashMap::from([
            (PathBuf::from("same"), (1, None)),
            (PathBuf::from("changed"), (2, None)),
            (PathBuf::from("deleted"), (1, None)),
        ]);
        let after = HashMap::from([
            (PathBuf::from("same"), (1, None)),
            (PathBuf::from("changed"), (3, None)),
            (PathBuf::from("created"), (1, None)),
        ]);
        let changes: HashMap<_, _> = diff(&before, &after).into_iter().collect();
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[Path::new("created")], 1);
        assert_eq!(changes[Path::new("changed")], 2);
        assert_eq!(changes[Path::new("deleted")], 4);
    }

    #[test]
    fn polling_does_not_follow_symlinks() {
        let root = std::env::temp_dir().join(format!("shark-watch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("file.rs"), "hello").unwrap();
        std::os::unix::fs::symlink("/", root.join("outside")).unwrap();
        let files = walk(&root, true, &AtomicBool::new(false)).unwrap().files;
        assert_eq!(files.len(), 1);
        assert!(files.contains_key(&root.join("file.rs")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn polling_skips_ignored_trees_unless_registered_and_only_fails_on_the_root() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("shark-watch-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(root.with_extension("outside"));
        for dir in [
            ".git",
            "src",
            "target/debug",
            "target/out",
            "locked",
            ".venv/lib",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        // uv and `python -m venv` mark the whole environment ignored from inside it.
        std::fs::write(root.join(".venv/.gitignore"), "*\n").unwrap();
        for file in [
            "src/lib.rs",
            "target/debug/dep.rs",
            "target/out/gen.rs",
            "locked/a.rs",
            ".venv/lib/site.py",
        ] {
            std::fs::write(root.join(file), "").unwrap();
        }
        let outside = root.with_extension("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.rs"), "").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
        let locked = root.join("locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let stop = AtomicBool::new(false);
        let files = walk(&root, true, &stop).unwrap().files;
        assert!(files.contains_key(&root.join("src/lib.rs")));
        assert!(
            files
                .keys()
                .all(|path| !path.starts_with(root.join("target")))
        );
        let out = url::Url::from_directory_path(root.join("target/out")).unwrap();
        let registration = json!({"globPattern":{"baseUri":out.as_str(),"pattern":"**/*.rs"}});
        let venv = url::Url::from_directory_path(root.join(".venv/lib")).unwrap();
        let venv = json!({"globPattern":{"baseUri":venv.as_str(),"pattern":"*.py"}});
        let linked = url::Url::from_directory_path(root.join("linked")).unwrap();
        let linked = json!({"globPattern":{"baseUri":linked.as_str(),"pattern":"*.rs"}});
        let registrations = Registrations::from([(
            "out".to_string(),
            (
                0,
                [registration, venv, linked]
                    .iter()
                    .map(|watcher| pattern(&root, watcher).unwrap().unwrap())
                    .collect(),
                None,
            ),
        )]);
        let files: Vec<_> = snapshot(&root, &registrations, true, &stop)
            .unwrap()
            .iter()
            .flat_map(|tree| tree.files.keys().cloned().collect::<Vec<_>>())
            .collect();
        assert!(files.contains(&root.join("target/out/gen.rs")));
        assert!(files.contains(&root.join(".venv/lib/site.py")));
        assert!(!files.contains(&root.join("target/debug/dep.rs")));
        assert!(!files.iter().any(|path| path.ends_with("secret.rs")));
        // A primary server and its companion reuse one recent walk of their worktree.
        let shared = shared_walk(&root, false, &stop).unwrap();
        assert!(Arc::ptr_eq(
            &shared,
            &shared_walk(&root, false, &stop).unwrap()
        ));
        assert!(!Arc::ptr_eq(
            &shared,
            &shared_walk(&root, true, &stop).unwrap()
        ));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
        assert!(walk(&root, true, &stop).is_err());
    }

    #[test]
    fn registration_batches_are_atomic_and_unregistration_is_validated() {
        let root = std::env::temp_dir().join(format!("shark-watch-batch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let client = Client::start(
            std::process::Command::new("/usr/bin/true"),
            &root,
            Value::Null,
        )
        .unwrap();
        let watcher = Watcher::new(&root, client).unwrap();
        let registration = |id: &str| json!({"id":id,"method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[{"globPattern":"**/*.rs"}]}});
        assert!(
            watcher
                .register(&json!({"registrations":[registration("one"),registration("one")]}))
                .is_err()
        );
        assert!(watcher.registrations.lock().unwrap().is_empty());
        watcher
            .register(&json!({"registrations":[registration("one")]}))
            .unwrap();
        let first = watcher.registrations.lock().unwrap()["one"].0;
        // Registering an existing id again replaces its watchers; bases outside the worktree are skipped.
        let outside = json!({"id":"two","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[{"globPattern":{"baseUri":"file:///elsewhere","pattern":"**"}},{"globPattern":"**/*.rs"}]}});
        watcher
            .register(&json!({"registrations":[outside,registration("one")]}))
            .unwrap();
        assert_eq!(watcher.registrations.lock().unwrap()["two"].1.len(), 1);
        assert_ne!(watcher.registrations.lock().unwrap()["one"].0, first);
        assert!(
            watcher
                .register(&json!({"registrations":[{"id":"bad","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[{"globPattern":"**","kind":8}]}}]}))
                .is_err()
        );
        assert_eq!(watcher.registrations.lock().unwrap().len(), 2);
        assert!(watcher.unregister(&json!({"unregisterations":[{"id":"one","method":"workspace/didChangeWatchedFiles"},{"id":"missing","method":"workspace/didChangeWatchedFiles"}]})).is_err());
        assert_eq!(watcher.registrations.lock().unwrap().len(), 2);
        watcher.unregister(&json!({"unregisterations":[{"id":"one","method":"workspace/didChangeWatchedFiles"},{"id":"two","method":"workspace/didChangeWatchedFiles"}]})).unwrap();
        assert!(watcher.registrations.lock().unwrap().is_empty());
        drop(watcher);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changes_after_registration_acknowledgement_are_observed() {
        let root = std::env::temp_dir().join(format!("shark-watch-ready-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("file.rs");
        let log = root.join("peer.jsonl");
        std::fs::write(&file, "old").unwrap();
        let client = Client::start(test_support::fake_server(&log), &root, Value::Null).unwrap();
        let watcher = Watcher::new(&root, client.clone()).unwrap();
        let ready = watcher.register(&json!({"registrations":[{"id":"source","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[{"globPattern":"**/*.rs","kind":2}]}}]})).unwrap();
        ready.recv_blocking().unwrap().unwrap();
        std::fs::write(&file, "changed immediately after ack").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let found = test_support::logged(&log).iter().any(|message| {
                message["method"] == "workspace/didChangeWatchedFiles"
                    && message["params"]["changes"]
                        .as_array()
                        .is_some_and(|changes| {
                            changes.iter().any(|change| {
                                change["type"] == 2
                                    && change["uri"]
                                        == url::Url::from_file_path(&file).unwrap().as_str()
                            })
                        })
            });
            if found {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "watcher missed the first change after acknowledgement"
            );
            thread::sleep(Duration::from_millis(20));
        }
        drop(watcher);
        client.terminate();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn re_registering_an_id_keeps_the_baseline_of_the_one_it_replaces() {
        let root = std::env::temp_dir().join(format!("shark-watch-rereg-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let (file, other) = (root.join("file.rs"), root.join("Cargo.toml"));
        let log = root.join("peer.jsonl");
        std::fs::write(&file, "old").unwrap();
        std::fs::write(&other, "old").unwrap();
        let client = Client::start(test_support::fake_server(&log), &root, Value::Null).unwrap();
        let watcher = Watcher::new(&root, client.clone()).unwrap();
        let register = |glob: &str| {
            watcher.register(&json!({"registrations":[{"id":"source","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[{"globPattern":glob}]}}]})).unwrap()
        };
        register("**/*.rs").recv_blocking().unwrap().unwrap();
        // Changed after the first baseline, before the replacement's own would be taken.
        std::fs::write(&file, "changed between registrations").unwrap();
        register("**/*.{rs,toml}").recv_blocking().unwrap().unwrap();
        let uri = |path: &Path| url::Url::from_file_path(path).unwrap().to_string();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let changes = loop {
            let found = test_support::logged(&log)
                .into_iter()
                .filter(|m| m["method"] == "workspace/didChangeWatchedFiles")
                .filter_map(|m| m["params"]["changes"].as_array().cloned())
                .find(|c| c.iter().any(|c| c["uri"] == uri(&file).as_str()));
            if let Some(found) = found {
                break found;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the change between registrations was missed"
            );
            thread::sleep(Duration::from_millis(20));
        };
        // Cargo.toml only became covered by the new patterns; it existed, it was not created.
        assert!(changes.iter().all(|c| c["uri"] != uri(&other).as_str()));
        drop(watcher);
        client.terminate();
        let _ = std::fs::remove_dir_all(root);
    }
}

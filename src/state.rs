use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct State {
    #[serde(default, deserialize_with = "tolerant_lists")]
    pub reviewed: HashMap<String, Vec<String>>,
    #[serde(default, deserialize_with = "tolerant_lists")]
    pub merge_order: HashMap<String, Vec<String>>,
    #[serde(default, deserialize_with = "tolerant_bool")]
    pub delete_remote_branches: bool,
    #[serde(skip)]
    pub save_error: std::cell::RefCell<Option<String>>,
    pub repos: Vec<Repo>,
    pub workspaces: Vec<Workspace>,
    /// Vim keys in the file editor.
    #[serde(default)]
    pub vim: bool,
    /// Repo names picked for the last new workspace; the next dialog starts with them.
    #[serde(default)]
    pub last_repos: Vec<String>,
    /// Applications opened through Shark, most recent first, shared by all workspaces.
    #[serde(default)]
    pub recent_apps: Vec<PathBuf>,
    /// The workspace and tab (`chat`/`diff`/`git`, or `home` while Home was shown over it) open at
    /// quit; restored on launch.
    #[serde(default)]
    pub last_open: Option<(String, String)>,
    #[serde(default)]
    pub appearance: Appearance,
    /// Edited git-button prompts by id (`prompts::PROMPTS`); missing ids use the default.
    #[serde(default)]
    pub prompts: BTreeMap<String, String>,
    /// Sidebar groups of workspaces, in display order; ungrouped workspaces follow them.
    #[serde(default)]
    pub sections: Vec<Section>,
    /// Keep the Mac from idle-sleeping while an agent works (`Shark::update_awake`).
    #[serde(default)]
    pub stay_awake: bool,
    /// Where `load` moved an unreadable state.json, for the launch notice.
    #[serde(skip)]
    pub corrupt_backup: Option<PathBuf>,
    /// state.json couldn't be read or moved aside: `save` leaves it alone this run.
    #[serde(skip)]
    pub read_only: bool,
    /// Create PRs succeeded at least once; the Home checklist ticks it off.
    #[serde(default)]
    pub opened_pr: bool,
    /// A plan was approved at least once; the Home checklist ticks it off.
    #[serde(default)]
    pub approved_plan: bool,
    /// The Home "Get started" checklist was dismissed.
    #[serde(default)]
    pub home_checklist_dismissed: bool,
    /// The palette was opened at least once; Home stops pointing at cmd-k.
    #[serde(default)]
    pub palette_used: bool,
    /// The app version Home last told the user about; empty before the first launch.
    #[serde(default)]
    pub last_seen_version: String,
    /// The model and thinking level last picked in any chat; new chats start on them.
    #[serde(default)]
    pub last_model: Option<String>,
    #[serde(default)]
    pub last_effort: Option<String>,
    /// Changes shows one column instead of Original | Working copy (`s`).
    #[serde(default)]
    pub unified: bool,
    /// Compare the working tree to HEAD instead of the workspace base.
    #[serde(default)]
    pub uncommitted_only: bool,
    /// The Files panel's width, once dragged.
    #[serde(default)]
    pub tree_width: Option<f32>,
    /// The expanded sidebar's width, once dragged.
    #[serde(default)]
    pub sidebar_width: Option<f32>,
    /// The original column's share of a Split diff, the same for every file; `None` is half.
    #[serde(default)]
    pub split_ratio: Option<f32>,
    /// Per-repo settings by repo name (the repo settings dialog); missing names use defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repo_settings: BTreeMap<String, RepoSettings>,
}

/// A repo's defaults and scripts. Scripts are shell commands run in its worktree, in order.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
#[serde(default)]
pub struct RepoSettings {
    /// Model and thinking level new chats start on in workspaces with this repo.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// After its worktree is created.
    pub setup: Vec<String>,
    /// cmd-r in a workspace with this repo; cmd-r again stops them.
    pub run: Vec<String>,
    /// Before its worktree is removed.
    pub archive: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Section {
    pub name: String,
    /// Lucide icon name from `app::sidebar::SECTION_ICONS`, e.g. `folder`.
    pub icon: String,
    #[serde(default)]
    pub collapsed: bool,
    /// Workspace names, in the order they were added.
    #[serde(default)]
    pub workspaces: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Default, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    System,
    Light,
    Dark,
}

/// Theme families by name (`settings::THEMES`); fonts `None` use the theme's.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Appearance {
    pub mode: Mode,
    pub light: String,
    pub dark: String,
    pub ui_font: Option<String>,
    pub mono_font: Option<String>,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            mode: Mode::System,
            light: "Linear".into(),
            dark: "Linear".into(),
            ui_font: None,
            mono_font: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Workspace {
    /// Last explicit user interaction, in Unix milliseconds; background work never updates it.
    #[serde(default)]
    pub last_interacted_at: Option<u64>,
    pub name: String,
    pub branch: String,
    pub repos: Vec<WsRepo>,
    /// Pre-tabs single chat; `migrate` moves it into `chats`.
    #[serde(default, skip_serializing)]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing)]
    pub model: Option<String>,
    /// Open chat tabs, left to right; never empty after `migrate`.
    #[serde(default)]
    pub chats: Vec<ChatMeta>,
    /// Closed tabs, newest last, so cmd-shift-t can bring one back with its transcript.
    #[serde(default)]
    pub closed: Vec<ChatMeta>,
    #[serde(default)]
    pub active_chat: usize,
    /// Closed by default, until an explicit Diff action opens it.
    #[serde(default = "diff_closed_by_default")]
    pub diff_closed: bool,
    /// Closed until the Git feature is explicitly opened.
    #[serde(default = "diff_closed_by_default")]
    pub git_closed: bool,
    /// Diff path -> fingerprint of its patch when marked viewed; a new patch makes it unviewed.
    #[serde(default)]
    pub viewed: HashMap<String, u64>,
    /// The tab strip, left to right. File entries are also the files reopened with the
    /// workspace; `normalize_tabs` keeps chat and feature tabs in step with their open/closed state.
    #[serde(default)]
    pub tab_order: Vec<TabId>,
}

/// A tab's identity in the strip, stable across reordering.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Hash, Debug)]
#[serde(rename_all = "lowercase")]
pub enum TabId {
    Chat(String),
    Diff,
    Git,
    /// `repo/rel/path`, as in the diff.
    File(String),
}

fn diff_closed_by_default() -> bool {
    true
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            last_interacted_at: None,
            name: String::new(),
            branch: String::new(),
            repos: Vec::new(),
            session_id: None,
            model: None,
            chats: Vec::new(),
            closed: Vec::new(),
            active_chat: 0,
            diff_closed: diff_closed_by_default(),
            git_closed: true,
            viewed: HashMap::new(),
            tab_order: Vec::new(),
        }
    }
}

/// One conversation: its own claude process, session and transcript.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
pub struct ChatMeta {
    /// Transcript and log file name. The first chat's id is the workspace name, which is
    /// where the single pre-tabs transcript already lives.
    pub id: String,
    /// First line of the first message; empty until then.
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Claude model alias (`opus`, `fable`, …) or `codex:<model id>`. Only chats saved before
    /// every chat named one have `None` (the CLI default); `Workspace::fill_models` gives
    /// them one.
    #[serde(default)]
    pub model: Option<String>,
    /// Thinking level (`chat::efforts`); `None` until one is picked (`chat::effort_of`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Plan mode: the agent plans and edits nothing until the plan is approved.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan: bool,
    /// codex's Fast service tier, once toggled; `None` follows codex's own config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct WsRepo {
    pub name: String,
    pub src: PathBuf,
    /// Ref the branch was cut from, e.g. `origin/main`.
    pub base: String,
    /// Its own branch; set only when it differs from the workspace branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// Moves item `from` into slot `to`; the ones between shift over.
pub fn move_item<T>(v: &mut Vec<T>, from: usize, to: usize) {
    let x = v.remove(from);
    v.insert(to, x);
}

/// Lets tests redirect app data without mutating the process environment.
#[cfg(test)]
pub static TEST_HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub(crate) fn home() -> PathBuf {
    #[cfg(test)]
    if let Some(h) = TEST_HOME.get() {
        return h.clone();
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

pub fn support_dir() -> PathBuf {
    home().join("Library/Application Support/shark")
}

pub fn workspaces_dir() -> PathBuf {
    home().join("shark/workspaces")
}

impl Workspace {
    pub fn dir(&self) -> PathBuf {
        workspaces_dir().join(&self.name)
    }

    /// The branch `r`'s worktree is on: its own, or the workspace's.
    pub fn branch_of<'a>(&'a self, r: &'a WsRepo) -> &'a str {
        r.branch.as_deref().unwrap_or(&self.branch)
    }

    /// Gives a workspace saved before chat tabs (or a new one) its first chat.
    pub fn migrate(&mut self) {
        if self.chats.is_empty() {
            self.chats.push(ChatMeta {
                id: self.name.clone(),
                session_id: self.session_id.take(),
                model: self.model.take(),
                ..Default::default()
            });
        }
        self.active_chat = self.active_chat.min(self.chats.len() - 1);
        self.normalize_tabs();
    }

    /// Drops duplicate and stale entries, then appends open chats and feature tabs
    /// the strip lacks: a workspace saved before `tab_order` gets its chats, then features. Open
    /// chats follow the strip's order, so chat-only cycling matches it.
    pub fn normalize_tabs(&mut self) {
        let mut seen = HashSet::new();
        let (chats, diff, git) = (&self.chats, !self.diff_closed, !self.git_closed);
        self.tab_order.retain(|t| {
            let live = match t {
                TabId::Chat(id) => chats.iter().any(|c| &c.id == id),
                TabId::Diff => diff,
                TabId::Git => git,
                TabId::File(_) => true,
            };
            live && seen.insert(t.clone())
        });
        let missing: Vec<TabId> = (self.chats.iter())
            .map(|c| TabId::Chat(c.id.clone()))
            .chain(diff.then_some(TabId::Diff))
            .chain(git.then_some(TabId::Git))
            .filter(|t| !seen.contains(t))
            .collect();
        self.tab_order.extend(missing);
        let active = self.chats.get(self.active_chat).map(|c| c.id.clone());
        let order = &self.tab_order;
        self.chats.sort_by_key(|c| {
            (order.iter()).position(|t| matches!(t, TabId::Chat(id) if *id == c.id))
        });
        if let Some(ix) = active.and_then(|a| self.chats.iter().position(|c| c.id == a)) {
            self.active_chat = ix;
        }
    }

    /// Appends `tab`, moving it to the end when it is already listed.
    pub fn open_tab(&mut self, tab: TabId) {
        self.tab_order.retain(|t| *t != tab);
        self.tab_order.push(tab);
    }

    pub fn close_tab(&mut self, tab: &TabId) {
        self.tab_order.retain(|t| t != tab);
    }

    /// `new` takes `old`'s slot (a replaced file), or the end when `old` isn't listed.
    pub fn replace_tab(&mut self, old: &TabId, new: TabId) {
        if old == &new {
            return;
        }
        self.tab_order.retain(|t| *t != new);
        match self.tab_order.iter().position(|t| t == old) {
            Some(ix) => self.tab_order[ix] = new,
            None => self.tab_order.push(new),
        }
    }

    /// Moves tab `from` into `to`'s slot; false when they are the same or either is missing.
    pub fn move_tab(&mut self, from: &TabId, to: &TabId) -> bool {
        let pos = |t: &TabId| self.tab_order.iter().position(|x| x == t);
        let (Some(from), Some(to)) = (pos(from), pos(to)) else {
            return false;
        };
        if from == to {
            return false;
        }
        move_item(&mut self.tab_order, from, to);
        self.normalize_tabs();
        true
    }

    /// The saved file tabs, left to right.
    pub fn file_tabs(&self) -> impl Iterator<Item = &String> {
        self.tab_order.iter().filter_map(|t| match t {
            TabId::File(path) => Some(path),
            _ => None,
        })
    }

    /// Chats without a model (an old one on the CLI default, a new workspace's first) get
    /// `model` and `effort`; old ones on codex's default get the model it was.
    pub fn fill_models(&mut self, model: &str, effort: Option<&String>) {
        for c in self.chats.iter_mut().chain(&mut self.closed) {
            match c.model.as_deref() {
                None => {
                    // A claude session can't resume on codex.
                    let claude = c.session_id.is_some() && crate::codex::is_codex(Some(model));
                    c.model = Some(
                        if claude {
                            crate::chat::MODELS[0]
                        } else {
                            model
                        }
                        .into(),
                    );
                    c.effort = c.effort.take().or(effort.cloned());
                }
                Some("codex") => c.model = Some("codex:gpt-6.1-sol".into()),
                _ => {}
            }
        }
    }

    /// A fresh chat id, unique among open and closed tabs: `<ws>~2`, `<ws>~3`, …
    pub fn next_chat_id(&self) -> String {
        let n = (self.chats.iter().chain(&self.closed))
            .filter_map(|c| c.id.rsplit_once('~')?.1.parse::<u32>().ok())
            .max()
            .unwrap_or(1);
        format!("{}~{}", self.name, n + 1)
    }
}

fn state_file() -> PathBuf {
    support_dir().join("state.json")
}

fn tolerant_lists<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<HashMap<String, Vec<String>>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(v.as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| {
            serde_json::from_value(v.clone())
                .ok()
                .map(|v| (k.clone(), v))
        })
        .collect())
}

fn tolerant_bool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(Value::deserialize(d)?.as_bool().unwrap_or(false))
}

pub(crate) fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::{
        io::Write,
        os::unix::fs::OpenOptionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let (tmp, mut file) = loop {
        let tmp = path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
        {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = file
        .write_all(bytes)
        .and_then(|_| file.sync_all())
        .and_then(|_| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

impl State {
    pub fn load() -> State {
        Self::load_at(&state_file())
    }

    fn load_at(path: &Path) -> State {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return State::default(),
            // Saving would replace a file we never read.
            Err(_) => {
                return State {
                    read_only: true,
                    ..State::default()
                };
            }
        };
        let mut state: State = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            // Keep the unreadable file instead of overwriting it on the next save; numbered so
            // a later corruption doesn't replace an earlier backup.
            let bak = (0..)
                .map(|n| path.with_extension(format!("json.bak-{n}")))
                .find(|p| !p.exists())
                .unwrap();
            let moved = fs::rename(path, &bak);
            eprintln!(
                "state.json unreadable ({e}), moved to {}: {moved:?}",
                bak.display()
            );
            State {
                read_only: moved.is_err(),
                corrupt_backup: moved.ok().map(|_| bak),
                ..State::default()
            }
        });
        let (model, effort) = (state.new_model(), state.last_effort.clone());
        for ws in &mut state.workspaces {
            ws.migrate();
            ws.fill_models(&model, effort.as_ref());
        }
        state
    }

    /// The model and level a new chat in `ws` starts on: the first of its repos with a default,
    /// else the last picked.
    pub fn defaults_for(&self, ws: &Workspace) -> (String, Option<String>) {
        let valid = |m: &String| crate::chat::MODELS.contains(&m.as_str());
        let s = (ws.repos.iter())
            .filter_map(|r| self.repo_settings.get(&r.name))
            .find(|s| s.model.as_ref().is_some_and(valid) || s.effort.is_some());
        let model = s.and_then(|s| s.model.clone().filter(valid));
        let effort = s.and_then(|s| s.effort.clone());
        (
            model.unwrap_or_else(|| self.new_model()),
            effort.or(self.last_effort.clone()),
        )
    }

    /// The model a new chat gets: the last one picked, else the first.
    pub fn new_model(&self) -> String {
        let models = crate::chat::MODELS;
        let last = self.last_model.as_deref().filter(|m| models.contains(m));
        last.unwrap_or(models[0]).into()
    }

    /// A new chat `id` in `ws` on its repos' default model, else the last picked.
    pub fn new_chat(&self, ws: &Workspace, id: String) -> ChatMeta {
        let (model, effort) = self.defaults_for(ws);
        ChatMeta {
            id,
            model: Some(model),
            effort,
            ..Default::default()
        }
    }

    /// Drag-and-drop of workspace `ws` onto workspace `target`: `ws` takes the target's
    /// slot in the target's section (or among the ungrouped), like a list reorder.
    pub fn drop_ws(&mut self, ws: &str, target: &str) {
        let section_of = |st: &State, n: &str| {
            st.sections
                .iter()
                .position(|s| s.workspaces.iter().any(|w| w == n))
        };
        let (from, to) = (section_of(self, ws), section_of(self, target));
        if ws == target {
            return;
        }
        let pos = |v: &[String], n: &str| v.iter().position(|w| w == n);
        match to {
            Some(i) if from == to => {
                let v = &mut self.sections[i].workspaces;
                let (f, t) = (pos(v, ws).unwrap(), pos(v, target).unwrap());
                move_item(v, f, t);
            }
            Some(i) => {
                for s in &mut self.sections {
                    s.workspaces.retain(|w| w != ws);
                }
                let v = &mut self.sections[i].workspaces;
                let t = pos(v, target).unwrap();
                v.insert(t, ws.to_string());
            }
            None => {
                for s in &mut self.sections {
                    s.workspaces.retain(|w| w != ws);
                }
                let v = &self.workspaces;
                let f = v.iter().position(|w| w.name == ws).unwrap();
                let t = v.iter().position(|w| w.name == target).unwrap();
                // Coming from a section it lands before the target; within the list it takes the slot.
                let t = match from {
                    Some(_) if f < t => t - 1,
                    _ => t,
                };
                move_item(&mut self.workspaces, f, t);
            }
        }
    }

    /// Sets a once-only flag, saving only when that changes it.
    pub fn mark(&mut self, flag: impl FnOnce(&mut State) -> &mut bool) {
        if !std::mem::replace(flag(self), true) {
            let _ = self.save();
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_at(&state_file())
    }

    fn save_at(&self, path: &Path) -> std::io::Result<()> {
        let result = if self.read_only {
            Err(std::io::Error::other(
                "state.json could not be read; the original store is protected",
            ))
        } else {
            serde_json::to_vec_pretty(self)
                .map_err(std::io::Error::other)
                .and_then(|bytes| write(path, &bytes))
        };
        *self.save_error.borrow_mut() = result.as_ref().err().map(ToString::to_string);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_apps_default_and_round_trip() {
        let mut state: State = serde_json::from_str(r#"{"repos":[],"workspaces":[]}"#).unwrap();
        assert!(state.recent_apps.is_empty());
        state.recent_apps = vec![
            "/Applications/Code.app".into(),
            "/Applications/Terminal.app".into(),
        ];
        let saved = serde_json::to_string(&state).unwrap();
        assert_eq!(
            serde_json::from_str::<State>(&saved).unwrap().recent_apps,
            state.recent_apps
        );
    }

    /// A repo saved before per-repo branches is on the workspace branch, and one on the
    /// workspace branch doesn't save the field.
    #[test]
    fn repo_branch_is_optional() {
        let old = r#"{"name":"w","branch":"feat","repos":[{"name":"api","src":"/x","base":"origin/main"}]}"#;
        let mut ws: Workspace = serde_json::from_str(old).unwrap();
        assert_eq!(ws.repos[0].branch, None);
        assert_eq!(ws.branch_of(&ws.repos[0]), "feat");
        assert!(
            !serde_json::to_string(&ws)
                .unwrap()
                .contains("\"branch\":null")
        );
        ws.repos[0].branch = Some("kd/x".into());
        let back: Workspace = serde_json::from_str(&serde_json::to_string(&ws).unwrap()).unwrap();
        assert_eq!(back.branch_of(&back.repos[0]), "kd/x");
    }

    /// Old workspaces get their chats then a shown Diff; the saved strip mixes kinds, round
    /// trips per workspace, and drops duplicates and stale entries without inventing files.
    #[test]
    fn tab_order_migrates_normalizes_and_moves() {
        use TabId::{Chat, Diff, File};
        let chat = |id: &str| Chat(id.into());
        let file = |p: &str| File(p.into());
        let old = r#"{"name":"w","branch":"b","repos":[],"diff_closed":false,
            "chats":[{"id":"w"},{"id":"w~2"}],"active_chat":1}"#;
        let mut ws: Workspace = serde_json::from_str(old).unwrap();
        ws.migrate();
        assert_eq!(ws.tab_order, [chat("w"), chat("w~2"), Diff]);

        ws.open_tab(file("api/a.rs"));
        ws.open_tab(file("api/b.rs"));
        let unchanged = ws.tab_order.clone();
        ws.replace_tab(&file("api/a.rs"), file("api/a.rs"));
        assert_eq!(ws.tab_order, unchanged);
        ws.replace_tab(&file("api/a.rs"), file("api/c.rs"));
        assert_eq!(
            ws.tab_order,
            [
                chat("w"),
                chat("w~2"),
                Diff,
                file("api/c.rs"),
                file("api/b.rs")
            ]
        );
        // A file into the first chat's slot, then a chat past Diff: chats follow, the same
        // chat stays selected.
        assert!(ws.move_tab(&file("api/b.rs"), &chat("w")));
        assert!(ws.move_tab(&chat("w~2"), &Diff));
        assert!(!ws.move_tab(&Diff, &Diff));
        assert!(!ws.move_tab(&chat("gone"), &Diff));
        let order = [
            file("api/b.rs"),
            chat("w"),
            Diff,
            chat("w~2"),
            file("api/c.rs"),
        ];
        assert_eq!(ws.tab_order, order);
        assert_eq!(ws.chats[ws.active_chat].id, "w~2");
        ws.close_tab(&file("api/c.rs"));
        ws.open_tab(file("api/b.rs"));
        assert_eq!(ws.file_tabs().collect::<Vec<_>>(), ["api/b.rs"]);

        let mut other = Workspace {
            name: "x".into(),
            ..Default::default()
        };
        other.migrate();
        let st = State {
            workspaces: vec![ws.clone(), other],
            ..Default::default()
        };
        let back: State = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
        assert_eq!(back.workspaces[0].tab_order, ws.tab_order);
        assert_eq!(back.workspaces[1].tab_order, [chat("x")]);

        // Hand-edited or stale: duplicates, a closed chat, a hidden Diff, a missing chat.
        ws.tab_order = vec![chat("w~2"), chat("closed"), Diff, file("f"), chat("w~2")];
        ws.diff_closed = true;
        ws.normalize_tabs();
        assert_eq!(ws.tab_order, [chat("w~2"), file("f"), chat("w")]);
        assert_eq!(
            ws.chats.iter().map(|c| &c.id[..]).collect::<Vec<_>>(),
            ["w~2", "w"]
        );
        assert_eq!(ws.chats[ws.active_chat].id, "w~2");
    }

    #[test]
    fn corrupt_state_is_kept() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("shark-corrupt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let baks = || {
            let mut v: Vec<String> = (fs::read_dir(&dir).unwrap())
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("state.json.bak-"))
                .collect();
            v.sort();
            v
        };
        fs::write(&path, "{ first").unwrap();
        let st = State::load_at(&path);
        let first = st.corrupt_backup.clone().unwrap();
        assert!(!st.read_only && !path.exists());
        assert_eq!(baks().len(), 1);
        st.save_at(&path).unwrap();
        assert!(path.exists(), "a moved-aside file is saved over");

        // Same second: the first backup stays.
        fs::write(&path, "{ second").unwrap();
        let second = State::load_at(&path).corrupt_backup.unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(&first).unwrap(), "{ first");
        assert_eq!(fs::read_to_string(&second).unwrap(), "{ second");

        // Can't move it aside: nothing saves over it this run.
        fs::write(&path, "{ third").unwrap();
        let mode = |m| fs::set_permissions(&dir, fs::Permissions::from_mode(m)).unwrap();
        mode(0o555);
        let st = State::load_at(&path);
        mode(0o755);
        assert!(st.read_only && st.corrupt_backup.is_none());
        assert!(st.save_at(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ third");
        assert_eq!(baks().len(), 2);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn auxiliary_values_and_failed_saves() {
        let json = r#"{"repos":[],"workspaces":[],"reviewed":{"ok":["r1"],"bad":9},"merge_order":false,"delete_remote_branches":"bad"}"#;
        let mut state: State = serde_json::from_str(json).unwrap();
        assert_eq!(state.reviewed["ok"], ["r1"]);
        assert!(state.merge_order.is_empty() && !state.delete_remote_branches);
        let dir = std::env::temp_dir().join(format!("shark-state-retry-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        fs::create_dir(&path).unwrap();
        state.reviewed.insert("ok".into(), vec!["latest".into()]);
        assert!(state.save_at(&path).is_err());
        assert!(state.save_error.borrow().is_some());
        fs::remove_dir(&path).unwrap();
        state.save_at(&path).unwrap();
        assert!(state.save_error.borrow().is_none());
        let saved = State::load_at(&path);
        assert_eq!(saved.reviewed["ok"], ["latest"]);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A pre-tabs workspace keeps its session, model and transcript as its first chat.
    #[test]
    fn migrates_single_chat() {
        let json = r#"{"name":"feat","branch":"b","repos":[],"session_id":"s1","model":"opus"}"#;
        let mut ws: Workspace = serde_json::from_str(json).unwrap();
        ws.migrate();
        assert_eq!(
            ws.chats,
            vec![ChatMeta {
                id: "feat".into(),
                title: String::new(),
                session_id: Some("s1".into()),
                model: Some("opus".into()),
                effort: None,
                plan: false,
                fast: None,
            }]
        );
        assert_eq!(ws.next_chat_id(), "feat~2");
        ws.closed.push(ChatMeta {
            id: "feat~7".into(),
            ..Default::default()
        });
        assert_eq!(ws.next_chat_id(), "feat~8");
        let saved = serde_json::to_value(&ws).unwrap();
        assert!(
            saved.get("session_id").is_none(),
            "legacy fields aren't written back"
        );
    }

    /// Chats from before every chat named a model load on one: claude's default on the last
    /// pick, codex's default on Sol. New chats take the last pick too.
    #[test]
    fn fills_models_and_new_chats() {
        let dir = std::env::temp_dir().join(format!("shark-models-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let chats =
            r#"[{"id":"a"},{"id":"b","model":"codex"},{"id":"c","model":"opus","effort":"low"}]"#;
        let ws = format!(
            r#"{{"name":"w","branch":"w","repos":[],"chats":{chats},"closed":[{{"id":"d"}}]}}"#
        );
        let json = |last: &str| format!(r#"{{"repos":[],"workspaces":[{ws}]{last}}}"#);
        fs::write(&path, json("")).unwrap();
        let st = State::load_at(&path);
        let models = |st: &State| {
            let w = &st.workspaces[0];
            (w.chats.iter().chain(&w.closed))
                .map(|c| (c.model.clone().unwrap(), c.effort.clone()))
                .collect::<Vec<_>>()
        };
        let m = |m: &str, e: Option<&str>| (m.to_string(), e.map(String::from));
        let sol = "codex:gpt-6.1-sol";
        assert_eq!(
            models(&st),
            [
                m("fable", None),
                m(sol, None),
                m("opus", Some("low")),
                m("fable", None)
            ]
        );
        fs::write(&path, json(r#","last_model":"haiku","last_effort":"max""#)).unwrap();
        let st = State::load_at(&path);
        assert_eq!(models(&st)[0], m("haiku", Some("max")));
        let new = st.new_chat(&st.workspaces[0], "x".into());
        assert_eq!(
            (new.model.as_deref(), new.effort.as_deref()),
            (Some("haiku"), Some("max"))
        );
        // A repo's default beats the last pick; its level falls back to the last one.
        let mut st = st;
        st.workspaces[0].repos = vec![WsRepo {
            name: "api".into(),
            src: "/api".into(),
            base: "origin/main".into(),
            branch: None,
        }];
        st.repo_settings.insert(
            "api".into(),
            RepoSettings {
                model: Some("opus".into()),
                ..Default::default()
            },
        );
        let new = st.new_chat(&st.workspaces[0], "y".into());
        assert_eq!(
            (new.model.as_deref(), new.effort.as_deref()),
            (Some("opus"), Some("max"))
        );
        let mut w: Workspace = serde_json::from_str(
            r#"{"name":"w","branch":"w","repos":[],"chats":[{"id":"a"},{"id":"b","session_id":"s"}]}"#,
        )
        .unwrap();
        w.fill_models("codex:gpt-6-astra", None);
        let picked: Vec<_> = w
            .chats
            .iter()
            .map(|c| c.model.as_deref().unwrap())
            .collect();
        assert_eq!(
            picked,
            ["codex:gpt-6-astra", "fable"],
            "a claude session stays on claude"
        );
        let gone = State {
            last_model: Some("default".into()),
            ..State::default()
        };
        assert_eq!(gone.new_model(), "fable", "a pick no longer listed");
        fs::remove_dir_all(&dir).unwrap();
    }

    fn ws(name: &str) -> Workspace {
        Workspace {
            name: name.into(),
            branch: name.into(),
            repos: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn diff_closure_defaults_and_round_trips() {
        let legacy = r#"{"name":"legacy","branch":"main","repos":[]}"#;
        let mut ws: Workspace = serde_json::from_str(legacy).unwrap();
        assert!(ws.diff_closed);
        assert!(Workspace::default().diff_closed);
        for closed in [false, true] {
            ws.diff_closed = closed;
            let saved = serde_json::to_string(&ws).unwrap();
            assert_eq!(
                serde_json::from_str::<Workspace>(&saved)
                    .unwrap()
                    .diff_closed,
                closed
            );
        }
    }

    #[test]
    fn drop_ws_reorders() {
        let mut st = State {
            workspaces: ["a", "b", "c", "d"].map(ws).into(),
            sections: vec![Section {
                name: "S".into(),
                icon: "folder".into(),
                collapsed: false,
                workspaces: vec!["c".into(), "d".into()],
            }],
            ..Default::default()
        };
        let names = |st: &State| {
            st.workspaces
                .iter()
                .map(|w| w.name.clone())
                .collect::<Vec<_>>()
        };
        st.drop_ws("a", "b"); // down within the ungrouped: takes b's slot
        assert_eq!(names(&st), ["b", "a", "c", "d"]);
        st.drop_ws("a", "b"); // and back up
        assert_eq!(names(&st), ["a", "b", "c", "d"]);
        st.drop_ws("d", "c"); // within a section
        assert_eq!(st.sections[0].workspaces, ["d", "c"]);
        st.drop_ws("a", "c"); // into a section, before the target
        assert_eq!(st.sections[0].workspaces, ["d", "a", "c"]);
        st.drop_ws("d", "b"); // out to the ungrouped, before the target
        assert_eq!(st.sections[0].workspaces, ["a", "c"]);
        let ungrouped: Vec<_> = names(&st)
            .into_iter()
            .filter(|n| !st.sections[0].workspaces.contains(n))
            .collect();
        assert_eq!(ungrouped, ["d", "b"]);
    }
}

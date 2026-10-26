use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct State {
    pub repos: Vec<Repo>,
    pub workspaces: Vec<Workspace>,
    /// Vim keys in the file editor.
    #[serde(default)]
    pub vim: bool,
    /// Repo names picked for the last new workspace; the next dialog starts with them.
    #[serde(default)]
    pub last_repos: Vec<String>,
    /// The workspace and tab (`chat`/`diff`, or `home` while Home was shown over it) open at
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
    /// The Files panel's width, once dragged.
    #[serde(default)]
    pub tree_width: Option<f32>,
    /// The original column's share of a Split diff, the same for every file; `None` is half.
    #[serde(default)]
    pub split_ratio: Option<f32>,
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

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Workspace {
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
    /// Diff path -> fingerprint of its patch when marked viewed; a new patch makes it unviewed.
    #[serde(default)]
    pub viewed: HashMap<String, u64>,
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
    }

    /// Chats without a model (an old one on the CLI default, a new workspace's first) get
    /// `model` and `effort`; old ones on codex's default get the model it was.
    pub fn fill_models(&mut self, model: &str, effort: Option<&String>) {
        for c in self.chats.iter_mut().chain(&mut self.closed) {
            match c.model.as_deref() {
                None => {
                    // A claude session can't resume on codex.
                    let claude = c.session_id.is_some() && crate::codex::is_codex(Some(model));
                    c.model = Some(if claude { crate::chat::MODELS[0] } else { model }.into());
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

/// Review IDs the git button already sent to the agent, per workspace name. They live in
/// state.json beside `State`'s fields rather than among them: only the button reads or writes
/// them, and `save` carries them over.
const REVIEWED: &str = "reviewed";
/// Repo names in the order the merge step merges them, per workspace name; same rules as REVIEWED.
const MERGE_ORDER: &str = "merge_order";
/// Opt-in: the merge step also deletes each PR's remote branch (`gh pr merge --delete-branch`).
const DELETE_REMOTE: &str = "delete_remote_branches";

fn read(path: &Path) -> Value {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write(path: &Path, v: &Value) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, serde_json::to_vec_pretty(v).unwrap()).is_ok() {
        let _ = fs::rename(tmp, path);
    }
}

pub fn reviewed(ws: &str) -> Vec<String> {
    list_at(&state_file(), REVIEWED, ws)
}

pub fn set_reviewed(ws: &str, ids: &[String]) {
    set_list_at(&state_file(), REVIEWED, ws, ids)
}

pub fn merge_order(ws: &str) -> Vec<String> {
    list_at(&state_file(), MERGE_ORDER, ws)
}

pub fn set_merge_order(ws: &str, names: &[String]) {
    set_list_at(&state_file(), MERGE_ORDER, ws, names)
}

pub fn delete_remote_branches() -> bool {
    read(&state_file())[DELETE_REMOTE] == true
}

pub fn set_delete_remote_branches(on: bool) {
    let path = state_file();
    let mut v = read(&path);
    if v.is_object() {
        v[DELETE_REMOTE] = on.into();
        write(&path, &v);
    }
}

fn list_at(path: &Path, key: &str, ws: &str) -> Vec<String> {
    serde_json::from_value(read(path)[key][ws].clone()).unwrap_or_default()
}

fn set_list_at(path: &Path, key: &str, ws: &str, ids: &[String]) {
    let mut v = read(path);
    if !v.is_object() {
        return; // no state to attach to; a lone key would make state.json unreadable
    }
    let mut all: HashMap<String, Vec<String>> =
        serde_json::from_value(v[key].take()).unwrap_or_default();
    all.insert(ws.into(), ids.to_vec());
    // Archived workspaces drop out here.
    let live: Vec<&str> = v["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| w["name"].as_str())
        .collect();
    all.retain(|k, _| live.contains(&k.as_str()));
    v[key] = serde_json::to_value(all).unwrap();
    write(path, &v);
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

    /// The model a new chat gets: the last one picked, else the first.
    pub fn new_model(&self) -> String {
        let models = crate::chat::MODELS;
        let last = self.last_model.as_deref().filter(|m| models.contains(m));
        last.unwrap_or(models[0]).into()
    }

    /// A new chat `id` on the last picked model and thinking level.
    pub fn new_chat(&self, id: String) -> ChatMeta {
        ChatMeta {
            id,
            model: Some(self.new_model()),
            effort: self.last_effort.clone(),
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
            self.save()
        }
    }

    pub fn save(&self) {
        self.save_at(&state_file())
    }

    fn save_at(&self, path: &Path) {
        if self.read_only {
            return;
        }
        let mut v = serde_json::to_value(self).unwrap();
        let mut old = read(path);
        for key in [REVIEWED, MERGE_ORDER, DELETE_REMOTE] {
            let kept = old[key].take();
            if !kept.is_null() {
                v[key] = kept;
            }
        }
        write(path, &v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        st.save_at(&path);
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
        st.save_at(&path);
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ third");
        assert_eq!(baks().len(), 2);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reviewed_ids_survive_saves() {
        let dir = std::env::temp_dir().join(format!("shark-state-{}", std::process::id()));
        let path = dir.join("state.json");
        let state = State {
            workspaces: vec![ws("a"), ws("b")],
            ..Default::default()
        };
        state.save_at(&path);
        set_list_at(&path, REVIEWED, "a", &["r1".into()]);
        set_list_at(&path, REVIEWED, "b", &["r2".into()]);
        state.save_at(&path); // the app saving its own copy keeps them
        assert_eq!(list_at(&path, REVIEWED, "a"), ["r1"]);
        let st: State = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(st.workspaces.len(), 2, "state.json still loads");
        State {
            workspaces: vec![ws("a")],
            ..Default::default()
        }
        .save_at(&path);
        set_list_at(&path, REVIEWED, "a", &["r1".into(), "r3".into()]);
        assert_eq!(
            list_at(&path, REVIEWED, "b"),
            Vec::<String>::new(),
            "archived"
        );
        set_list_at(&path, MERGE_ORDER, "a", &["web".into(), "api".into()]);
        state.save_at(&path);
        assert_eq!(
            list_at(&path, MERGE_ORDER, "a"),
            ["web", "api"],
            "side keys survive saves"
        );
        fs::remove_dir_all(&dir).unwrap();
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
        let chats = r#"[{"id":"a"},{"id":"b","model":"codex"},{"id":"c","model":"opus","effort":"low"}]"#;
        let ws = format!(r#"{{"name":"w","branch":"w","repos":[],"chats":{chats},"closed":[{{"id":"d"}}]}}"#);
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
            [m("fable", None), m(sol, None), m("opus", Some("low")), m("fable", None)]
        );
        fs::write(&path, json(r#","last_model":"haiku","last_effort":"max""#)).unwrap();
        let st = State::load_at(&path);
        assert_eq!(models(&st)[0], m("haiku", Some("max")));
        let new = st.new_chat("x".into());
        assert_eq!((new.model.as_deref(), new.effort.as_deref()), (Some("haiku"), Some("max")));
        let mut w: Workspace = serde_json::from_str(
            r#"{"name":"w","branch":"w","repos":[],"chats":[{"id":"a"},{"id":"b","session_id":"s"}]}"#,
        )
        .unwrap();
        w.fill_models("codex:gpt-6-astra", None);
        let picked: Vec<_> = w.chats.iter().map(|c| c.model.as_deref().unwrap()).collect();
        assert_eq!(picked, ["codex:gpt-6-astra", "fable"], "a claude session stays on claude");
        let gone = State { last_model: Some("default".into()), ..State::default() };
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

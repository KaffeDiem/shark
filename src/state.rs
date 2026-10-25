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
    /// The workspace and tab (`chat`/`diff`) open at quit; restored on launch.
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
    /// Claude model alias (`opus`, `fable`, …) or `codex[:<model id>]`; `None` uses the claude
    /// CLI default.
    #[serde(default)]
    pub model: Option<String>,
    /// Plan mode: the agent plans and edits nothing until the plan is approved.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct WsRepo {
    pub name: String,
    pub src: PathBuf,
    /// Ref the branch was cut from, e.g. `origin/main`.
    pub base: String,
}

/// Moves item `from` into slot `to`; the ones between shift over.
pub fn move_item<T>(v: &mut Vec<T>, from: usize, to: usize) {
    let x = v.remove(from);
    v.insert(to, x);
}

/// Lets tests redirect app data without mutating the process environment.
#[cfg(test)]
pub static TEST_HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn home() -> PathBuf {
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
        let path = state_file();
        let Ok(bytes) = fs::read(&path) else {
            return State::default();
        };
        let mut state: State = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            // keep the unreadable file instead of overwriting it on the next save
            let _ = fs::rename(&path, path.with_extension("json.bak"));
            eprintln!("state.json unreadable ({e}), moved to state.json.bak");
            State::default()
        });
        state.workspaces.iter_mut().for_each(Workspace::migrate);
        state
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

    pub fn save(&self) {
        self.save_at(&state_file())
    }

    fn save_at(&self, path: &Path) {
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
                plan: false,
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

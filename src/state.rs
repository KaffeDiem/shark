use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::PathBuf,
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
    #[serde(default)]
    pub appearance: Appearance,
    /// Edited git-button prompts by id (`prompts::PROMPTS`); missing ids use the default.
    #[serde(default)]
    pub prompts: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Default, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    System,
    Light,
    Dark,
}

/// Theme families by name (`main::THEMES`); fonts `None` use the theme's.
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
    /// Claude model alias (`opus`, `fable`, …); `None` uses the CLI default.
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct WsRepo {
    pub name: String,
    pub src: PathBuf,
    /// Ref the branch was cut from, e.g. `origin/main`.
    pub base: String,
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

impl State {
    pub fn load() -> State {
        let path = support_dir().join("state.json");
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

    pub fn save(&self) {
        let dir = support_dir();
        let _ = fs::create_dir_all(&dir);
        let tmp = dir.join("state.json.tmp");
        if fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap()).is_ok() {
            let _ = fs::rename(tmp, dir.join("state.json"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

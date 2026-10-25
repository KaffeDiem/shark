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

#[derive(Serialize, Deserialize, Clone)]
pub struct Workspace {
    pub name: String,
    pub branch: String,
    pub repos: Vec<WsRepo>,
    pub session_id: Option<String>,
    /// Claude model alias (`opus`, `fable`, …); `None` uses the CLI default.
    #[serde(default)]
    pub model: Option<String>,
    /// Diff path -> fingerprint of its patch when marked viewed; a new patch makes it unviewed.
    #[serde(default)]
    pub viewed: HashMap<String, u64>,
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
}

impl State {
    pub fn load() -> State {
        let path = support_dir().join("state.json");
        let Ok(bytes) = fs::read(&path) else {
            return State::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            // keep the unreadable file instead of overwriting it on the next save
            let _ = fs::rename(&path, path.with_extension("json.bak"));
            eprintln!("state.json unreadable ({e}), moved to state.json.bak");
            State::default()
        })
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

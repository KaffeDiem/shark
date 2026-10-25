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
    /// Sidebar groups of workspaces, in display order; ungrouped workspaces follow them.
    #[serde(default)]
    pub sections: Vec<Section>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Section {
    pub name: String,
    /// Lucide icon name from `app::SECTION_ICONS`, e.g. `folder`.
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

    fn ws(name: &str) -> Workspace {
        Workspace {
            name: name.into(),
            branch: name.into(),
            repos: vec![],
            session_id: None,
            model: None,
            viewed: HashMap::new(),
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

use crate::scripts::Phase;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct State {
    /// Legacy chat identities have been copied from their tasks exactly once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub chat_agents_migrated: bool,
    #[serde(default, deserialize_with = "tolerant_lists")]
    pub reviewed: HashMap<String, Vec<String>>,
    #[serde(default, deserialize_with = "tolerant_lists")]
    pub merge_order: HashMap<String, Vec<String>>,
    #[serde(default, deserialize_with = "tolerant_bool")]
    pub delete_remote_branches: bool,
    #[serde(skip)]
    pub save_error: std::cell::RefCell<Option<String>>,
    pub repos: Vec<Repo>,
    #[serde(rename = "workspaces")]
    pub tasks: Vec<Task>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<Agent>,
    /// Vim keys in the file editor.
    #[serde(default)]
    pub vim: bool,
    /// The file editor's code size in logical px (cmd-=/cmd--); None is
    /// `editor::FONT_SIZE`.
    #[serde(default)]
    pub editor_font_size: Option<f32>,
    /// Repo names picked for the last agent made in the dialog; the next one starts with them.
    #[serde(default)]
    pub last_repos: Vec<String>,
    /// Applications opened through Shark, most recent first, shared by all tasks.
    #[serde(default)]
    pub recent_apps: Vec<PathBuf>,
    /// The task and tab (`chat`/`diff`/`git`, or `home` while Home was shown over it) open at
    /// quit; restored on launch.
    #[serde(default)]
    pub last_open: Option<(String, String)>,
    #[serde(default)]
    pub appearance: Appearance,
    /// Edited git-button prompts by id (`prompts::PROMPTS`); missing ids use the default.
    #[serde(default)]
    pub prompts: BTreeMap<String, String>,
    /// Sidebar groups of tasks, in display order; ungrouped tasks follow them.
    #[serde(default)]
    pub sections: Vec<Section>,
    /// Keep the Mac from idle-sleeping while an agent works (`Shark::update_awake`).
    #[serde(default)]
    pub stay_awake: bool,
    /// Who Linear is connected as; the credential itself is in the Keychain.
    #[serde(default)]
    pub linear: Option<crate::linear::Connection>,
    /// Keep the repos' skills from the agents (`git::claude_skill_args`, `git::link_skills`).
    #[serde(default)]
    pub no_repo_skills: bool,
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
    /// Compare the working tree to HEAD instead of the task base.
    #[serde(default)]
    pub uncommitted_only: bool,
    /// The Files panel's width, once dragged.
    #[serde(default)]
    pub tree_width: Option<f32>,
    /// The Artifacts list's width, once dragged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts_width: Option<f32>,
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
    /// Model and thinking level new chats start on in tasks with this repo.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// After its worktree is created.
    pub setup: Vec<String>,
    /// cmd-r in a task with this repo; cmd-r again stops them.
    pub run: Vec<String>,
    /// Before its worktree is removed.
    pub archive: Vec<String>,
}

impl RepoSettings {
    /// The scripts of `p`.
    pub fn phase(&self, p: Phase) -> &Vec<String> {
        match p {
            Phase::Setup => &self.setup,
            Phase::Run => &self.run,
            Phase::Archive => &self.archive,
        }
    }

    /// The scripts of `p`, to edit.
    pub fn phase_mut(&mut self, p: Phase) -> &mut Vec<String> {
        match p {
            Phase::Setup => &mut self.setup,
            Phase::Run => &mut self.run,
            Phase::Archive => &mut self.archive,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
/// Older installs also saved an agent on some sections; serde drops it and they load as plain ones.
pub struct Section {
    pub name: String,
    /// Lucide icon name from `app::sidebar::SECTION_ICONS`, e.g. `folder`.
    pub icon: String,
    #[serde(default)]
    pub collapsed: bool,
    /// Task names, in the order they were added.
    #[serde(default)]
    #[serde(rename = "workspaces")]
    pub tasks: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Default, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    System,
    Light,
    Dark,
}

/// Theme families by name (`theme::THEMES`); fonts `None` use the theme's.
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

/// Accept pre-avatar states where the icon was a plain Lucide name.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AgentIcon {
    Symbol {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tint: Option<String>,
    },
    Emoji {
        value: String,
    },
    Image {
        file: String,
    },
}

impl Default for AgentIcon {
    fn default() -> Self {
        "bot".into()
    }
}

impl From<&str> for AgentIcon {
    fn from(name: &str) -> Self {
        name.to_owned().into()
    }
}

impl From<String> for AgentIcon {
    fn from(name: String) -> Self {
        Self::Symbol { name, tint: None }
    }
}

impl<'de> Deserialize<'de> for AgentIcon {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "lowercase")]
        enum Tagged {
            Symbol {
                name: String,
                #[serde(default)]
                tint: Option<String>,
            },
            Emoji {
                value: String,
            },
            Image {
                file: String,
            },
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Legacy(String),
            Current(Tagged),
        }
        Ok(match Stored::deserialize(d)? {
            Stored::Legacy(name) => name.into(),
            Stored::Current(Tagged::Symbol { name, tint }) => Self::Symbol { name, tint },
            Stored::Current(Tagged::Emoji { value }) => Self::Emoji { value },
            Stored::Current(Tagged::Image { file }) => Self::Image { file },
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub struct Agent {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub icon: AgentIcon,
    pub instructions: String,
    pub repos: Vec<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<TaskRecord>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TaskRecord {
    pub title: String,
    pub branch: String,
    pub prs: Vec<(String, String)>,
    pub merged: Option<bool>,
    /// Archive time in Unix milliseconds.
    pub at: u64,
}

/// What a task's chats know of one agent they use: harness startup and context notes read this
/// snapshot, not global `State`.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub struct AgentLink {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub icon: AgentIcon,
    pub instructions: String,
}

impl From<&Agent> for AgentLink {
    fn from(agent: &Agent) -> Self {
        Self {
            slug: agent.slug.clone(),
            name: agent.name.clone(),
            icon: agent.icon.clone(),
            instructions: agent.instructions.clone(),
        }
    }
}

pub fn agent_skills_dir(slug: &str) -> PathBuf {
    support_dir().join("agents").join(slug).join("skills")
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Task {
    /// Before agents were per chat, a task belonged to one; `State::load` moves it into `agents`.
    #[serde(default, skip_serializing)]
    pub agent: Option<AgentLink>,
    /// The agents its chats use (`ChatMeta::agent`), one snapshot each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentLink>,
    /// Distinguishes this task from later tasks that reuse its name.
    #[serde(default)]
    pub instance_id: String,
    /// Last explicit user interaction, in Unix milliseconds; background work never updates it.
    #[serde(default)]
    pub last_interacted_at: Option<u64>,
    /// Parked until the user resumes it or an agent starts working.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub backlog: bool,
    /// Immutable identity: the directory, the first chat's id, every per-task key.
    pub name: String,
    /// What the user sees and edits; empty or `NEW_TASK` shows `name` (`display_title`).
    #[serde(default)]
    pub title: String,
    /// With no repos: a placeholder equal to `name`, which nothing shows. The first repo replaces
    /// it with its branch (`default_branch` is what the dialog offers), and later repos inherit it.
    pub branch: String,
    pub repos: Vec<TaskRepo>,
    /// The attachment operation under way, in the order it runs. Saved before Git changes anything;
    /// while it holds anything, the agent's chats and Git steps wait.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attaching: Vec<Attachment>,
    /// The Linear issues attached to it.
    #[serde(default)]
    pub linear: crate::linear::Attachments,
    /// `linear`'s snapshots as `linear::read_snapshots` has them; their file owns them.
    #[serde(skip)]
    pub issues: Vec<crate::linear::Snapshot>,
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
    /// Highest reserved chat number, retained after closed history is evicted.
    #[serde(default)]
    pub chat_sequence: u64,
    #[serde(default)]
    pub active_chat: usize,
    /// Closed by default, until an explicit Diff action opens it.
    #[serde(default = "closed_by_default")]
    pub diff_closed: bool,
    /// Closed until the Git feature is explicitly opened.
    #[serde(default = "closed_by_default")]
    pub git_closed: bool,
    /// Closed until the Artifacts feature is explicitly opened.
    #[serde(default = "closed_by_default")]
    pub artifacts_closed: bool,
    /// Artifacts pinned to the top of the list, newest pin first; paths inside `.context`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts_pinned: Vec<String>,
    /// Diff path -> fingerprint of its patch when marked viewed; a new patch makes it unviewed.
    #[serde(default)]
    pub viewed: HashMap<String, u64>,
    /// The tab strip, left to right. File entries are also the files reopened with the
    /// task; `normalize_tabs` keeps chat and feature tabs in step with their open/closed state.
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
    Artifacts,
    /// `repo/rel/path`, as in the diff.
    File(String),
}

/// What `Task::remove_chat` took out of the lists.
pub struct RemovedChat {
    pub meta: ChatMeta,
    /// It was the only chat, so a fresh one took its place.
    pub last: bool,
    pub untouched: bool,
    /// Closed chats pushed out by the cap.
    pub evicted: Vec<ChatMeta>,
}

fn closed_by_default() -> bool {
    true
}

fn task_instance_id() -> String {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).expect("task identity randomness");
    format!("{:032x}", u128::from_be_bytes(bytes))
}

impl Default for Task {
    fn default() -> Self {
        Self {
            agent: None,
            agents: Vec::new(),
            instance_id: task_instance_id(),
            last_interacted_at: None,
            backlog: false,
            name: String::new(),
            title: String::new(),
            branch: String::new(),
            repos: Vec::new(),
            attaching: Vec::new(),
            linear: Default::default(),
            issues: Vec::new(),
            session_id: None,
            model: None,
            chats: Vec::new(),
            closed: Vec::new(),
            chat_sequence: 0,
            active_chat: 0,
            diff_closed: closed_by_default(),
            git_closed: true,
            artifacts_closed: true,
            artifacts_pinned: Vec::new(),
            viewed: HashMap::new(),
            tab_order: Vec::new(),
        }
    }
}

/// One conversation: its own claude process, session and transcript.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
pub struct ChatMeta {
    /// Transcript and log file name. The first chat's id is the task name, which is
    /// where the single pre-tabs transcript already lives.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// First line of the first message; empty until then.
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Claude model alias (`opus`, `fable`, …) or `codex:<model id>`. Only chats saved before
    /// every chat named one have `None` (the CLI default); `Task::fill_models` gives
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
    /// What this chat's session has been told of `Task::context`, by source, as text
    /// fingerprints. Anything else goes out with its next normal message.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub delivered: BTreeMap<String, u64>,
}

/// A repository on its way into an agent. The stage says how far it got, so a restart can
/// reconcile it (`app::attach`).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Attachment {
    pub repo: String,
    pub src: PathBuf,
    pub branch: String,
    /// What a fresh branch starts on; empty is the repo's default.
    #[serde(default)]
    pub base: String,
    /// A new branch, so an existing one of that name is refused. Otherwise a branch the user
    /// picked to check out.
    pub fresh: bool,
    pub stage: AttachStage,
    /// Why it stopped: the setup failure, or what a restart found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum AttachStage {
    /// Nothing exists yet: it waits for idle or its turn, and can be cancelled.
    Queued,
    /// Git may have started; a restart looks at what is there.
    Creating,
    /// In `Task::repos`; its setup scripts are running.
    Setup,
    /// Kept with its worktree until the user retries or continues.
    SetupFailed,
    /// After an interruption, resources that can't be told apart from someone else's.
    Uncertain,
}

impl Attachment {
    /// Fresh on `branch`, waiting its turn.
    pub fn fresh(r: &Repo, branch: &str, base: &str) -> Self {
        Attachment {
            repo: r.name.clone(),
            src: r.path.clone(),
            branch: branch.into(),
            base: base.into(),
            fresh: true,
            stage: AttachStage::Queued,
            error: None,
        }
    }

    /// It stopped and waits for the user.
    pub fn stuck(&self) -> bool {
        matches!(
            self.stage,
            AttachStage::SetupFailed | AttachStage::Uncertain
        )
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct TaskRepo {
    pub name: String,
    pub src: PathBuf,
    /// Ref the branch was cut from, e.g. `origin/main`.
    pub base: String,
    /// Its own branch; set only when it differs from the task branch.
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

/// Tests never reach the real home, even ones that don't set it up themselves.
#[cfg(test)]
pub(crate) fn home() -> PathBuf {
    (TEST_HOME
        .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id()))))
    .clone()
}

#[cfg(not(test))]
pub(crate) fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

pub fn support_dir() -> PathBuf {
    home().join("Library/Application Support/shark")
}

pub fn tasks_dir() -> PathBuf {
    home().join("shark/workspaces")
}

/// The saved title of an agent nothing has been said to yet; its first message replaces it. Only
/// this means untitled: a user may title an agent after its name.
pub const NEW_TASK: &str = "New agent";

/// The key prefix of a repository in `Task::context`. A session reads these from the root
/// CLAUDE.md or AGENTS.md when it starts, so it starts with them delivered.
pub const REPO_CONTEXT: &str = "repo:";
pub const AGENT_CONTEXT: &str = "agent:";

/// The key prefix of a Linear issue in `Task::context`, before its UUID. Unlike repositories
/// these are in no root doc, so every session gets them with its first normal turn.
pub const ISSUE_CONTEXT: &str = "issue:";

/// One entry of `Task::context`.
#[derive(Clone, Debug, PartialEq)]
pub struct ContextNote {
    pub text: String,
    /// Of what the source says, which may be less than `text`: only a change here sends it again.
    pub fingerprint: u64,
}

/// `Task::context` as a chat records it (`ChatMeta::delivered`).
pub fn context_record(context: &BTreeMap<String, ContextNote>) -> BTreeMap<String, u64> {
    (context.iter())
        .map(|(k, note)| (k.clone(), note.fingerprint))
        .collect()
}

/// What new agents are called, each safe as a directory and as a branch.
pub const SEA_NAMES: &[&str] = &[
    "albacore",
    "amberjack",
    "anchovy",
    "anemone",
    "angelfish",
    "anglerfish",
    "barnacle",
    "barracuda",
    "basslet",
    "beluga",
    "bigeye",
    "blenny",
    "blue-tang",
    "blue-whale",
    "bluefin",
    "bluefish",
    "boarfish",
    "bonito",
    "boxfish",
    "bream",
    "brill",
    "butterflyfish",
    "capelin",
    "cardinalfish",
    "clam",
    "clownfish",
    "cobia",
    "cod",
    "coelacanth",
    "conch",
    "conger",
    "coral",
    "cowfish",
    "crab",
    "croaker",
    "cuttlefish",
    "damselfish",
    "dolphin",
    "dorado",
    "dory",
    "dragonet",
    "dugong",
    "eel",
    "filefish",
    "flounder",
    "flyingfish",
    "frogfish",
    "garibaldi",
    "goby",
    "grouper",
    "gurnard",
    "haddock",
    "hake",
    "halibut",
    "hammerhead",
    "hawkfish",
    "herring",
    "hoki",
    "humpback",
    "jawfish",
    "jellyfish",
    "john-dory",
    "kingfish",
    "krill",
    "lanternfish",
    "limpet",
    "ling",
    "lingcod",
    "lionfish",
    "lobster",
    "lumpfish",
    "mackerel",
    "mahi-mahi",
    "mako",
    "manatee",
    "mandarinfish",
    "manta",
    "marlin",
    "menhaden",
    "monkfish",
    "moonfish",
    "moray",
    "mullet",
    "mussel",
    "narwhal",
    "nautilus",
    "needlefish",
    "oarfish",
    "octopus",
    "opah",
    "orca",
    "otter",
    "oyster",
    "parrotfish",
    "pilchard",
    "pilotfish",
    "pipefish",
    "plaice",
    "pollock",
    "pomfret",
    "pompano",
    "porcupinefish",
    "porgy",
    "porpoise",
    "prawn",
    "pufferfish",
    "quillback",
    "razorfish",
    "redfish",
    "reef-shark",
    "remora",
    "rockfish",
    "sablefish",
    "sailfish",
    "salmon",
    "sardine",
    "sawfish",
    "scallop",
    "scorpionfish",
    "sculpin",
    "sea-bass",
    "sea-lion",
    "sea-otter",
    "sea-robin",
    "sea-turtle",
    "seadragon",
    "seahorse",
    "seal",
    "shad",
    "shrimp",
    "skate",
    "smelt",
    "snapper",
    "snook",
    "sole",
    "spadefish",
    "spearfish",
    "sponge",
    "sprat",
    "squid",
    "squirrelfish",
    "starfish",
    "stingray",
    "stonefish",
    "sturgeon",
    "sunfish",
    "surgeonfish",
    "swordfish",
    "tang",
    "tarpon",
    "tautog",
    "tiger-shark",
    "tilefish",
    "toadfish",
    "trevally",
    "triggerfish",
    "tripletail",
    "trumpetfish",
    "tuna",
    "turbot",
    "turtle",
    "urchin",
    "wahoo",
    "walrus",
    "whale-shark",
    "whiting",
    "wolffish",
    "wrasse",
    "wreckfish",
    "yellowfin",
    "yellowtail",
    "zebra-shark",
];

/// A new agent's identity: a random entry of `SEA_NAMES` that isn't `taken`, or with all of them
/// gone, a numbered one (`marlin-2`). `seed` stands in if the system has no randomness to give.
pub fn task_name(seed: u64, taken: impl Fn(&str) -> bool) -> String {
    let pick = getrandom::u64().unwrap_or(seed) as usize;
    (1..)
        .find_map(|n| {
            let free: Vec<String> = (SEA_NAMES.iter())
                .map(|name| match n {
                    1 => name.to_string(),
                    n => format!("{name}-{n}"),
                })
                .filter(|name| !taken(name))
                .collect();
            (!free.is_empty()).then(|| free[pick % free.len()].clone())
        })
        .expect("numbers outlast the agents")
}

/// A short, branch-safe name shared by agent identities and task titles.
pub fn slug(source: &str) -> String {
    let mut slug = String::new();
    for word in source
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if slug.len() + word.len() > 40 {
            break;
        }
        slug += &word.to_ascii_lowercase();
        slug.push('-');
    }
    slug.pop();
    slug
}

impl Task {
    pub fn dir(&self) -> PathBuf {
        tasks_dir().join(&self.name)
    }

    /// What to call it on screen. Untitled, that's its name capitalised, but an `agent-<hex>` from
    /// before `SEA_NAMES` stays "New agent".
    pub fn display_title(&self) -> Cow<'_, str> {
        match self.title.as_str() {
            "" => self.name.as_str().into(),
            NEW_TASK if !self.name.starts_with("agent-") => {
                let mut c = self.name.chars();
                let first = c.next().into_iter().flat_map(char::to_uppercase);
                first.chain(c).collect::<String>().into()
            }
            NEW_TASK => "New task".into(),
            title => title.into(),
        }
    }

    /// Whether it has a repository to build in; plan mode can't end without one. An attachment
    /// counts until it is done: set up, or continued past a failed setup.
    pub fn ready(&self) -> bool {
        !self.repos.is_empty() && self.attaching.is_empty()
    }

    /// The primary attached issue, as last retrieved.
    pub fn primary_issue(&self) -> Option<&crate::linear::Snapshot> {
        self.linear.primary()?.of(&self.issues)
    }

    /// The branch the next repository attaches on: the one the agent is on, or for the first,
    /// `<primary issue or title>-<name>`, less the `agent-` of an old name.
    pub fn default_branch(&self) -> String {
        if !self.repos.is_empty() {
            return self.branch.clone();
        }
        let title = self.display_title();
        let source = match self.primary_issue() {
            Some(issue) => &issue.identifier,
            None if self.title == NEW_TASK => "",
            None => &title,
        };
        let title = slug(source);
        match title.is_empty() {
            true => self.name.clone(),
            false => format!(
                "{title}-{}",
                self.name.strip_prefix("agent-").unwrap_or(&self.name)
            ),
        }
    }

    /// What every chat of the agent should know, one entry per source, each a `[shark]` note:
    /// `repo:<name>` for each repository, `issue:<uuid>` for each attached issue. A chat gets what
    /// it hasn't received (`ChatMeta::delivered`) with its next normal message.
    pub fn context(&self) -> BTreeMap<String, ContextNote> {
        let mut notes: BTreeMap<_, _> = (self.repos.iter())
            .map(|r| {
                let rules = crate::git::rules_file(&self.dir().join(&r.name))
                    .map_or(String::new(), |f| format!(" Read {}/{f} for its rules.", r.name));
                let text = format!(
                    "[shark] Repo `{0}` is attached: `{0}/` is a git worktree of {1} on branch `{2}` (base `{3}`), and the root CLAUDE.md and AGENTS.md list it.{rules}",
                    r.name,
                    r.src.display(),
                    self.branch_of(r),
                    r.base
                );
                let fingerprint = crate::files::fingerprint(&text);
                (
                    format!("{REPO_CONTEXT}{}", r.name),
                    ContextNote { text, fingerprint },
                )
            })
            .chain(crate::linear::context_notes(&self.linear, &self.issues))
            .collect();
        for agent in &self.agents {
            let text = format!(
                "[shark] Your instructions as {} changed: {}",
                agent.name, agent.instructions
            );
            notes.insert(
                format!("{AGENT_CONTEXT}{}", agent.slug),
                ContextNote {
                    fingerprint: crate::files::fingerprint(&agent.instructions),
                    text,
                },
            );
        }
        notes
    }

    /// The snapshot of agent `slug`, if one of its chats uses it.
    pub fn agent_link(&self, slug: &str) -> Option<&AgentLink> {
        self.agents.iter().find(|a| a.slug == slug)
    }

    /// The agents its open and closed chats use, first use first.
    pub fn chat_agents(&self) -> Vec<&str> {
        let mut slugs: Vec<&str> = Vec::new();
        for slug in (self.chats.iter().chain(&self.closed)).filter_map(|c| c.agent.as_deref()) {
            if !slugs.contains(&slug) {
                slugs.push(slug);
            }
        }
        slugs
    }

    /// Keeps one snapshot per agent its chats use: known ones stay as they are, new ones are taken
    /// from `agents`, unused or deleted ones go. Whether anything changed.
    pub fn link_agents(&mut self, agents: &[Agent]) -> bool {
        let used = self.chat_agents();
        let links: Vec<AgentLink> = used
            .iter()
            .filter(|slug| agents.iter().any(|a| &a.slug == *slug))
            .filter_map(|slug| {
                self.agent_link(slug)
                    .cloned()
                    .or_else(|| agents.iter().find(|a| a.slug == *slug).map(AgentLink::from))
            })
            .collect();
        let changed = links != self.agents;
        self.agents = links;
        changed
    }

    /// Its repos as (name, worktree path).
    pub fn repo_paths(&self) -> Vec<(String, PathBuf)> {
        (self.repos.iter())
            .map(|r| (r.name.clone(), self.dir().join(&r.name)))
            .collect()
    }

    /// The branch `r`'s worktree is on: its own, or the task's.
    pub fn branch_of<'a>(&'a self, r: &'a TaskRepo) -> &'a str {
        r.branch.as_deref().unwrap_or(&self.branch)
    }

    /// Gives a task saved before chat tabs (or a new one) its first chat.
    pub fn migrate(&mut self) {
        if self.instance_id.is_empty() {
            self.instance_id = task_instance_id();
        }
        if self.chats.is_empty() {
            self.chats.push(ChatMeta {
                id: self.name.clone(),
                agent: (self.agent.as_ref().or(self.agents.first())).map(|a| a.slug.clone()),
                session_id: self.session_id.take(),
                model: self.model.take(),
                ..Default::default()
            });
        }
        self.active_chat = self.active_chat.min(self.chats.len() - 1);
        self.normalize_tabs();
    }

    /// Drops duplicate and stale entries, then appends open chats and feature tabs
    /// the strip lacks: a task saved before `tab_order` gets its chats, then features. Open
    /// chats follow the strip's order, so chat-only cycling matches it.
    pub fn normalize_tabs(&mut self) {
        let mut seen = HashSet::new();
        let (diff, git, artifacts) = (
            crate::features::Feature::Diff.is_open(self),
            crate::features::Feature::Git.is_open(self),
            crate::features::Feature::Artifacts.is_open(self),
        );
        let chats = &self.chats;
        self.tab_order.retain(|t| {
            let live = match t {
                TabId::Chat(id) => chats.iter().any(|c| &c.id == id),
                TabId::Diff => diff,
                TabId::Git => git,
                TabId::Artifacts => artifacts,
                TabId::File(_) => true,
            };
            live && seen.insert(t.clone())
        });
        let missing: Vec<TabId> = (self.chats.iter())
            .map(|c| TabId::Chat(c.id.clone()))
            .chain(diff.then_some(TabId::Diff))
            .chain(git.then_some(TabId::Git))
            .chain(artifacts.then_some(TabId::Artifacts))
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

    /// Appends `meta` as the last chat tab; returns its index.
    pub fn push_chat(&mut self, meta: ChatMeta) -> usize {
        self.open_tab(TabId::Chat(meta.id.clone()));
        self.chats.push(meta);
        self.chats.len() - 1
    }

    /// Removes chat `id`; the last one gives way to `fresh` (renamed) in its tab slot. Unless
    /// `untouched` says it's empty, it is kept in `closed`, the newest 20.
    pub fn remove_chat(
        &mut self,
        id: &str,
        fresh: ChatMeta,
        untouched: impl FnOnce(&ChatMeta) -> bool,
    ) -> Option<RemovedChat> {
        let ix = self.chats.iter().position(|c| c.id == id)?;
        let selected = self.chats[self.active_chat].id.clone();
        // Reserve before removing history so evicted IDs cannot be reused.
        let fresh_id = self.next_chat_id();
        let meta = self.chats.remove(ix);
        let last = self.chats.is_empty();
        if last {
            // The replacement takes the closed chat's slot.
            self.replace_tab(&TabId::Chat(id.to_string()), TabId::Chat(fresh_id.clone()));
            self.chats.push(ChatMeta {
                id: fresh_id,
                ..fresh
            });
        }
        self.active_chat = (self.chats.iter().position(|c| c.id == selected))
            .unwrap_or_else(|| ix.min(self.chats.len() - 1));
        self.normalize_tabs();
        let untouched = untouched(&meta);
        if !untouched {
            self.closed.push(meta.clone());
        }
        // ponytail: keeps the 20 newest closed chats; a closed-chats browser can lift this
        let drop = self.closed.len().saturating_sub(20);
        let evicted = self.closed.drain(..drop).collect();
        Some(RemovedChat {
            meta,
            last,
            untouched,
            evicted,
        })
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

    /// Chats without a model (an old one on the CLI default, a new task's first) get
    /// `model` and `effort`; old ones on codex's default get the model it was.
    pub fn fill_models(&mut self, model: &str, effort: Option<&String>) {
        for c in self.chats.iter_mut().chain(&mut self.closed) {
            match c.model.as_deref() {
                None => {
                    // A claude session can't resume on codex.
                    let claude = c.session_id.is_some() && crate::codex::is_codex(Some(model));
                    c.model = Some(
                        if claude {
                            crate::models::MODELS[0]
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

    /// Reserves a fresh chat id, never reused after history eviction: `<ws>~2`, `<ws>~3`, …
    pub fn next_chat_id(&mut self) -> String {
        let n = (self.chats.iter().chain(&self.closed))
            .filter_map(|c| c.id.rsplit_once('~')?.1.parse::<u64>().ok())
            .max()
            .unwrap_or(1);
        self.chat_sequence = n.max(self.chat_sequence) + 1;
        format!("{}~{}", self.name, self.chat_sequence)
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

/// The first unused `state.json.bak-N`; numbered so a later backup never replaces an earlier one.
fn backup_path(path: &Path) -> PathBuf {
    (0..)
        .map(|n| path.with_extension(format!("json.bak-{n}")))
        .find(|p| !p.exists())
        .unwrap()
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return State {
                    chat_agents_migrated: true,
                    ..State::default()
                };
            }
            // Saving would replace a file we never read.
            Err(_) => {
                return State {
                    read_only: true,
                    ..State::default()
                };
            }
        };
        let mut state: State = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            // Keep the unreadable file instead of overwriting it on the next save.
            let bak = backup_path(path);
            let moved = fs::rename(path, &bak);
            eprintln!(
                "state.json unreadable ({e}), moved to {}: {moved:?}",
                bak.display()
            );
            State {
                chat_agents_migrated: true,
                read_only: moved.is_err(),
                corrupt_backup: moved.ok().map(|_| bak),
                ..State::default()
            }
        });
        let migrate_agents = !state.chat_agents_migrated;
        let migrated_instances = state.tasks.iter().any(|ws| ws.instance_id.is_empty());
        let (model, effort) = (state.new_model(), state.last_effort.clone());
        let agents = state.agents.clone();
        let mut linked = false;
        for ws in &mut state.tasks {
            ws.migrate();
            ws.fill_models(&model, effort.as_ref());
            if migrate_agents && let Some(agent) = &ws.agent {
                for chat in ws.chats.iter_mut().chain(&mut ws.closed) {
                    if chat.agent.is_none() {
                        chat.agent = Some(agent.slug.clone());
                    }
                }
            }
            // A task-wide agent becomes the snapshot of the agent its chats use.
            if let Some(agent) = ws.agent.take() {
                ws.agents.retain(|a| a.slug != agent.slug);
                ws.agents.insert(0, agent);
                linked = true;
            }
            linked |= ws.link_agents(&agents);
            if ws.linear.primary().is_some() {
                // Unreadable shows as missing snapshots; `put_snapshot` still won't overwrite it.
                ws.issues = crate::linear::read_snapshots(&ws.name).unwrap_or_default();
            }
        }
        state.chat_agents_migrated = true;
        if migrated_instances || migrate_agents || linked {
            let _ = state.save_at(path);
        }
        state
    }

    /// The model and level a new chat in `ws` starts on: each from its agent, else the first of
    /// its repos that sets it, else the last picked.
    pub fn defaults_for(&self, ws: &Task, agent: Option<&str>) -> (String, Option<String>) {
        let valid = |m: &String| crate::models::MODELS.contains(&m.as_str());
        let mut settings = (ws.repos.iter()).filter_map(|r| self.repo_settings.get(&r.name));
        let agent = agent.and_then(|slug| self.agents.iter().find(|a| a.slug == slug));
        let model = agent
            .and_then(|a| a.model.clone().filter(valid))
            .or_else(|| settings.clone().find_map(|s| s.model.clone().filter(valid)));
        let effort = agent
            .and_then(|a| a.effort.clone())
            .or_else(|| settings.find_map(|s| s.effort.clone()));
        (
            model.unwrap_or_else(|| self.new_model()),
            effort.or(self.last_effort.clone()),
        )
    }

    /// The model a new chat gets: the last one picked, else the first.
    pub fn new_model(&self) -> String {
        let models = crate::models::MODELS;
        let last = self.last_model.as_deref().filter(|m| models.contains(m));
        last.unwrap_or(models[0]).into()
    }

    /// A new chat `id` in `ws` for `agent`, on its defaults (`defaults_for`).
    pub fn new_chat(&self, ws: &Task, id: String, agent: Option<String>) -> ChatMeta {
        let agent = agent.filter(|slug| self.agents.iter().any(|a| &a.slug == slug));
        let (model, effort) = self.defaults_for(ws, agent.as_deref());
        ChatMeta {
            id,
            agent,
            model: Some(model),
            effort,
            ..Default::default()
        }
    }

    /// Forgets task `name` everywhere the state lists it.
    pub fn remove_task(&mut self, name: &str) {
        self.tasks.retain(|w| w.name != name);
        self.reviewed.remove(name);
        self.merge_order.remove(name);
        for s in &mut self.sections {
            s.tasks.retain(|w| w != name);
        }
    }

    /// Moves task `ws` into section `to`, or out of all sections for `None`.
    pub fn move_to_section(&mut self, ws: &str, to: Option<usize>) {
        let from = (self.sections.iter()).position(|s| s.tasks.iter().any(|w| w == ws));
        if from == to {
            return;
        }
        for s in &mut self.sections {
            s.tasks.retain(|w| w != ws);
        }
        if let Some(s) = to.and_then(|i| self.sections.get_mut(i)) {
            s.tasks.push(ws.to_string());
        }
    }

    /// Drag-and-drop of task `ws` onto task `target`: `ws` takes the target's slot in the
    /// target's section (or among the ungrouped), like a list reorder.
    pub fn drop_task(&mut self, ws: &str, target: &str) {
        let section_of =
            |st: &State, n: &str| (st.sections.iter()).position(|s| s.tasks.iter().any(|w| w == n));
        let (from, to) = (section_of(self, ws), section_of(self, target));
        let pos = |v: &[String], n: &str| v.iter().position(|w| w == n);
        let task = |st: &State, n: &str| st.tasks.iter().position(|w| w.name == n);
        if ws == target || task(self, ws).is_none() || task(self, target).is_none() {
            return;
        }
        if let Some(i) = to
            && from == to
        {
            let v = &mut self.sections[i].tasks;
            if let (Some(f), Some(t)) = (pos(v, ws), pos(v, target)) {
                move_item(v, f, t);
            }
            return;
        }
        for s in &mut self.sections {
            s.tasks.retain(|w| w != ws);
        }
        match to {
            Some(i) => {
                let v = &mut self.sections[i].tasks;
                let t = pos(v, target).unwrap_or(v.len());
                v.insert(t, ws.to_string());
            }
            None => {
                let (Some(f), Some(t)) = (task(self, ws), task(self, target)) else {
                    return;
                };
                // Coming from a section it lands before the target; within the list it takes the slot.
                let t = match from {
                    Some(_) if f < t => t - 1,
                    _ => t,
                };
                move_item(&mut self.tasks, f, t);
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
    fn pinned_artifacts_default_empty_and_roundtrip() {
        let mut task: Task =
            serde_json::from_str(r#"{"name":"old","branch":"main","repos":[]}"#).unwrap();
        assert!(task.artifacts_pinned.is_empty());
        let json = serde_json::to_value(&task).unwrap();
        assert!(json.get("artifacts_pinned").is_none());
        task.artifacts_pinned = vec!["plans/a.md".into()];
        let saved = serde_json::to_string(&task).unwrap();
        let back: Task = serde_json::from_str(&saved).unwrap();
        assert_eq!(back.artifacts_pinned, ["plans/a.md"]);
    }

    #[test]
    fn backlog_defaults_false_and_roundtrips() {
        let mut task: Task =
            serde_json::from_str(r#"{"name":"old","branch":"main","repos":[]}"#).unwrap();
        assert!(!task.backlog);
        assert!(
            serde_json::to_value(&task)
                .unwrap()
                .get("backlog")
                .is_none()
        );
        task.backlog = true;
        let saved = serde_json::to_string(&task).unwrap();
        assert!(serde_json::from_str::<Task>(&saved).unwrap().backlog);
    }

    #[test]
    fn legacy_agent_icons_and_variants_roundtrip() {
        let legacy = r#"{"repos":[],"workspaces":[],"agents":[{"slug":"a","name":"A","icon":"Palette","instructions":"","repos":[],"model":null,"effort":null}]}"#;
        let mut state: State = serde_json::from_str(legacy).unwrap();
        assert_eq!(state.agents[0].icon, AgentIcon::from("Palette"));
        for icon in [
            AgentIcon::default(),
            AgentIcon::Symbol {
                name: "star".into(),
                tint: Some("#ff0000".into()),
            },
            AgentIcon::Emoji {
                value: "🦈".into()
            },
            AgentIcon::Image {
                file: "agent-icons/a.png".into(),
            },
        ] {
            state.agents[0].icon = icon.clone();
            let saved = serde_json::to_string(&state).unwrap();
            assert_eq!(
                serde_json::from_str::<State>(&saved).unwrap().agents[0].icon,
                icon
            );
        }
    }

    #[test]
    fn legacy_state_agent_without_icon_loads() {
        let state: State = serde_json::from_str(r#"{"repos":[],"workspaces":[],"agents":[{"slug":"a","name":"A","instructions":"","repos":[],"model":null,"effort":null}]}"#).unwrap();
        assert_eq!(
            serde_json::to_value(&state.agents[0].icon).unwrap(),
            serde_json::json!({"kind":"symbol","name":"bot"})
        );
    }

    #[test]
    fn handoff_fields_roundtrip_and_legacy_defaults() {
        let agent: Agent = serde_json::from_str(r#"{"slug":"designer","name":"Designer","icon":"Palette","instructions":"Design","repos":[],"model":null,"effort":null}"#).unwrap();
        assert!(agent.next.is_none());
        assert!(serde_json::to_value(&agent).unwrap().get("next").is_none());
        // A quote from the handoff before transcripts were attached is dropped.
        let mut chat: ChatMeta = serde_json::from_str(
            r#"{"id":"legacy","handoff":{"from":"Designer","title":"Ideas","text":"Build"}}"#,
        )
        .unwrap();
        assert!(chat.agent.is_none());
        let value = serde_json::to_value(&chat).unwrap();
        assert!(value.get("agent").is_none() && value.get("handoff").is_none());
        chat.agent = Some("planner".into());
        assert_eq!(
            serde_json::from_value::<ChatMeta>(serde_json::to_value(&chat).unwrap()).unwrap(),
            chat
        );
        let mut agent = agent;
        agent.next = Some("planner".into());
        assert_eq!(
            serde_json::from_value::<Agent>(serde_json::to_value(&agent).unwrap()).unwrap(),
            agent
        );
        let task = Task {
            agents: vec![AgentLink::from(&agent)],
            ..Default::default()
        };
        let mut initial = task.clone();
        initial.migrate();
        assert_eq!(initial.chats[0].agent.as_deref(), Some("designer"));
        let state = State {
            agents: vec![agent],
            ..Default::default()
        };
        let pick = |a: Option<&str>| state.new_chat(&task, "new".into(), a.map(Into::into)).agent;
        assert_eq!(pick(Some("designer")).as_deref(), Some("designer"));
        assert_eq!(pick(None), None, "a task has no agent of its own");
        assert_eq!(pick(Some("deleted")), None);
    }

    #[test]
    fn legacy_task_agent_becomes_the_snapshot_of_its_chats_agents() {
        let path = std::env::temp_dir().join(format!("shark-agents-{}.json", std::process::id()));
        let designer = Agent {
            slug: "designer".into(),
            name: "Designer".into(),
            instructions: "Use Mobbin".into(),
            ..Default::default()
        };
        let coder = Agent {
            slug: "coder".into(),
            name: "Coder".into(),
            ..Default::default()
        };
        let legacy = serde_json::json!({
            "repos": [], "chat_agents_migrated": true,
            "agents": [designer, coder],
            "workspaces": [{
                "name": "t", "branch": "t", "repos": [],
                "agent": {"slug": "designer", "name": "Designer", "instructions": "Use Mobbin"},
                "chats": [{"id": "t", "agent": "designer"}, {"id": "t-2", "agent": "coder"}],
            }],
        });
        fs::write(&path, legacy.to_string()).unwrap();
        let loaded = State::load_at(&path);
        let slugs: Vec<_> = loaded.tasks[0]
            .agents
            .iter()
            .map(|a| a.slug.as_str())
            .collect();
        assert_eq!(slugs, ["designer", "coder"]);
        assert!(loaded.tasks[0].agent.is_none());
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(
            saved["workspaces"][0].get("agent").is_none(),
            "only `agents` is written"
        );
        let mut task = loaded.tasks[0].clone();
        task.chats
            .retain(|c| c.agent.as_deref() == Some("designer"));
        assert!(task.link_agents(&loaded.agents));
        assert_eq!(task.agents.len(), 1, "an unused agent's snapshot goes");
        assert!(task.link_agents(&[]), "so does a deleted one's");
        assert!(task.agents.is_empty());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn load_tags_legacy_open_and_closed_chats_only_in_agent_tasks() {
        let path = std::env::temp_dir().join(format!(
            "shark-handoff-migration-{}.json",
            std::process::id()
        ));
        let mut task = Task {
            name: "design".into(),
            agent: Some(AgentLink {
                slug: "designer".into(),
                ..Default::default()
            }),
            chats: vec![
                chat("open"),
                ChatMeta {
                    agent: Some("previous".into()),
                    ..chat("tagged")
                },
            ],
            closed: vec![chat("closed")],
            ..Default::default()
        };
        let mut state = State {
            tasks: vec![task.clone()],
            ..Default::default()
        };
        task.name = "plain".into();
        task.agent = None;
        state.tasks.push(task);
        // Only installs from before agents were per chat wrote a task's agent.
        let mut saved = serde_json::to_value(&state).unwrap();
        saved["workspaces"][0]["agent"] =
            serde_json::json!({"slug": "designer", "name": "", "instructions": ""});
        fs::write(&path, saved.to_string()).unwrap();
        let mut loaded = State::load_at(&path);
        assert_eq!(loaded.tasks[0].chats[0].agent.as_deref(), Some("designer"));
        assert_eq!(loaded.tasks[0].closed[0].agent.as_deref(), Some("designer"));
        assert_eq!(loaded.tasks[0].chats[1].agent.as_deref(), Some("previous"));
        assert!(loaded.tasks[1].chats[0].agent.is_none());
        assert!(loaded.tasks[1].closed[0].agent.is_none());
        loaded.tasks[1].agents = loaded.tasks[0].agents.clone();
        loaded.save_at(&path).unwrap();
        let reloaded = State::load_at(&path);
        assert!(
            reloaded.tasks[1].chats[0].agent.is_none(),
            "handoff must not retag unassigned chats on restart"
        );
        assert!(reloaded.tasks[1].closed[0].agent.is_none());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn agent_defaults_and_instruction_context() {
        let mut state = State::default();
        state.repo_settings.insert(
            "api".into(),
            RepoSettings {
                model: Some(crate::models::MODELS[1].into()),
                effort: Some("low".into()),
                ..Default::default()
            },
        );
        state.agents.push(Agent {
            slug: "designer".into(),
            model: Some(crate::models::MODELS[0].into()),
            effort: Some("high".into()),
            ..Default::default()
        });
        let mut task = Task {
            agents: vec![AgentLink {
                slug: "designer".into(),
                name: "Designer".into(),
                instructions: "Use Mobbin".into(),
                ..Default::default()
            }],
            repos: vec![TaskRepo {
                name: "api".into(),
                src: "/missing".into(),
                base: "main".into(),
                branch: None,
            }],
            ..Default::default()
        };
        assert_eq!(
            state.defaults_for(&task, Some("designer")),
            (crate::models::MODELS[0].into(), Some("high".into()))
        );
        assert_eq!(
            state.defaults_for(&task, None),
            (crate::models::MODELS[1].into(), Some("low".into())),
            "without an agent the repo's defaults apply"
        );
        state.agents[0].model = Some("invalid model".into());
        state.agents[0].effort = None;
        assert_eq!(
            state.defaults_for(&task, Some("designer")),
            (crate::models::MODELS[1].into(), Some("low".into()))
        );
        let before = task.context()["agent:designer"].clone();
        assert!(before.text.contains("Use Mobbin"));
        task.agents[0].name = "Design".into();
        assert_eq!(
            task.context()["agent:designer"].fingerprint,
            before.fingerprint
        );
        task.agents[0].instructions = "Read the manual".into();
        assert_ne!(
            task.context()["agent:designer"].fingerprint,
            before.fingerprint
        );
        assert_eq!(slug("  Design / UI! "), "design-ui");
        assert_eq!(slug("!!!"), "");
    }

    #[test]
    fn pre_rename_state_round_trips_unchanged() {
        let saved = include_str!("../tests/fixtures/pre-task-rename-state.json");
        let state: State = serde_json::from_str(saved).unwrap();
        assert_eq!(state.tasks[0].name, "agent-123");
        assert!(state.tasks[0].agent.is_none());
        assert_eq!(state.sections[0].tasks, ["agent-123"]);
        assert_eq!(serde_json::to_string_pretty(&state).unwrap(), saved);
    }

    #[test]
    fn tests_never_touch_the_real_home() {
        for dir in [support_dir(), tasks_dir()] {
            assert!(dir.starts_with(std::env::temp_dir()), "{}", dir.display());
        }
    }

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

    /// A repo saved before per-repo branches is on the task branch, and one on the
    /// task branch doesn't save the field.
    #[test]
    fn repo_branch_is_optional() {
        let old = r#"{"name":"w","branch":"feat","repos":[{"name":"api","src":"/x","base":"origin/main"}]}"#;
        let mut ws: Task = serde_json::from_str(old).unwrap();
        assert_eq!(ws.repos[0].branch, None);
        assert_eq!(ws.branch_of(&ws.repos[0]), "feat");
        assert!(
            !serde_json::to_string(&ws)
                .unwrap()
                .contains("\"branch\":null")
        );
        ws.repos[0].branch = Some("kd/x".into());
        let back: Task = serde_json::from_str(&serde_json::to_string(&ws).unwrap()).unwrap();
        assert_eq!(back.branch_of(&back.repos[0]), "kd/x");
    }

    /// Tasks saved before Artifacts load with it closed; an open one keeps its tab.
    #[test]
    fn artifacts_state_defaults_closed_and_normalizes() {
        let mut ws: Task =
            serde_json::from_str(r#"{"name":"old","branch":"main","repos":[]}"#).unwrap();
        ws.migrate();
        assert!(ws.artifacts_closed);
        assert!(!ws.tab_order.contains(&TabId::Artifacts));
        assert_eq!(
            serde_json::to_string(&TabId::Artifacts).unwrap(),
            r#""artifacts""#
        );
        ws.artifacts_closed = false;
        ws.normalize_tabs();
        assert!(ws.tab_order.contains(&TabId::Artifacts));
        let saved = serde_json::to_string(&ws).unwrap();
        let mut back: Task = serde_json::from_str(&saved).unwrap();
        back.migrate();
        assert_eq!(back.tab_order, ws.tab_order);
        back.artifacts_closed = true;
        back.normalize_tabs();
        assert!(!back.tab_order.contains(&TabId::Artifacts));
    }

    /// Old tasks get their chats then a shown Diff; the saved strip mixes kinds, round
    /// trips per task, and drops duplicates and stale entries without inventing files.
    #[test]
    fn tab_order_migrates_normalizes_and_moves() {
        use TabId::{Chat, Diff, File};
        let chat = |id: &str| Chat(id.into());
        let file = |p: &str| File(p.into());
        let old = r#"{"name":"w","branch":"b","repos":[],"diff_closed":false,
            "chats":[{"id":"w"},{"id":"w~2"}],"active_chat":1}"#;
        let mut ws: Task = serde_json::from_str(old).unwrap();
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

        let mut other = Task {
            name: "x".into(),
            ..Default::default()
        };
        other.migrate();
        let st = State {
            tasks: vec![ws.clone(), other],
            ..Default::default()
        };
        let back: State = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
        assert_eq!(back.tasks[0].tab_order, ws.tab_order);
        assert_eq!(back.tasks[1].tab_order, [chat("x")]);

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

    /// A pre-tabs task keeps its session, model and transcript as its first chat.
    /// Files from before attachments load, and a recorded one survives a round trip.
    #[test]
    fn issue_attachments_default_and_round_trip_without_their_text() {
        let old = r#"{"name":"a","branch":"b","repos":[]}"#;
        let mut ws: Task = serde_json::from_str(old).unwrap();
        assert_eq!(ws.linear.primary(), None);
        let snapshot = crate::linear::Snapshot {
            org: "o".into(),
            id: "i".into(),
            identifier: "ENG-1".into(),
            description: "secret plans".into(),
            ..Default::default()
        };
        ws.linear.attach((&snapshot).into());
        ws.issues.push(snapshot.clone());
        let out = serde_json::to_string(&ws).unwrap();
        assert!(
            !out.contains("secret plans") && !out.contains("ENG-1"),
            "{out}"
        );
        let back: Task = serde_json::from_str(&out).unwrap();
        assert_eq!(back.linear, ws.linear);
        assert!(back.issues.is_empty(), "their file owns them");
    }

    #[test]
    fn the_primary_issue_names_a_first_branch_and_context_holds_each_issue() {
        let issue = |id: &str, n: u32| crate::linear::Snapshot {
            org: "o".into(),
            id: id.into(),
            identifier: format!("ENG-{n}"),
            title: "Hostile: title".into(),
            ..Default::default()
        };
        let (one, two) = (issue("uuid-1", 1), issue("uuid-2", 2));
        let mut ws = Task {
            name: "agent-19a2b".into(),
            title: "Some title".into(),
            ..Default::default()
        };
        assert_eq!(ws.default_branch(), "some-title-19a2b");
        ws.linear.attach((&one).into());
        ws.linear.attach((&two).into());
        ws.issues = vec![one, two.clone()];
        assert_eq!(ws.default_branch(), "eng-1-19a2b");
        ws.linear.make_primary(&(&two).into());
        assert_eq!(ws.default_branch(), "eng-2-19a2b");
        ws.title = NEW_TASK.into();
        assert_eq!(ws.default_branch(), "eng-2-19a2b");
        let context = ws.context();
        let keys: Vec<&String> = context.keys().collect();
        assert_eq!(keys, ["issue:uuid-1", "issue:uuid-2"]);
        assert!(context.values().all(|n| n.text.starts_with("[shark] ")));
        ws.branch = "chosen".into();
        ws.repos.push(TaskRepo {
            name: "api".into(),
            src: "/src/api".into(),
            base: "main".into(),
            branch: None,
        });
        assert_eq!(ws.default_branch(), "chosen", "an existing branch stays");
    }

    #[test]
    fn attachments_and_delivered_context_default_and_round_trip() {
        let json = r#"{"name":"feat","branch":"b","repos":[],"chats":[{"id":"feat"}]}"#;
        let mut ws: Task = serde_json::from_str(json).unwrap();
        assert!(ws.attaching.is_empty() && ws.chats[0].delivered.is_empty());
        let out = serde_json::to_string(&ws).unwrap();
        assert!(
            !out.contains("attaching") && !out.contains("delivered"),
            "{out}"
        );
        ws.attaching.push(Attachment {
            stage: AttachStage::SetupFailed,
            error: Some("exit 3".into()),
            ..Attachment::fresh(
                &Repo {
                    name: "api".into(),
                    path: "/src/api".into(),
                },
                "b",
                "origin/dev",
            )
        });
        ws.chats[0].delivered.insert("repo:api".into(), 7);
        let back: Task = serde_json::from_str(&serde_json::to_string(&ws).unwrap()).unwrap();
        assert_eq!(back.attaching, ws.attaching);
        assert_eq!(back.chats, ws.chats);
    }

    /// Reproducible desktop workload: 20 issues with 32 KiB descriptions, 100 context updates.
    /// Run with `cargo test context_fingerprint_workload -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn context_fingerprint_workload() {
        let mut ws = Task {
            name: "context-workload".into(),
            ..Default::default()
        };
        for i in 0..20 {
            let issue = crate::linear::Snapshot {
                org: "org".into(),
                id: format!("issue-{i}"),
                identifier: format!("ENG-{i}"),
                description: "A description with <tags> and [shark] notes.\n".repeat(800),
                ..Default::default()
            };
            ws.linear.attach((&issue).into());
            ws.issues.push(issue);
        }
        let expected = context_record(&ws.context());
        let started = std::time::Instant::now();
        for _ in 0..100 {
            assert_eq!(
                context_record(&std::hint::black_box(&ws).context()),
                expected
            );
        }
        eprintln!(
            "100 context updates, 20 issues / {} KiB descriptions: {:?}",
            ws.issues.iter().map(|s| s.description.len()).sum::<usize>() / 1024,
            started.elapsed()
        );
        ws.issues[0].description.push_str("Changed");
        let changed = context_record(&ws.context());
        assert_eq!(
            expected
                .iter()
                .filter(|(key, value)| changed.get(*key) != Some(*value))
                .count(),
            1
        );
    }

    /// A default branch is the title's words plus the agent's suffix; an untitled agent has only
    /// its identity. Readiness needs a repo and no attachment left.
    #[test]
    fn default_branch_and_readiness() {
        let mut ws = Task {
            name: "agent-19a2b".into(),
            title: NEW_TASK.into(),
            branch: "agent-19a2b".into(),
            ..Default::default()
        };
        assert_eq!(ws.default_branch(), "agent-19a2b");
        ws.title = "Fix: the Login page — properly, please, before the long weekend!".into();
        assert_eq!(
            ws.default_branch(),
            "fix-the-login-page-properly-please-19a2b"
        );
        assert!(!ws.ready());
        ws.repos.push(TaskRepo {
            name: "api".into(),
            src: "/src/api".into(),
            base: "main".into(),
            branch: None,
        });
        ws.branch = "chosen".into();
        assert_eq!(ws.default_branch(), "chosen", "later repos inherit it");
        assert!(ws.ready());
        let r = Repo {
            name: "web".into(),
            path: "/src/web".into(),
        };
        ws.attaching.push(Attachment::fresh(&r, "chosen", ""));
        assert!(!ws.ready());
    }

    #[test]
    fn migrates_single_chat() {
        let json = r#"{"name":"feat","branch":"b","repos":[],"session_id":"s1","model":"opus"}"#;
        let mut ws: Task = serde_json::from_str(json).unwrap();
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
                delivered: Default::default(),
                ..Default::default()
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
            let w = &st.tasks[0];
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
        let new = st.new_chat(&st.tasks[0], "x".into(), None);
        assert_eq!(
            (new.model.as_deref(), new.effort.as_deref()),
            (Some("haiku"), Some("max"))
        );
        // A repo's default beats the last pick; its level falls back to the last one.
        let mut st = st;
        st.tasks[0].repos = vec![TaskRepo {
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
        let new = st.new_chat(&st.tasks[0], "y".into(), None);
        assert_eq!(
            (new.model.as_deref(), new.effort.as_deref()),
            (Some("opus"), Some("max"))
        );
        // Model and level each come from the first repo that sets them.
        let web = TaskRepo {
            name: "web".into(),
            ..st.tasks[0].repos[0].clone()
        };
        st.tasks[0].repos.insert(0, web);
        st.repo_settings.insert(
            "web".into(),
            RepoSettings {
                effort: Some("low".into()),
                ..Default::default()
            },
        );
        let new = st.new_chat(&st.tasks[0], "z".into(), None);
        assert_eq!(
            (new.model.as_deref(), new.effort.as_deref()),
            (Some("opus"), Some("low"))
        );
        let mut w: Task = serde_json::from_str(
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

    /// A task saved before titles shows its name and saves back loadable.
    #[test]
    fn old_state_has_no_title() {
        let old =
            r#"{"repos":[],"workspaces":[{"name":"kd-login","branch":"kd/login","repos":[]}]}"#;
        let st: State = serde_json::from_str(old).unwrap();
        let ws = &st.tasks[0];
        assert_eq!((ws.title.as_str(), &*ws.display_title()), ("", "kd-login"));
        let back: State = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
        assert_eq!(back.tasks[0].display_title(), "kd-login");
    }

    /// A generated name comes from the table and skips what's taken; with the table used up, it
    /// counts up.
    #[test]
    fn task_names_skip_taken_ones() {
        assert!(SEA_NAMES.contains(&task_name(0, |_| false).as_str()));
        assert_eq!(task_name(0, |n| n != "marlin"), "marlin");
        assert_eq!(task_name(0, |n| n != "marlin-2"), "marlin-2");
        let bare = |n: &str| SEA_NAMES.contains(&n);
        let numbered = task_name(0, bare);
        assert!(bare(numbered.strip_suffix("-2").unwrap()), "{numbered}");
        let next = task_name(0, |n| bare(n) || n.ends_with("-2"));
        assert!(bare(next.strip_suffix("-3").unwrap()), "{next}");
    }

    #[test]
    fn sea_names_are_unique_directories_and_branches() {
        assert!(SEA_NAMES.len() >= 150);
        assert_eq!(
            SEA_NAMES.iter().collect::<HashSet<_>>().len(),
            SEA_NAMES.len()
        );
        for name in SEA_NAMES {
            let letters = |w: &str| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase());
            assert!(name.split('-').all(letters), "{name}");
            assert!(!name.starts_with("agent-"), "{name}");
            let args = ["check-ref-format", "--branch", name];
            crate::git::git(&std::env::temp_dir(), &args).unwrap();
        }
    }

    #[test]
    fn untitled_agents_show_and_branch_on_their_name() {
        let mut ws = Task {
            name: "marlin".into(),
            title: NEW_TASK.into(),
            ..Default::default()
        };
        assert_eq!(ws.display_title(), "Marlin");
        assert_eq!(ws.default_branch(), "marlin");
        ws.name = "mahi-mahi-2".into();
        assert_eq!(ws.display_title(), "Mahi-mahi-2");
        assert_eq!(ws.default_branch(), "mahi-mahi-2");
        ws.title = "Fix login".into();
        assert_eq!(ws.display_title(), "Fix login");
        assert_eq!(ws.default_branch(), "fix-login-mahi-mahi-2");
    }

    /// An `agent-<hex>` saved before `SEA_NAMES` looks and branches as it did.
    #[test]
    fn old_task_names_stay_as_they_were() {
        let mut ws = Task {
            name: "agent-19a2b".into(),
            title: NEW_TASK.into(),
            ..Default::default()
        };
        assert_eq!(ws.display_title(), "New task");
        assert_eq!(ws.default_branch(), "agent-19a2b");
        ws.title = "Fix login".into();
        assert_eq!(ws.default_branch(), "fix-login-19a2b");
    }

    #[test]
    fn diff_closure_defaults_and_round_trips() {
        let legacy = r#"{"name":"legacy","branch":"main","repos":[]}"#;
        let mut ws: Task = serde_json::from_str(legacy).unwrap();
        assert!(ws.diff_closed);
        assert!(Task::default().diff_closed);
        for closed in [false, true] {
            ws.diff_closed = closed;
            let saved = serde_json::to_string(&ws).unwrap();
            assert_eq!(
                serde_json::from_str::<Task>(&saved).unwrap().diff_closed,
                closed
            );
        }
    }

    #[test]
    fn remove_task_clears_every_list() {
        let task = |n: &str| Task {
            name: n.into(),
            ..Default::default()
        };
        let mut st = State {
            tasks: vec![task("a"), task("b")],
            sections: vec![Section {
                name: "S".into(),
                icon: "folder".into(),
                collapsed: false,
                tasks: vec!["a".into(), "b".into()],
            }],
            ..Default::default()
        };
        st.reviewed.insert("a".into(), vec!["f".into()]);
        st.reviewed.insert("b".into(), vec![]);
        st.merge_order.insert("a".into(), vec!["b".into()]);
        st.remove_task("a");
        assert_eq!(st.tasks.len(), 1);
        assert_eq!(st.sections[0].tasks, ["b"]);
        assert_eq!(st.reviewed.keys().collect::<Vec<_>>(), ["b"]);
        assert!(st.merge_order.is_empty());
    }

    fn chat(id: &str) -> ChatMeta {
        ChatMeta {
            id: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn remove_last_chat_keeps_the_slot_with_a_fresh_one() {
        let mut ws = Task {
            name: "w".into(),
            diff_closed: false,
            ..Default::default()
        };
        ws.push_chat(chat("w"));
        ws.open_tab(TabId::Diff);
        let r = ws.remove_chat("w", chat(""), |_| false).unwrap();
        assert!(r.last && !r.untouched && r.evicted.is_empty());
        assert_eq!(ws.chats.len(), 1);
        assert_eq!(ws.chats[0].id, "w~2");
        assert_eq!(ws.tab_order, [TabId::Chat("w~2".into()), TabId::Diff]);
        assert_eq!(ws.closed, [chat("w")]);
        assert!(ws.remove_chat("gone", chat(""), |_| false).is_none());
    }

    #[test]
    fn workspace_instance_migration_is_saved_and_stable() {
        let dir = std::env::temp_dir().join(format!("shark-instance-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        fs::write(
            &path,
            r#"{"repos":[],"workspaces":[{"name":"one","branch":"one","repos":[]}]}"#,
        )
        .unwrap();
        let first = State::load_at(&path);
        let id = &first.tasks[0].instance_id;
        assert!(!id.is_empty());
        assert_eq!(State::load_at(&path).tasks[0].instance_id, *id);
        assert_ne!(Task::default().instance_id, *id);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn chat_ids_stay_reserved_after_history_eviction_and_reload() {
        let mut ws: Task = serde_json::from_value(serde_json::json!({
            "name": "one", "branch": "one", "repos": [],
            "chats": (1..=22).map(|n| serde_json::json!({"id": format!("one~{n}")})).collect::<Vec<_>>()
        })).unwrap();
        ws.migrate();
        ws.remove_chat("one~22", chat(""), |_| false).unwrap();
        for n in 2..=21 {
            ws.remove_chat(&format!("one~{n}"), chat(""), |_| false)
                .unwrap();
        }
        assert!(!ws.closed.iter().any(|c| c.id == "one~22"));
        let mut ws: Task = serde_json::from_str(&serde_json::to_string(&ws).unwrap()).unwrap();
        ws.migrate();
        let id = ws.next_chat_id();
        assert!(id.rsplit_once('~').unwrap().1.parse::<u64>().unwrap() > 22);
        assert_ne!(ws.next_chat_id(), id, "each allocation reserves its ID");
    }

    #[test]
    fn remove_chat_skips_untouched_and_caps_closed_at_twenty() {
        let mut ws = Task {
            chats: vec![chat("a"), chat("b")],
            active_chat: 1,
            closed: (0..20).map(|i| chat(&format!("old{i}"))).collect(),
            ..Default::default()
        };
        let r = ws.remove_chat("a", chat(""), |_| false).unwrap();
        assert!(!r.last);
        assert_eq!(ws.chats[ws.active_chat].id, "b");
        assert_eq!(ws.closed.len(), 20);
        assert_eq!(ws.closed.last().unwrap().id, "a");
        assert_eq!(
            r.evicted.iter().map(|c| &c.id[..]).collect::<Vec<_>>(),
            ["old0"]
        );
        let r = ws.remove_chat("b", chat(""), |_| true).unwrap();
        assert!(r.last && r.untouched);
        assert_eq!(ws.closed.len(), 20);
        assert_eq!(ws.closed.last().unwrap().id, "a");
    }

    #[test]
    fn legacy_agent_sections_load_as_plain_sections() {
        let section: Section = serde_json::from_str(
            r#"{"agent":"designer","name":"Design","icon":"folder","workspaces":["a"]}"#,
        )
        .unwrap();
        assert_eq!((section.name.as_str(), section.tasks.len()), ("Design", 1));
        assert!(
            serde_json::to_value(&section)
                .unwrap()
                .get("agent")
                .is_none()
        );
    }

    #[test]
    fn move_to_section_is_idempotent_and_ungroups() {
        let section = |n: &str, ws: &[&str]| Section {
            name: n.into(),
            icon: "folder".into(),
            collapsed: false,
            tasks: ws.iter().map(|w| w.to_string()).collect(),
        };
        let mut st = State {
            sections: vec![section("A", &["x", "y"]), section("B", &[])],
            ..Default::default()
        };
        st.move_to_section("x", Some(0));
        assert_eq!(st.sections[0].tasks, ["x", "y"]);
        st.move_to_section("x", Some(1));
        assert_eq!(st.sections[0].tasks, ["y"]);
        assert_eq!(st.sections[1].tasks, ["x"]);
        st.move_to_section("x", None);
        assert!(st.sections[1].tasks.is_empty());
    }

    #[test]
    fn drop_task_reorders_and_regroups() {
        let task = |name: &str| Task {
            name: name.into(),
            branch: name.into(),
            ..Default::default()
        };
        let mut st = State {
            tasks: ["a", "b", "c", "d"].map(task).into(),
            sections: vec![Section {
                name: "S".into(),
                icon: "folder".into(),
                collapsed: false,
                tasks: vec!["c".into(), "d".into()],
            }],
            ..Default::default()
        };
        let names = |st: &State| st.tasks.iter().map(|w| w.name.clone()).collect::<Vec<_>>();
        st.drop_task("a", "b"); // down within the ungrouped: takes b's slot
        assert_eq!(names(&st), ["b", "a", "c", "d"]);
        st.drop_task("a", "b"); // and back up
        assert_eq!(names(&st), ["a", "b", "c", "d"]);
        st.drop_task("d", "c"); // within a section
        assert_eq!(st.sections[0].tasks, ["d", "c"]);
        st.drop_task("a", "c"); // into a section, before the target
        assert_eq!(st.sections[0].tasks, ["d", "a", "c"]);
        st.drop_task("d", "b"); // out to the ungrouped, before the target
        assert_eq!(st.sections[0].tasks, ["a", "c"]);
        let ungrouped: Vec<_> = names(&st)
            .into_iter()
            .filter(|n| !st.sections[0].tasks.contains(n))
            .collect();
        assert_eq!(ungrouped, ["d", "b"]);
        st.drop_task("gone", "b"); // a stale drag changes nothing
        assert_eq!(st.sections[0].tasks, ["a", "c"]);
    }
}

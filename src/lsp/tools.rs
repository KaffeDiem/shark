//! Project semantics are selected once, before installing anything; crashes never switch backends.
use super::install::{self, managed, path, run};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    Rust,
    Pyrefly,
    Pyright,
    TypeScript,
    Swift,
    Go,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub root: PathBuf,
    pub worktree: PathBuf,
    pub language_id: &'static str,
    pub backend: Backend,
    pub toolchain: String,
    pub rust_toolchain: Option<String>,
    pub interpreter: Option<PathBuf>,
}

impl Project {
    pub fn session_key(&self) -> String {
        format!(
            "{:?}|{:?}|{:?}|{}",
            self.worktree, self.root, self.backend, self.toolchain
        )
    }
}

pub fn language_id(file: &Path) -> Option<&'static str> {
    language(file).map(|(id, _)| id)
}

#[derive(Clone, Debug)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub root: PathBuf,
    pub initialization_options: Value,
    pub settings: Value,
    pub detail: Option<String>,
    /// The index stays incomplete until the user builds the project (Xcode).
    pub awaiting_build: bool,
    pub companion: Option<Box<Launch>>,
}

#[derive(Debug)]
pub struct XcodeChoice {
    pub options: Vec<PathBuf>,
}

impl std::fmt::Display for XcodeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Select the Xcode project or workspace for this folder")
    }
}
impl std::error::Error for XcodeChoice {}

fn xcode_selection_file(project: &Project) -> PathBuf {
    let key = crate::files::fingerprint(project.root.as_os_str().as_encoded_bytes());
    crate::state::support_dir()
        .join("language-tools")
        .join(format!("xcode-{key:016x}.json"))
}

/// Remember a container choice without changing project settings. Selection is revalidated on use.
pub fn select_xcode(project: &Project, selection: &Path) -> Result<()> {
    ensure!(
        xcode_containers(&project.root)
            .iter()
            .any(|p| p == selection),
        "Xcode project moved; Retry to discover it again"
    );
    let file = xcode_selection_file(project);
    fs::create_dir_all(file.parent().context("Missing setup directory")?)?;
    let staged = file.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(
        &staged,
        serde_json::to_vec(&selection.strip_prefix(&project.root)?)?,
    )?;
    fs::rename(staged, file)?;
    Ok(())
}

/// Blocking filesystem/toolchain discovery, bounded to the current repository worktree.
pub fn discover(file: &Path, worktree: &Path) -> Result<Option<Project>> {
    let Some((language_id, mut backend)) = language(file) else {
        return Ok(None);
    };
    let worktree = worktree.canonicalize().context("Worktree is unavailable")?;
    let file = file.canonicalize().context("Source file is unavailable")?;
    ensure!(
        file.starts_with(&worktree),
        "File is outside the current worktree"
    );
    let dirs: Vec<_> = file
        .parent()
        .context("Source file has no parent")?
        .ancestors()
        .take_while(|p| p.starts_with(&worktree))
        .collect();
    let gowork = (backend == Backend::Go).then(|| go_env("GOWORK")).flatten();
    // An explicit go.work file names the workspace wherever it sits; "off" disables workspaces.
    let explicit_go_work = gowork
        .as_deref()
        .filter(|w| *w != "off")
        .and_then(|w| Path::new(w).canonicalize().ok())
        .filter(|w| w.is_file() && w.starts_with(&worktree))
        .and_then(|w| w.parent().map(Path::to_path_buf));
    let mut root = match backend {
        Backend::Rust => dirs
            .iter()
            .find(|d| d.join("Cargo.toml").is_file())
            .copied(),
        Backend::Pyrefly | Backend::Pyright => dirs
            .iter()
            .find(|d| {
                [
                    "pyrefly.toml",
                    "pyrightconfig.json",
                    "pyproject.toml",
                    "setup.py",
                    "setup.cfg",
                    "requirements.txt",
                ]
                .iter()
                .any(|m| d.join(m).is_file())
            })
            .copied(),
        Backend::TypeScript => dirs
            .iter()
            .find(|d| {
                ["tsconfig.json", "jsconfig.json", "package.json"]
                    .iter()
                    .any(|m| d.join(m).is_file())
            })
            .copied(),
        Backend::Swift => dirs
            .iter()
            .find(|d| {
                d.join("Package.swift").is_file()
                    || d.join("buildServer.json").is_file()
                    || !xcode_containers(d).is_empty()
            })
            .copied(),
        Backend::Go => explicit_go_work
            .as_deref()
            .or_else(|| {
                dirs.iter()
                    .find(|d| gowork.as_deref() != Some("off") && d.join("go.work").is_file())
                    .copied()
            })
            .or_else(|| dirs.iter().find(|d| d.join("go.mod").is_file()).copied()),
    }
    .unwrap_or(&worktree)
    .to_path_buf();
    if backend == Backend::Pyrefly {
        // A checker config above the nearest packaging file still governs that package.
        for dir in &dirs {
            let config = read_toml(&dir.join("pyproject.toml"))?;
            if dir.join("pyrightconfig.json").is_file()
                || config.get("tool").and_then(|t| t.get("pyright")).is_some()
            {
                backend = Backend::Pyright;
                root = dir.to_path_buf();
                break;
            }
            if dir.join("pyrefly.toml").is_file()
                || config.get("tool").and_then(|t| t.get("pyrefly")).is_some()
            {
                root = dir.to_path_buf();
                break;
            }
        }
    }
    let rust_toolchain = if backend == Backend::Rust {
        check_rustup()?;
        rustup(file.parent().unwrap(), &["show", "active-toolchain"], 10)
            .ok()
            .and_then(|s| s.split_whitespace().next().map(str::to_string))
    } else {
        None
    };
    if backend == Backend::Rust {
        let mut command = Command::new("cargo");
        command.env("RUSTUP_AUTO_INSTALL", "0");
        if let Some(toolchain) = &rust_toolchain {
            command.env("RUSTUP_TOOLCHAIN", toolchain);
        }
        if let Ok(manifest) = crate::git::wait_bounded(
            command,
            &root,
            "cargo",
            &["locate-project", "--workspace", "--message-format", "plain"],
            10,
            true,
        ) && let Some(workspace) = Path::new(&manifest).parent()
            && let Ok(workspace) = workspace.canonicalize()
            && workspace.starts_with(&worktree)
        {
            root = workspace;
        }
    }
    let interpreter = if matches!(backend, Backend::Pyrefly | Backend::Pyright) {
        dirs.iter()
            .flat_map(|dir| [dir.join(".venv/bin/python"), dir.join("venv/bin/python")])
            .find(|p| p.is_file())
            .or_else(|| {
                ["VIRTUAL_ENV", "CONDA_PREFIX"]
                    .iter()
                    .filter_map(std::env::var_os)
                    .map(PathBuf::from)
                    .map(|p| p.join("bin/python"))
                    .find(|p| p.is_file())
            })
    } else {
        None
    };
    let mut toolchain = String::new();
    for key in [
        "PATH",
        "RUSTUP_TOOLCHAIN",
        "RUSTUP_HOME",
        "CARGO_HOME",
        "VIRTUAL_ENV",
        "CONDA_PREFIX",
        "DEVELOPER_DIR",
        "TOOLCHAINS",
        "GOTOOLCHAIN",
        "GOWORK",
        "GOROOT",
    ] {
        toolchain.push_str(&format!(
            "{key}={}\n",
            std::env::var(key).unwrap_or_default()
        ));
    }
    if backend == Backend::Go {
        toolchain.push_str(&format!(
            "go env GOTOOLCHAIN={:?} GOWORK={gowork:?}\n",
            go_env("GOTOOLCHAIN")
        ));
        if let Some(file) = gowork.as_deref()
            && let Ok(text) = fs::read_to_string(file)
        {
            toolchain.push_str(&format!("{file}:{text}\n"));
        }
    }
    // Toolchain files can sit above a nested package but never above this worktree.
    for dir in &dirs {
        for name in [
            "rust-toolchain",
            "rust-toolchain.toml",
            ".python-version",
            "pyrightconfig.json",
            "pyrefly.toml",
            "pyproject.toml",
            "go.mod",
            "go.work",
            "sweetpad.toml",
            "buildServer.json",
            "node_modules/typescript/package.json",
            ".yarn/sdks/typescript/package.json",
        ] {
            // A go.work chooses the toolchain for all its modules; member go.mod files do not split that session.
            if backend == Backend::Go
                && (explicit_go_work.is_some() || root.join("go.work").is_file())
                && name == "go.mod"
                && *dir != root
            {
                continue;
            }
            if let Ok(text) = fs::read_to_string(dir.join(name)) {
                toolchain.push_str(&format!("{}:{text}\n", dir.join(name).display()));
            }
        }
    }
    toolchain.push_str(&format!(
        "rust_toolchain={rust_toolchain:?}\ninterpreter={interpreter:?}"
    ));
    if backend == Backend::Swift
        && let Ok(selected) = run(&root, "/usr/bin/xcrun", &["--find", "sourcekit-lsp"], 10)
    {
        toolchain.push_str(&selected);
    }
    Ok(Some(Project {
        root,
        worktree,
        language_id,
        backend,
        toolchain,
        rust_toolchain,
        interpreter,
    }))
}

fn language(file: &Path) -> Option<(&'static str, Backend)> {
    Some(match file.extension()?.to_str()? {
        "rs" => ("rust", Backend::Rust),
        "py" | "pyi" => ("python", Backend::Pyrefly),
        "ts" | "mts" | "cts" => ("typescript", Backend::TypeScript),
        "tsx" => ("typescriptreact", Backend::TypeScript),
        "js" | "mjs" | "cjs" => ("javascript", Backend::TypeScript),
        "jsx" => ("javascriptreact", Backend::TypeScript),
        "swift" => ("swift", Backend::Swift),
        "go" => ("go", Backend::Go),
        _ => return None,
    })
}

fn read_toml(file: &Path) -> Result<toml::Value> {
    match fs::read_to_string(file) {
        Ok(text) => {
            toml::from_str(&text).with_context(|| format!("Fix invalid TOML in {}", file.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(toml::Value::Table(Default::default()))
        }
        Err(e) => Err(e).with_context(|| format!("Could not read {}", file.display())),
    }
}

fn launch(project: &Project, program: PathBuf, args: &[&str]) -> Launch {
    Launch {
        program,
        args: args.iter().map(|s| s.to_string()).collect(),
        env: vec![],
        root: project.root.clone(),
        initialization_options: Value::Null,
        settings: json!({}),
        detail: None,
        awaiting_build: false,
        companion: None,
    }
}

/// Runs on a worker. Compiler toolchains and project dependencies are never upgraded here.
pub fn prepare(project: &Project) -> Result<Launch> {
    match project.backend {
        Backend::Rust => rust(project),
        Backend::Pyrefly | Backend::Pyright => python(project),
        Backend::TypeScript => typescript(project),
        Backend::Swift => swift(project),
        Backend::Go => go(project),
    }
}

fn rustup(root: &Path, args: &[&str], seconds: u64) -> Result<String> {
    let mut command = Command::new("rustup");
    command.env("RUSTUP_AUTO_INSTALL", "0");
    crate::git::wait_bounded(command, root, "rustup", args, seconds, true)
        .map_err(anyhow::Error::msg)
}

/// rustup before 1.28 installs a missing active toolchain on any query and has no
/// RUSTUP_AUTO_INSTALL, so opening a file could start a compiler download.
fn check_rustup() -> Result<()> {
    static CURRENT: AtomicBool = AtomicBool::new(false);
    if CURRENT.load(Ordering::Relaxed) {
        return Ok(());
    }
    let mut command = Command::new("rustup");
    command.env_remove("RUSTUP_TOOLCHAIN");
    // Outside any project no override toolchain is selected, so even old rustup installs nothing.
    let Ok(version) =
        crate::git::wait_bounded(command, Path::new("/"), "rustup", &["--version"], 10, true)
    else {
        // Without rustup, the selected rustc must supply its own analyzer.
        return Ok(());
    };
    ensure!(
        rustup_is_current(&version),
        "Update rustup to 1.28 or newer (rustup self update); older versions install toolchains implicitly"
    );
    CURRENT.store(true, Ordering::Relaxed);
    Ok(())
}

fn rustup_is_current(version: &str) -> bool {
    let mut parts = version.split_whitespace().nth(1).unwrap_or("").split('.');
    let mut next = || parts.next().and_then(|p| p.parse::<u32>().ok());
    // An unparseable version is a newer format, not an old rustup.
    matches!((next(), next()), (Some(major), Some(minor)) if (major, minor) >= (1, 28))
        || !version.starts_with("rustup ")
}

fn rust_src_detail(sysroot: &Path) -> Option<String> {
    (!sysroot.join("lib/rustlib/src/rust/library").is_dir())
        .then(|| "Install matching Rust standard-library sources for complete navigation".into())
}

fn rust(project: &Project) -> Result<Launch> {
    let root = &project.root;
    if let Some(toolchain) = project.rust_toolchain.as_deref() {
        // `which`, unlike invoking the rustup proxy, never silently installs a compiler.
        rustup(root, &["which", "--toolchain", toolchain, "rustc"], 10)
            .context("Install the Rust toolchain required by rust-toolchain.toml")?;
        let which = || {
            rustup(
                root,
                &["which", "--toolchain", toolchain, "rust-analyzer"],
                10,
            )
        };
        let sysroot = |analyzer: &str| {
            PathBuf::from(analyzer)
                .ancestors()
                .nth(2)
                .map(Path::to_path_buf)
                .unwrap_or_default()
        };
        let analyzer = match which() {
            Ok(analyzer) if rust_src_detail(&sysroot(&analyzer)).is_none() => analyzer,
            found => {
                // rustup is not safe for concurrent changes to one toolchain (rust-lang/rustup#988).
                static RUSTUP: Mutex<()> = Mutex::new(());
                let _lock = RUSTUP.lock().unwrap_or_else(|e| e.into_inner());
                match rustup(
                    root,
                    &["component", "list", "--installed", "--toolchain", toolchain],
                    15,
                ) {
                    Ok(installed) => {
                        if !installed.lines().any(|s| s.starts_with("rust-analyzer"))
                            || !installed.lines().any(|s| s.starts_with("rust-src"))
                        {
                            let mut command = Command::new("rustup");
                            command.env("RUSTUP_AUTO_INSTALL", "0");
                            // A generous limit: interrupting an install can leave the toolchain
                            // half-written. On expiry rustup gets TERM, not a process-group KILL.
                            let args = [
                                "component",
                                "add",
                                "--toolchain",
                                toolchain,
                                "rust-analyzer",
                                "rust-src",
                            ];
                            crate::git::wait_bounded(command, root, "rustup", &args, 900, false)
                                .map_err(anyhow::Error::msg)
                                .with_context(|| {
                                    format!(
                                        "Add rust-analyzer and rust-src for the existing toolchain: rustup component add --toolchain {toolchain} rust-analyzer rust-src"
                                    )
                                })?;
                        }
                        which()?
                    }
                    // Linked/custom toolchains have no components but can ship their own analyzer.
                    Err(error) => found.map_err(|_| error).with_context(|| {
                        format!(
                            "Toolchain {toolchain} does not support rustup components; add rust-analyzer to it or select a rustup-distributed toolchain"
                        )
                    })?,
                }
            }
        };
        let mut result = launch(project, analyzer.clone().into(), &[]);
        result.detail = rust_src_detail(&sysroot(&analyzer));
        result
            .env
            .push(("RUSTUP_TOOLCHAIN".into(), toolchain.into()));
        result.env.push(("RUSTUP_AUTO_INSTALL".into(), "0".into()));
        return Ok(result);
    }
    let mut command = Command::new("rustc");
    command.env("RUSTUP_AUTO_INSTALL", "0");
    let sysroot =
        crate::git::wait_bounded(command, root, "rustc", &["--print", "sysroot"], 15, true)
            .map_err(anyhow::Error::msg)
            .context("Install the project's required Rust toolchain; Shark will not upgrade it")?;
    let sysroot = Path::new(&sysroot);
    // Homebrew's rust formula ships no analyzer; its rust-analyzer formula installs on PATH.
    let analyzer = Some(sysroot.join("bin/rust-analyzer"))
        .into_iter()
        .chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join("rust-analyzer")),
        )
        .find(|p| p.is_file())
        .with_context(|| {
            format!(
                "Install rust-analyzer (brew install rust-analyzer) or use rustup; the Rust toolchain at {} has none",
                sysroot.display()
            )
        })?;
    let mut result = launch(project, analyzer, &[]);
    result.detail = rust_src_detail(sysroot);
    Ok(result)
}

fn python(project: &Project) -> Result<Launch> {
    let interpreter = match &project.interpreter {
        Some(interpreter) => interpreter.clone(),
        None => run(
            &project.root,
            "python3",
            &["-c", "import sys; print(sys.executable)"],
            10,
        )
        .context("Install Python and this project's dependencies, or create its .venv")?
        .into(),
    };
    let mut result = if project.backend == Backend::Pyright {
        let node = managed("node")?;
        let server = managed("pyright")?;
        launch(project, node, &[path(&server)?, "--stdio"])
    } else {
        launch(project, managed("pyrefly")?, &["lsp"])
    };
    result.settings = json!({"python": {"pythonPath": interpreter}});
    result.initialization_options = json!({"pythonPath": interpreter});
    // Ruff only formats and fixes; without it the checker still supplies intelligence.
    match managed("ruff") {
        Ok(ruff) => result.companion = Some(Box::new(launch(project, ruff, &["server"]))),
        Err(error) => result.detail = Some(companion_note(&error)),
    }
    Ok(result)
}

/// Shown when the Ruff companion cannot start; the primary server keeps running.
pub fn companion_note(error: &anyhow::Error) -> String {
    format!("Ruff unavailable: {error:#}; formatting and lint fixes are disabled")
}

fn typescript(project: &Project) -> Result<Launch> {
    let dirs: Vec<_> = project
        .root
        .ancestors()
        .take_while(|d| d.starts_with(&project.worktree))
        .collect();
    // Yarn Plug'n'Play has no node_modules; its editor SDK wraps the zipped package.
    if let Some(package) = dirs
        .iter()
        .flat_map(|d| {
            [
                d.join("node_modules/typescript/package.json"),
                d.join(".yarn/sdks/typescript/package.json"),
            ]
        })
        .find(|p| p.is_file())
    {
        let value: Value = serde_json::from_slice(&fs::read(&package)?)?;
        let version = value["version"]
            .as_str()
            .context("Project TypeScript SDK has no version")?;
        let major: u32 = version
            .split('.')
            .next()
            .context("Invalid TypeScript SDK version")?
            .parse()?;
        let sdk = package
            .parent()
            .context("Invalid TypeScript SDK location")?;
        let node = managed("node")?;
        if major >= 7 {
            let tsc = sdk.join("bin/tsc");
            ensure!(
                tsc.is_file(),
                "Reinstall the project's TypeScript SDK: {} is missing",
                tsc.display()
            );
            let mut result = launch(project, node.clone(), &[path(&tsc)?, "--lsp", "--stdio"]);
            node_path(&mut result, &node);
            return Ok(result);
        }
        let server = sdk.join("lib/tsserver.js");
        ensure!(
            server.is_file(),
            "Reinstall the project's classic TypeScript SDK; lib/tsserver.js is missing"
        );
        let wrapper = managed("typescript-language-server")?;
        let mut result = launch(project, node.clone(), &[path(&wrapper)?, "--stdio"]);
        result.initialization_options = json!({"tsserver": {"path": server}, "npmLocation": node.parent().unwrap().join("npm")});
        // tsserver's child runtime comes from Shark's managed Node, including Finder launches.
        node_path(&mut result, &node);
        return Ok(result);
    }
    for dir in &dirs {
        let package = dir.join("package.json");
        if !package.is_file() {
            continue;
        }
        let value: Value = serde_json::from_slice(&fs::read(&package)?)?;
        if ["dependencies", "devDependencies", "peerDependencies"]
            .iter()
            .any(|k| value[*k].get("typescript").is_some())
        {
            if dirs.iter().any(|d| d.join(".pnp.cjs").is_file()) {
                bail!(
                    "Run `yarn dlx @yarnpkg/sdks base` to expose this Yarn Plug'n'Play project's TypeScript SDK to editors, then Retry"
                );
            }
            bail!(
                "Install this project's dependencies to restore its declared TypeScript SDK, then Retry"
            );
        }
    }
    let mut result = launch(project, managed("typescript")?, &["--lsp", "--stdio"]);
    node_path(&mut result, &managed("node")?);
    Ok(result)
}

fn node_path(launch: &mut Launch, node: &Path) {
    launch.env.push((
        "PATH".into(),
        format!(
            "{}:{}",
            node.parent().unwrap().display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    ));
}

fn xcode_containers(root: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<_> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && matches!(
                    p.extension().and_then(|s| s.to_str()),
                    Some("xcodeproj" | "xcworkspace")
                )
        })
        .collect();
    entries.sort();
    // A workspace normally contains the adjacent project; choose the workspace when unique.
    if entries
        .iter()
        .filter(|p| p.extension().is_some_and(|s| s == "xcworkspace"))
        .count()
        == 1
    {
        entries.retain(|p| p.extension().is_some_and(|s| s == "xcworkspace"));
    }
    entries
}

fn swift(project: &Project) -> Result<Launch> {
    let program = run(&project.root, "/usr/bin/xcrun", &["--find", "sourcekit-lsp"], 15)
        .context("Install/select Xcode or a Swift toolchain with SourceKit-LSP (xcode-select --switch /Applications/Xcode.app)")?;
    let mut result = launch(project, program.into(), &[]);
    let bsp = project.root.join("buildServer.json");
    if project.root.join("Package.swift").is_file() {
        if bsp.exists() {
            result.detail = Some(
                "Existing buildServer.json overrides SwiftPM; verify that this is intentional"
                    .into(),
            );
        }
        return Ok(result);
    }
    let previous = if bsp.exists() {
        Some(fs::read(&bsp)?)
    } else {
        None
    };
    let current: Option<Value> = previous
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()
        .context("Fix invalid buildServer.json; Shark preserves your custom BSP configuration")?;
    // Any Shark-written file is ours; one whose `_shark` differs from today's is regenerated.
    let owned = current
        .as_ref()
        .is_some_and(|v| v["_shark"]["owner"] == "shark");
    if let Some(config) = current.as_ref().filter(|_| !owned) {
        ensure!(
            ["name", "version", "bspVersion"]
                .iter()
                .all(|key| config[*key].is_string())
                && config["languages"].is_array()
                && config["argv"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty() && a.iter().all(Value::is_string)),
            "Complete name, version, bspVersion, languages and argv in buildServer.json; Shark preserves custom BSP configuration"
        );
        result.detail = Some(
            "Using existing buildServer.json; build the project to prepare its navigation index"
                .into(),
        );
        result.awaiting_build = true;
        return Ok(result);
    }
    let mut containers = xcode_containers(&project.root);
    if containers.is_empty() {
        result.detail = Some(
            "Open a Swift package or configure buildServer.json for project navigation".into(),
        );
        return Ok(result);
    }
    if containers.len() > 1 {
        let selected = fs::read(xcode_selection_file(project))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PathBuf>(&bytes).ok())
            .map(|p| project.root.join(p))
            .or_else(|| {
                current
                    .as_ref()
                    .and_then(|v| v["_shark"]["container"].as_str())
                    .map(|p| project.root.join(p))
            });
        match selected.filter(|p| containers.contains(p)) {
            Some(selected) => containers = vec![selected],
            None => {
                return Err(XcodeChoice {
                    options: containers,
                }
                .into());
            }
        }
    }
    let sweetpad = managed("sweetpad")?;
    let expected = json!({"owner":"shark", "version":2, "root": project.root, "tool": sweetpad, "container": containers[0].file_name()});
    if current.as_ref().and_then(|v| v.get("_shark")) != Some(&expected) {
        let staged = project
            .root
            .join(format!(".shark-buildServer-{}.json", std::process::id()));
        ensure!(
            !staged.exists(),
            "Remove stale {} and Retry",
            staged.display()
        );
        let generated = (|| -> Result<()> {
            run(
                &project.root,
                path(&sweetpad)?,
                &[
                    "--non-interactive",
                    "bsp",
                    "init",
                    container_flag(&containers[0]),
                    path(&containers[0])?,
                    "--output-file",
                    path(&staged)?,
                ],
                30,
            )?;
            let mut config: Value = serde_json::from_slice(&fs::read(&staged)?)?;
            // ponytail: read once per generated config; a renamed Debug configuration takes a
            // deleted buildServer.json or a new config version.
            if let Some(configuration) =
                debug_configuration(&configurations(&sweetpad, &containers[0])?)
            {
                let argv = config["argv"]
                    .as_array_mut()
                    .context("SweetPad wrote no BSP argv")?;
                argv.extend(["--configuration".into(), configuration.into()]);
            }
            config["_shark"] = expected;
            let text = serde_json::to_string_pretty(&config)?;
            if let Some(previous) = &previous {
                let fingerprint = crate::files::fingerprint(previous);
                ensure!(
                    crate::editor::write_checked(&bsp, fingerprint, &text, false)?.is_some(),
                    "buildServer.json changed during setup; Retry"
                );
            } else {
                fs::write(&staged, text)?;
                // Linking activates the complete file atomically and cannot replace a concurrent writer.
                fs::hard_link(&staged, &bsp)?;
            }
            // Best effort, for new and regenerated Shark-owned files alike: outside a git
            // repository there is nothing to keep clean.
            let _ = exclude_from_git(&project.root, "buildServer.json");
            Ok(())
        })();
        let _ = fs::remove_file(staged);
        generated?;
    }
    // Background indexing waits on SweetPad's prepare, which builds every target first and
    // holds diagnostics back. SweetPad's builds still write the index sourcekit-lsp reads.
    result.initialization_options = json!({"backgroundIndexing": false});
    Ok(result)
}

fn container_flag(container: &Path) -> &'static str {
    if container.extension().is_some_and(|s| s == "xcworkspace") {
        "--workspace"
    } else {
        "--project"
    }
}

/// SweetPad builds and resolves `Debug` unless told otherwise; in a project without it,
/// xcodebuild substitutes the default (often Release) configuration, whose builds can fail and
/// whose arguments are optimized.
fn configurations(sweetpad: &Path, container: &Path) -> Result<Vec<String>> {
    let args = ["--json", "--non-interactive", "project", "info"];
    let target = [container_flag(container), path(container)?];
    let out = run(
        container.parent().unwrap_or(container),
        path(sweetpad)?,
        &[&args[..], &target].concat(),
        30,
    )
    .context("Could not read the Xcode project's build configurations; Retry")?;
    let mut info: Value = serde_json::from_str(&out)?;
    Ok(serde_json::from_value(
        info["data"]["configurations"].take(),
    )?)
}

fn debug_configuration(configurations: &[String]) -> Option<&str> {
    (configurations.iter().find(|c| *c == "Debug"))
        .or_else(|| (configurations.iter()).find(|c| c.to_ascii_lowercase().contains("debug")))
        .map(String::as_str)
}

/// Keeps a generated file with machine-specific paths out of `git status`. info/exclude is
/// local to the clone, shared by its worktrees, and never committed.
fn exclude_from_git(root: &Path, name: &str) -> Result<()> {
    let output = run(
        root,
        "git",
        &["rev-parse", "--git-path", "info/exclude", "--show-prefix"],
        10,
    )?;
    let mut lines = output.lines();
    let exclude = root.join(lines.next().context("git printed no exclude path")?);
    let pattern = format!("/{}{name}", lines.next().unwrap_or_default());
    let text = fs::read_to_string(&exclude).unwrap_or_default();
    if !text.lines().any(|line| line == pattern) {
        fs::create_dir_all(exclude.parent().context("Invalid exclude path")?)?;
        let separator = if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&exclude)?;
        std::io::Write::write_all(&mut file, format!("{separator}{pattern}\n").as_bytes())?;
    }
    Ok(())
}

/// The single gopls pin: the Makefile and scripts read it from this line.
const GOPLS_VERSION: &str = "0.24.0";

/// A Go setting as the go command resolves it: process environment, then the `go env -w`
/// file. $GOROOT/go.env only supplies defaults that match leaving the value unset.
fn go_env(key: &str) -> Option<String> {
    if let Some(value) = std::env::var(key).ok().filter(|v| !v.is_empty()) {
        return Some(value);
    }
    let file = match std::env::var_os("GOENV") {
        Some(file) if file == "off" => return None,
        Some(file) if !file.is_empty() => PathBuf::from(file),
        // os.UserConfigDir on macOS.
        _ => PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/go/env"),
    };
    fs::read_to_string(file)
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .filter(|v| !v.is_empty())
        .map(String::from)
}

/// The user's GOTOOLCHAIN with downloads disabled: Go's `path` mode honors go.mod/go.work
/// toolchain selection without downloading compilers.
fn go_toolchain() -> String {
    let selected = go_env("GOTOOLCHAIN").unwrap_or_else(|| "auto".into());
    match selected.as_str() {
        "auto" => "path".into(),
        "local" | "path" => selected,
        value if value.ends_with("+auto") => value.replace("+auto", "+path"),
        value if value.ends_with("+path") => selected,
        _ => format!("{selected}+path"),
    }
}

fn go(project: &Project) -> Result<Launch> {
    let selected = go_toolchain();
    let mut command = Command::new("go");
    command.env("GOTOOLCHAIN", &selected);
    crate::git::wait_bounded(command, &project.root, "go", &["version"], 15, true)
        .map_err(anyhow::Error::msg)
        .context("Install the Go toolchain required by go.mod/go.work, then Retry")?;
    let executable = std::env::current_exe()?;
    let mut resources = executable
        .parent()
        .and_then(Path::parent)
        .context("Cannot locate Shark resources")?
        .join("Resources/language-tools/gopls")
        .join(GOPLS_VERSION);
    if cfg!(debug_assertions) && !resources.join("gopls").is_file() {
        resources = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/language-tools/gopls")
            .join(GOPLS_VERSION);
    }
    let binary = resources.join("gopls");
    ensure!(
        binary.is_file(),
        "This Shark build has no bundled gopls {GOPLS_VERSION}; install a release containing language tools (developers: run scripts/build-gopls.sh before make release)"
    );
    let expected = fs::read_to_string(resources.join("gopls.sha256"))?;
    install::verify(&binary, expected.trim())?;
    let mut result = launch(project, binary, &[]);
    result.env.push(("GOTOOLCHAIN".into(), selected));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("shark-lsp-project-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (file, text) in files {
            let p = root.join(file);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        root
    }
    /// Runs `f` with environment overrides, restoring them afterwards. Tests that call it also
    /// hold UI_LOCK (when built) so the other modules that set PATH or SHARK_* stay out.
    fn with_env<T>(vars: &[(&str, Option<String>)], f: impl FnOnce() -> T) -> T {
        static ENV: Mutex<()> = Mutex::new(());
        let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<_> = vars
            .iter()
            .map(|(k, _)| (*k, std::env::var_os(k)))
            .collect();
        let set = |k: &str, v: Option<std::ffi::OsString>| match v {
            // SAFETY: ENV serializes this module's mutations; UI_LOCK, held by callers under
            // test-support, serializes them against other modules' tests.
            Some(v) => unsafe { std::env::set_var(k, v) },
            None => unsafe { std::env::remove_var(k) },
        };
        for (k, v) in vars {
            set(k, v.clone().map(Into::into));
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        for (k, v) in saved {
            set(k, v);
        }
        result.unwrap_or_else(|e| std::panic::resume_unwind(e))
    }

    fn script(dir: &Path, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let file = dir.join(name);
        fs::create_dir_all(dir).unwrap();
        fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn rust_analyzer_is_found_for_linked_and_non_rustup_toolchains() {
        #[cfg(feature = "test-support")]
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = fixture("rust-tools", &[("Cargo.toml", "[package]\nname='a'")]);
        let sysroot = root.join("stage1");
        script(&sysroot.join("bin"), "rust-analyzer", "exit 0");
        let bin = root.join("bin");
        // A linked toolchain: rustup finds its tools but rejects component operations.
        script(
            &bin,
            "rustup",
            &format!(
                "case \"$1 $4\" in\n'which rustc'|'which rust-analyzer') echo {}/bin/$4 ;;\n*) echo \"error: invalid toolchain name: 'stage1'\" >&2; exit 1 ;;\nesac",
                sysroot.display()
            ),
        );
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        let mut project = Project {
            root: root.clone(),
            worktree: root.clone(),
            language_id: "rust",
            backend: Backend::Rust,
            toolchain: String::new(),
            rust_toolchain: Some("stage1".into()),
            interpreter: None,
        };
        let linked = with_env(&[("PATH", Some(path.clone()))], || prepare(&project)).unwrap();
        assert_eq!(linked.program, sysroot.join("bin/rust-analyzer"));
        // An informational note is not indexing work.
        assert!(linked.detail.is_some() && !linked.awaiting_build);
        // Homebrew: rustc's sysroot has no analyzer; the separate formula is on PATH.
        let brew = root.join("brew");
        let cellar = root.join("cellar");
        fs::create_dir_all(&cellar).unwrap();
        script(
            &brew.join("bin"),
            "rustc",
            &format!("echo {}", cellar.display()),
        );
        script(&brew.join("bin"), "rust-analyzer", "exit 0");
        project.rust_toolchain = None;
        let path = format!(
            "{}:{}",
            brew.join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        let homebrew = with_env(&[("PATH", Some(path))], || prepare(&project)).unwrap();
        assert_eq!(homebrew.program, brew.join("bin/rust-analyzer"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rustup_older_than_1_28_is_rejected() {
        assert!(!rustup_is_current("rustup 1.27.1 (54dd3d00f 2024-04-24)"));
        assert!(rustup_is_current("rustup 1.28.0 (6ebc3db2b 2025-03-02)"));
        assert!(rustup_is_current("rustup 1.29.2 (28d1352db 2025-03-05)"));
        assert!(rustup_is_current("rustup 2.0.0"));
    }

    #[test]
    fn go_settings_from_go_env_file_and_explicit_gowork_are_honored() {
        #[cfg(feature = "test-support")]
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = fixture(
            "goenv",
            &[
                ("env", "GOPROXY=off\nGOTOOLCHAIN=local\n"),
                ("go.work", "go 1.26\nuse ./b"),
                ("b/go.mod", "module b"),
                ("b/main.go", ""),
                ("tools/dev.work", "go 1.26\nuse ../b"),
            ],
        );
        let env = |gowork: &Path| {
            [
                ("GOENV", Some(root.join("env").display().to_string())),
                ("GOTOOLCHAIN", None),
                ("GOWORK", Some(gowork.display().to_string())),
            ]
        };
        let (toolchain, discovered) = with_env(&env(&root.join("tools/dev.work")), || {
            (go_toolchain(), discover(&root.join("b/main.go"), &root))
        });
        assert_eq!(toolchain, "local");
        assert_eq!(
            discovered.unwrap().unwrap().root,
            root.join("tools").canonicalize().unwrap()
        );
        let off = with_env(&env(Path::new("off")), || {
            discover(&root.join("b/main.go"), &root)
        });
        assert_eq!(
            off.unwrap().unwrap().root,
            root.join("b").canonicalize().unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn generated_build_server_config_is_excluded_from_every_worktree() {
        let root = fixture("exclude", &[("app/README", "")]);
        let git = |dir: &Path, args: &[&str]| {
            run(dir, "git", args, 10).unwrap();
        };
        git(&root, &["init", "-q"]);
        git(
            &root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "i",
            ],
        );
        git(&root, &["worktree", "add", "-q", "--detach", "linked"]);
        let linked = root.join("linked/app");
        fs::create_dir_all(&linked).unwrap();
        exclude_from_git(&linked, "buildServer.json").unwrap();
        exclude_from_git(&linked, "buildServer.json").unwrap();
        let exclude = fs::read_to_string(root.join(".git/info/exclude")).unwrap();
        assert_eq!(
            exclude.matches("/app/buildServer.json").count(),
            1,
            "{exclude}"
        );
        fs::write(linked.join("buildServer.json"), "{}").unwrap();
        assert_eq!(
            run(
                &linked,
                "git",
                &["status", "--porcelain", "--untracked-files=all"],
                10
            )
            .unwrap(),
            ""
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn yarn_pnp_without_editor_sdk_explains_how_to_expose_typescript() {
        let root = fixture(
            "pnp",
            &[
                (
                    "package.json",
                    r#"{"devDependencies":{"typescript":"^5.9.0"}}"#,
                ),
                (".pnp.cjs", ""),
                ("src/a.ts", ""),
            ],
        );
        let project = discover(&root.join("src/a.ts"), &root).unwrap().unwrap();
        let error = prepare(&project).unwrap_err().to_string();
        assert!(error.contains("yarn dlx @yarnpkg/sdks base"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nested_projects_and_go_work_are_bounded_by_worktree() {
        #[cfg(feature = "test-support")]
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = fixture(
            "roots",
            &[
                ("Cargo.toml", "[workspace]\nmembers=['nested']"),
                ("nested/Cargo.toml", "[package]\nname='nested'"),
                ("nested/src/lib.rs", ""),
                ("go.work", "go 1.26\nuse ./a"),
                ("a/go.mod", "module a"),
                ("a/main.go", ""),
                ("b/go.mod", "module b"),
                ("b/main.go", ""),
            ],
        );
        assert_eq!(
            discover(&root.join("nested/src/lib.rs"), &root)
                .unwrap()
                .unwrap()
                .root,
            root.canonicalize().unwrap()
        );
        assert_eq!(
            discover(&root.join("a/main.go"), &root)
                .unwrap()
                .unwrap()
                .root,
            root.canonicalize().unwrap()
        );
        assert_eq!(
            discover(&root.join("a/main.go"), &root)
                .unwrap()
                .unwrap()
                .session_key(),
            discover(&root.join("b/main.go"), &root)
                .unwrap()
                .unwrap()
                .session_key(),
        );
        assert_eq!(
            discover(&root.join("a/main.go"), &root.join("a"))
                .unwrap()
                .unwrap()
                .root,
            root.join("a").canonicalize().unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn explicit_python_checker_and_nested_swift_package_are_preserved() {
        let root = fixture(
            "semantics",
            &[
                ("pyproject.toml", "[tool.\"pyright\"]\nstrict=['.']"),
                ("pkg/pyproject.toml", "[project]\nname='nested'"),
                ("pkg/code.py", ""),
                ("pkg/.venv/bin/python", "test interpreter"),
                ("App.xcodeproj/project.pbxproj", ""),
                ("Package/Package.swift", ""),
                ("Package/Sources/main.swift", ""),
            ],
        );
        assert_eq!(
            discover(&root.join("pkg/code.py"), &root)
                .unwrap()
                .unwrap()
                .backend,
            Backend::Pyright
        );
        let python = discover(&root.join("pkg/code.py"), &root).unwrap().unwrap();
        assert_eq!(python.root, root.canonicalize().unwrap());
        assert_eq!(
            python.interpreter,
            Some(root.canonicalize().unwrap().join("pkg/.venv/bin/python"))
        );
        assert_eq!(
            discover(&root.join("Package/Sources/main.swift"), &root)
                .unwrap()
                .unwrap()
                .root,
            root.join("Package").canonicalize().unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn symlink_outside_worktree_is_rejected() {
        let root = fixture("symlink", &[("repo/a.rs", ""), ("external.rs", "")]);
        std::os::unix::fs::symlink(root.join("external.rs"), root.join("repo/escape.rs")).unwrap();
        assert!(discover(&root.join("repo/escape.rs"), &root.join("repo")).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sessions_share_language_family_but_not_worktrees_or_moved_roots() {
        #[cfg(feature = "test-support")]
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = fixture(
            "sessions",
            &[
                ("one/package.json", "{}"),
                ("one/a.ts", ""),
                ("one/b.jsx", ""),
                ("two/package.json", "{}"),
                ("two/a.ts", ""),
            ],
        );
        let a = discover(&root.join("one/a.ts"), &root.join("one"))
            .unwrap()
            .unwrap();
        let b = discover(&root.join("one/b.jsx"), &root.join("one"))
            .unwrap()
            .unwrap();
        let other = discover(&root.join("two/a.ts"), &root.join("two"))
            .unwrap()
            .unwrap();
        assert_eq!(a.session_key(), b.session_key());
        assert_ne!(a.language_id, b.language_id);
        assert_ne!(a.session_key(), other.session_key());
        fs::rename(root.join("one"), root.join("moved")).unwrap();
        let moved = discover(&root.join("moved/a.ts"), &root.join("moved"))
            .unwrap()
            .unwrap();
        assert_ne!(a.session_key(), moved.session_key());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_declared_typescript_sdk_never_selects_managed_native() {
        let root = fixture(
            "sdk",
            &[
                (
                    "package.json",
                    r#"{"devDependencies":{"typescript":"~5.9.0"}}"#,
                ),
                ("src/a.ts", ""),
            ],
        );
        let project = discover(&root.join("src/a.ts"), &root).unwrap().unwrap();
        let error = prepare(&project).unwrap_err().to_string();
        assert!(error.contains("declared TypeScript SDK"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sweetpad_indexes_the_projects_debug_configuration() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            debug_configuration(&names(&["Release", "Debug", "Beta-Debug"])),
            Some("Debug")
        );
        assert_eq!(
            debug_configuration(&names(&["Dev-Release", "Dev-Debug", "Prod-Debug"])),
            Some("Dev-Debug")
        );
        assert_eq!(debug_configuration(&names(&["Release"])), None);
    }

    #[test]
    fn xcode_workspace_selection_does_not_claim_swift_packages() {
        let root = fixture(
            "xcode",
            &[
                ("A.xcodeproj/project.pbxproj", ""),
                ("B.xcodeproj/project.pbxproj", ""),
                ("App.xcworkspace/contents.xcworkspacedata", ""),
                ("Packages/Feature/Package.swift", ""),
                ("Packages/Feature/source.swift", ""),
            ],
        );
        assert_eq!(xcode_containers(&root), vec![root.join("App.xcworkspace")]);
        let package = discover(&root.join("Packages/Feature/source.swift"), &root)
            .unwrap()
            .unwrap();
        assert_eq!(
            package.root,
            root.join("Packages/Feature").canonicalize().unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rediscovery_uses_new_virtual_environments_and_checker_configuration() {
        #[cfg(feature = "test-support")]
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = fixture("rediscover", &[("src/a.py", "answer = 1\n")]);
        let first = discover(&root.join("src/a.py"), &root).unwrap().unwrap();
        let interpreter = root.join(".venv/bin/python");
        fs::create_dir_all(interpreter.parent().unwrap()).unwrap();
        fs::write(&interpreter, "test interpreter").unwrap();
        let second = discover(&root.join("src/a.py"), &root).unwrap().unwrap();
        assert_eq!(
            second.interpreter,
            Some(root.canonicalize().unwrap().join(".venv/bin/python"))
        );
        assert_ne!(first.session_key(), second.session_key());
        fs::write(root.join("pyrightconfig.json"), "{}").unwrap();
        let third = discover(&root.join("src/a.py"), &root).unwrap().unwrap();
        assert_eq!(third.backend, Backend::Pyright);
        assert_ne!(second.session_key(), third.session_key());
        fs::remove_dir_all(root).unwrap();
    }
}

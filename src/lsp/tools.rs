//! Project semantics are selected once, before installing anything; crashes never switch backends.
#[path = "install.rs"]
mod install;
use anyhow::{Context, Result, bail, ensure};
use install::{managed, path, run};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Backend {
    Rust,
    Pyrefly,
    Pyright,
    TypeScript,
    Swift,
    Go,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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
    let mut root = match backend {
        Backend::Rust => dirs
            .iter()
            .find(|d| d.join("Cargo.toml").is_file())
            .copied(),
        Backend::Pyrefly => dirs
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
        Backend::Go => dirs
            .iter()
            .find(|d| {
                std::env::var("GOWORK").ok().as_deref() != Some("off")
                    && d.join("go.work").is_file()
            })
            .or_else(|| dirs.iter().find(|d| d.join("go.mod").is_file()))
            .copied(),
        Backend::Pyright => unreachable!(),
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
                    .filter_map(|key| std::env::var_os(key))
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
        ] {
            // A go.work chooses the toolchain for all its modules; member go.mod files do not split that session.
            if backend == Backend::Go
                && root.join("go.work").is_file()
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
    match backend {
        Backend::Swift => {
            if let Ok(selected) = run(&root, "/usr/bin/xcrun", &["--find", "sourcekit-lsp"], 10) {
                toolchain.push_str(&selected);
            }
        }
        _ => {}
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

fn rust(project: &Project) -> Result<Launch> {
    let root = &project.root;
    if let Some(toolchain) = project.rust_toolchain.as_deref() {
        // `which`, unlike invoking the rustup proxy, never silently installs a compiler.
        rustup(root, &["which", "--toolchain", toolchain, "rustc"], 10)
            .context("Install the Rust toolchain required by rust-toolchain.toml")?;
        let installed = rustup(
            root,
            &["component", "list", "--installed", "--toolchain", toolchain],
            15,
        )?;
        if !installed.lines().any(|s| s.starts_with("rust-analyzer"))
            || !installed.lines().any(|s| s.starts_with("rust-src"))
        {
            rustup(root, &["component", "add", "--toolchain", toolchain, "rust-analyzer", "rust-src"], 180)
                .with_context(|| format!("Add rust-analyzer and rust-src for the existing toolchain: rustup component add --toolchain {toolchain} rust-analyzer rust-src"))?;
        }
        let analyzer = rustup(
            root,
            &["which", "--toolchain", toolchain, "rust-analyzer"],
            10,
        )?;
        let mut result = launch(project, analyzer.into(), &[]);
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
    let analyzer = Path::new(&sysroot).join("bin/rust-analyzer");
    ensure!(
        analyzer.is_file(),
        "Install rust-analyzer for the selected Rust toolchain at {}",
        sysroot
    );
    let mut result = launch(project, analyzer, &[]);
    if !Path::new(&sysroot)
        .join("lib/rustlib/src/rust/library")
        .is_dir()
    {
        result.detail =
            Some("Install matching Rust standard-library sources for complete navigation".into());
    }
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
    result.companion = Some(Box::new(launch(project, managed("ruff")?, &["server"])));
    Ok(result)
}

fn typescript(project: &Project) -> Result<Launch> {
    let dirs: Vec<_> = project
        .root
        .ancestors()
        .take_while(|d| d.starts_with(&project.worktree))
        .collect();
    if let Some(package) = dirs
        .iter()
        .map(|d| d.join("node_modules/typescript/package.json"))
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
    for dir in dirs {
        let package = dir.join("package.json");
        if !package.is_file() {
            continue;
        }
        let value: Value = serde_json::from_slice(&fs::read(&package)?)?;
        if ["dependencies", "devDependencies", "peerDependencies"]
            .iter()
            .any(|k| value[*k].get("typescript").is_some())
        {
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
    let owned = previous
        .as_ref()
        .and_then(|b| serde_json::from_slice::<Value>(b).ok())
        .is_some_and(|v| v["_shark"]["owner"] == "shark" && v["_shark"]["version"] == 1);
    if previous.is_some() && !owned {
        let config: Value = serde_json::from_slice(previous.as_ref().unwrap()).context(
            "Fix invalid buildServer.json; Shark preserves your custom BSP configuration",
        )?;
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
                previous
                    .as_ref()
                    .and_then(|b| serde_json::from_slice::<Value>(b).ok())
                    .and_then(|v| {
                        v["_shark"]["container"]
                            .as_str()
                            .map(|p| project.root.join(p))
                    })
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
    let expected = json!({"owner":"shark", "version":1, "root": project.root, "tool": sweetpad, "container": containers[0].file_name()});
    let current: Option<Value> = previous
        .as_ref()
        .and_then(|b| serde_json::from_slice(b).ok());
    if current.as_ref().and_then(|v| v.get("_shark")) != Some(&expected) {
        let staged = project
            .root
            .join(format!(".shark-buildServer-{}.json", std::process::id()));
        ensure!(
            !staged.exists(),
            "Remove stale {} and Retry",
            staged.display()
        );
        let flag = if containers[0]
            .extension()
            .is_some_and(|s| s == "xcworkspace")
        {
            "--workspace"
        } else {
            "--project"
        };
        let generated = (|| -> Result<()> {
            run(
                &project.root,
                path(&sweetpad)?,
                &[
                    "--non-interactive",
                    "bsp",
                    "init",
                    flag,
                    path(&containers[0])?,
                    "--output-file",
                    path(&staged)?,
                ],
                30,
            )?;
            let mut config: Value = serde_json::from_slice(&fs::read(&staged)?)?;
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
            Ok(())
        })();
        let _ = fs::remove_file(staged);
        generated?;
    }
    result.detail =
        Some("Preparing index: build this Xcode project for project-wide navigation".into());
    Ok(result)
}

fn go(project: &Project) -> Result<Launch> {
    // Go's `path` mode honors go.mod/go.work toolchain selection without downloading compilers.
    let selected = std::env::var("GOTOOLCHAIN").unwrap_or_else(|_| "auto".into());
    let selected = match selected.as_str() {
        "auto" => "path".into(),
        "local" | "path" => selected,
        value if value.ends_with("+auto") => value.replace("+auto", "+path"),
        value if value.ends_with("+path") => selected,
        _ => format!("{selected}+path"),
    };
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
        .join("Resources/language-tools/gopls/0.24.0");
    if cfg!(debug_assertions) && !resources.join("gopls").is_file() {
        resources =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/language-tools/gopls/0.24.0");
    }
    let binary = resources.join("gopls");
    ensure!(
        binary.is_file(),
        "This Shark build has no bundled gopls 0.24.0; install a release containing language tools (developers: run scripts/build-gopls.sh before make release)"
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

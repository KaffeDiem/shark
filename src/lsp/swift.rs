//! Swift: SourceKit-LSP, with a SweetPad-generated BSP config for Xcode projects.
use super::install::{managed, path, run};
use super::tools::{Launch, Project, launch};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

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

pub(super) fn xcode_containers(root: &Path) -> Vec<PathBuf> {
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

pub(super) fn swift(project: &Project) -> Result<Launch> {
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
                    crate::files::write_checked(&bsp, fingerprint, &text, false)?.is_some(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::tools::{discover, tests::fixture};

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
}

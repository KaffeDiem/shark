//! Per-provider roots and one-time login-shell discovery for local token logs.
//! Custom roots only add to defaults: a broad ancestor cannot hide a curated dot-directory.
use crate::usage::Provider;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};

pub const LOCATION_VARIABLES: &[&str] = &["CLAUDE_CONFIG_DIR", "CODEX_HOME"];
pub const CLAUDE_PROJECTS: &str = ".claude/projects";
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roots(pub Vec<PathBuf>);

impl Roots {
    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self(normalized(paths))
    }
    pub fn for_provider_with_custom(provider: Provider, custom: Option<&str>) -> Self {
        let home = crate::state::home();
        let environment = environment();
        let defaults = match provider {
            Provider::Claude => claude_roots(
                &home,
                environment.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            ),
            Provider::Codex => {
                codex_roots(&home, environment.get("CODEX_HOME").map(String::as_str))
            }
        };
        with_custom(defaults.0, custom, &home)
    }
}

/// Unlike raw string prefixes, components distinguish `/projects` from `/projects-other`.
fn normalize(path: &Path) -> PathBuf {
    if let Ok(path) = fs::canonicalize(path) {
        return path;
    }
    // Canonicalize an existing parent too, so a not-yet-created child under a symlink folds.
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        if parent != path
            && let Ok(real) = fs::canonicalize(parent)
        {
            return real.join(name);
        }
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            _ => out.push(component.as_os_str()),
        }
    }
    out
}
pub fn normalized(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut result: Vec<PathBuf> = vec![];
    for path in paths
        .into_iter()
        .filter(|p| p.is_absolute())
        .map(|p| normalize(&p))
    {
        if result.iter().any(|r| path.starts_with(r)) {
            continue;
        }
        result.retain(|r| !r.starts_with(&path));
        result.push(path);
    }
    result
}
pub fn parse_paths(raw: &str, home: &Path) -> Vec<PathBuf> {
    raw.split([',', '\n', '\r'])
        .filter_map(|part| {
            let part = part.trim();
            let path = if part == "~" {
                home.to_path_buf()
            } else if let Some(tail) = part.strip_prefix("~/") {
                home.join(tail)
            } else {
                PathBuf::from(part)
            };
            // A bare `/` or home directory would scan the whole disk or account.
            (!part.is_empty() && path.is_absolute() && path != Path::new("/") && path != home)
                .then_some(path)
        })
        .collect()
}
pub fn expand(raw: &str, home: &Path) -> Vec<PathBuf> {
    let mut output = vec![];
    for pattern in parse_paths(raw, home) {
        let mut paths = vec![PathBuf::from("/")];
        for component in pattern.components() {
            let Component::Normal(segment) = component else {
                continue;
            };
            let segment = segment.to_string_lossy();
            if segment.contains(['*', '?', '[']) {
                let Ok(glob) = globset::GlobBuilder::new(&segment)
                    .literal_separator(true)
                    .build()
                else {
                    paths.clear();
                    break;
                };
                let matcher = glob.compile_matcher();
                let mut next = vec![];
                for base in paths {
                    if let Ok(entries) = fs::read_dir(base) {
                        let mut matches: Vec<_> = entries
                            .flatten()
                            .filter(|e| matcher.is_match(e.file_name()))
                            .map(|e| e.path())
                            .collect();
                        matches.sort();
                        next.extend(matches);
                    }
                }
                paths = next;
            } else {
                paths = paths
                    .into_iter()
                    .map(|p| p.join(segment.as_ref()))
                    .collect();
            }
        }
        output.extend(paths.into_iter().filter(|p| p.is_dir()));
    }
    output
}
pub fn with_custom(defaults: Vec<PathBuf>, custom: Option<&str>, home: &Path) -> Roots {
    let defaults = normalized(defaults);
    let extra = custom
        .map(|raw| expand(raw, home))
        .unwrap_or_default()
        .into_iter()
        .map(|p| normalize(&p))
        .filter(|p| !defaults.iter().any(|d| d != p && d.starts_with(p)))
        .collect::<Vec<_>>();
    Roots::new(defaults.into_iter().chain(extra).collect())
}
pub fn claude_roots(home: &Path, config: Option<&str>) -> Roots {
    let mut paths = vec![
        home.join(CLAUDE_PROJECTS),
        home.join(".config/claude/projects"),
    ];
    if let Some(config) = config {
        paths.extend(
            parse_paths(config, home)
                .into_iter()
                .map(|p| p.join("projects")),
        );
    }
    // Claude Desktop sessions keep their own hidden CLI stores under sandbox workspaces.
    paths.extend(embedded_claude(
        &home.join("Library/Application Support/Claude/local-agent-mode-sessions"),
        8,
    ));
    Roots::new(paths)
}
pub fn codex_roots(home: &Path, config: Option<&str>) -> Roots {
    let mut homes = vec![home.join(".codex")];
    if let Some(config) = config {
        homes.extend(parse_paths(config, home));
    }
    Roots::new(
        homes
            .into_iter()
            .flat_map(|p| [p.join("sessions"), p.join("archived_sessions")])
            .collect(),
    )
}
pub fn embedded_claude(base: &Path, max_depth: usize) -> Vec<PathBuf> {
    fn visit(
        path: &Path,
        depth: usize,
        limit: usize,
        visited: &mut BTreeSet<PathBuf>,
        output: &mut Vec<PathBuf>,
        budget: &mut usize,
    ) {
        if depth > limit || *budget == 0 || !path.is_dir() || !visited.insert(normalize(path)) {
            return;
        }
        *budget -= 1;
        if path.file_name().is_some_and(|s| s == "projects")
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|s| s == ".claude")
        {
            output.push(path.to_path_buf());
            return;
        }
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if entry.file_type().is_ok_and(|t| t.is_symlink()) {
                continue;
            }
            if matches!(
                entry.file_name().to_str(),
                Some("node_modules" | ".git" | "vendor" | ".venv" | "venv" | "__pycache__")
            ) {
                continue;
            }
            visit(&entry.path(), depth + 1, limit, visited, output, budget);
        }
    }
    let mut output = vec![];
    visit(
        base,
        0,
        max_depth.min(32),
        &mut BTreeSet::new(),
        &mut output,
        &mut 20_000,
    );
    output
}

pub fn safe_environment_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
        && !name.as_bytes()[0].is_ascii_digit()
}
pub fn parse_marked_value<'a>(output: &'a str, name: &str) -> Option<&'a str> {
    let start = format!("<<<BIN:{name}:");
    let rest = output.split_once(&start)?.1;
    let value = rest.split_once(":BIN>>>")?.0;
    (!value.trim().is_empty()).then_some(value)
}
pub fn resolve_environment(
    names: &[&str],
    process: &BTreeMap<String, String>,
    shell: impl FnOnce(&[&str]) -> BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    let mut missing = vec![];
    for &name in names {
        if !safe_environment_name(name) {
            continue;
        }
        if let Some(value) = process.get(name).filter(|v| !v.trim().is_empty()) {
            result.insert(name.into(), value.clone());
        } else {
            missing.push(name);
        }
    }
    if !missing.is_empty() {
        for (name, value) in shell(&missing) {
            if missing.contains(&name.as_str()) && !value.trim().is_empty() {
                result.insert(name, value);
            }
        }
    }
    result
}
fn shell_values(names: &[&str]) -> BTreeMap<String, String> {
    let names = names
        .iter()
        .copied()
        .filter(|n| safe_environment_name(n))
        .collect::<Vec<_>>();
    if names.is_empty() {
        return BTreeMap::new();
    }
    // Only validated variable names enter code; values are expanded by the user's own shell.
    let code = names
        .iter()
        .map(|name| format!("printf '<<<BIN:{name}:%s:BIN>>>' \"${name}\"\n"))
        .collect::<String>();
    let shell = crate::scripts::shell();
    let output = crate::proc::wait_bounded(
        std::process::Command::new(&shell),
        Path::new("/"),
        &shell,
        &["-lic", &code],
        5,
        true,
    )
    .unwrap_or_default();
    names
        .into_iter()
        .filter_map(|name| parse_marked_value(&output, name).map(|v| (name.into(), v.into())))
        .collect()
}
fn environment() -> &'static BTreeMap<String, String> {
    static VALUES: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    VALUES.get_or_init(|| {
        // Tests always inject roots and cannot execute shell startup files or scan real accounts.
        #[cfg(test)]
        {
            BTreeMap::new()
        }
        #[cfg(not(test))]
        {
            resolve_environment(
                LOCATION_VARIABLES,
                &LOCATION_VARIABLES
                    .iter()
                    .filter_map(|name| {
                        std::env::var(name)
                            .ok()
                            .map(|value| ((*name).into(), value))
                    })
                    .collect(),
                shell_values,
            )
        }
    })
}

#[cfg(test)]
#[path = "roots_tests.rs"]
mod tests;

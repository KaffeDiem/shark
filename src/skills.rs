//! Portable skill imports. Preparation snapshots files; installation never runs their code.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

const MAX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_FILES: usize = 2000;
const SOURCE_FILE: &str = ".shark-source.json";

pub struct Skill {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub compatibility: Option<String>,
    pub has_scripts: bool,
    directory: PathBuf,
}

pub struct Import {
    pub skills: Vec<Skill>,
    pub source: String,
    pub revision: Option<String>,
    pub plugin: bool,
    _snapshot: Scratch,
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(parent: &Path) -> Result<Self, String> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
        let name: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let path = parent.join(format!(".shark-skills-{name}"));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = remove_owned_tree(&self.0);
    }
}

fn remove_owned_tree(path: &Path) -> std::io::Result<()> {
    fn make_writable(path: &Path) -> std::io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() {
            return Ok(());
        }
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(metadata.permissions().mode() | 0o700),
        )?;
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                make_writable(&entry.path())?;
            }
        }
        Ok(())
    }
    fs::remove_dir_all(path).or_else(|_| {
        make_writable(path)?;
        fs::remove_dir_all(path)
    })
}

pub fn prepare(source: &str) -> Result<Import, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("Choose a skill folder or enter a GitHub URL.".into());
    }
    let scratch = Scratch::new(&std::env::temp_dir())?;
    let (root, revision, plugin) = if source.starts_with("https://") {
        github(source, &scratch.0)?
    } else if source.contains("://") {
        return Err("Use an HTTPS GitHub URL or a local skill folder.".into());
    } else {
        let expanded = source
            .strip_prefix("~/")
            .map(|tail| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(tail));
        let path = expanded.unwrap_or_else(|| PathBuf::from(source));
        let path = fs::canonicalize(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let root = if path.is_file() {
            if path.file_name().is_none_or(|n| n != "SKILL.md") {
                return Err("Choose a SKILL.md file or a folder containing skills.".into());
            }
            path.parent().unwrap().to_path_buf()
        } else {
            path
        };
        let plugin = plugin_at(&root);
        (root, None, plugin)
    };
    let mut directories = vec![];
    discover(&root, 0, &mut directories)?;
    if directories.is_empty() {
        return Err("No skills found. Choose a folder containing <skill>/SKILL.md.".into());
    }
    let mut skills = vec![];
    let mut budget = (0, 0);
    for (index, directory) in directories.into_iter().enumerate() {
        let snapshot = scratch.0.join(format!("skill-{index}"));
        let executable = copy_tree(&directory, &snapshot, &mut budget)?;
        let instructions = fs::read_to_string(snapshot.join("SKILL.md"))
            .map_err(|e| format!("{}: {e}", directory.join("SKILL.md").display()))?;
        let metadata = metadata(&instructions)
            .map_err(|e| format!("{}: {e}", directory.join("SKILL.md").display()))?;
        let name = metadata.get("name").cloned().unwrap_or_default();
        if !valid_name(&name) {
            return Err(format!(
                "Invalid skill name {name:?}. Use lowercase letters, digits and single hyphens (1–64 characters)."
            ));
        }
        let description = metadata.get("description").cloned().unwrap_or_default();
        if description.is_empty() {
            return Err(format!(
                "Skill {name} needs a description in its SKILL.md frontmatter."
            ));
        }
        skills.push(Skill {
            name,
            description,
            instructions: instructions
                .lines()
                .skip(1)
                .skip_while(|line| *line != "---")
                .skip(1)
                .collect::<Vec<_>>()
                .join("\n"),
            compatibility: metadata.get("compatibility").cloned(),
            has_scripts: executable || snapshot.join("scripts").exists(),
            directory: snapshot,
        });
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Import {
        skills,
        source: source.into(),
        revision,
        plugin,
        _snapshot: scratch,
    })
}

impl Import {
    pub fn install(&self, destination: &Path, selected: &[usize]) -> Result<(), String> {
        if selected.is_empty() {
            return Err("Select at least one skill.".into());
        }
        safe_destination(destination)?;
        let mut names = BTreeSet::new();
        for &index in selected {
            let skill = self
                .skills
                .get(index)
                .ok_or("The selected skill is no longer available.")?;
            if !names.insert(&skill.name) {
                return Err(format!(
                    "Two selected skills are named {}. Select only one.",
                    skill.name
                ));
            }
            if fs::symlink_metadata(destination.join(&skill.name)).is_ok() {
                return Err(format!(
                    "{} is already installed. Existing skills are never overwritten.",
                    skill.name
                ));
            }
        }
        fs::create_dir_all(destination).map_err(|e| e.to_string())?;
        let staging = Scratch::new(destination)?;
        let mut budget = (0, 0);
        for &index in selected {
            let skill = &self.skills[index];
            let target = staging.0.join(&skill.name);
            copy_tree(&skill.directory, &target, &mut budget)?;
            let permissions = fs::metadata(&target)
                .map_err(|e| e.to_string())?
                .permissions();
            fs::set_permissions(
                &target,
                fs::Permissions::from_mode(permissions.mode() | 0o700),
            )
            .map_err(|e| e.to_string())?;
            let provenance = fs::write(
                target.join(SOURCE_FILE),
                serde_json::to_vec_pretty(&serde_json::json!({
                    "source": self.source,
                    "revision": self.revision,
                }))
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string());
            provenance?;
        }
        // Reserve each destination before publishing, so a competing import cannot be overwritten.
        let mut reserved = vec![];
        for &index in selected {
            let target = destination.join(&self.skills[index].name);
            if let Err(e) = fs::create_dir(&target) {
                for path in &reserved {
                    let _ = fs::remove_dir(path);
                }
                return Err(format!(
                    "{}: {e}. No existing skill was replaced.",
                    target.display()
                ));
            }
            reserved.push(target);
        }
        for (&index, target) in selected.iter().zip(&reserved) {
            let skill = &self.skills[index];
            // macOS requires owner write access while moving a read-only directory.
            let publish = fs::rename(staging.0.join(&skill.name), target).and_then(|()| {
                fs::set_permissions(target, fs::metadata(&skill.directory)?.permissions())
            });
            if let Err(e) = publish {
                for path in &reserved {
                    let _ = remove_owned_tree(path);
                }
                return Err(format!(
                    "Could not install {}: {e}",
                    self.skills[index].name
                ));
            }
        }
        Ok(())
    }
}

fn safe_destination(path: &Path) -> Result<(), String> {
    // The agent-owned boundary is agents/<slug>/skills; HOME and system ancestors may be links.
    for ancestor in path.ancestors().take(3) {
        match fs::symlink_metadata(ancestor) {
            Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                return Err(format!(
                    "{} must be a directory, not a link or file.",
                    ancestor.display()
                ));
            }
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
            _ => {}
        }
    }
    Ok(())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn plugin_at(root: &Path) -> bool {
    [".claude-plugin", ".codex-plugin", "hooks/hooks.json"]
        .iter()
        .any(|p| root.join(p).exists())
}

fn discover(root: &Path, depth: usize, found: &mut Vec<PathBuf>) -> Result<(), String> {
    let metadata = fs::symlink_metadata(root).map_err(|e| format!("{}: {e}", root.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(());
    }
    if root.join("SKILL.md").exists() {
        found.push(root.to_path_buf());
        return Ok(());
    }
    if depth == 0 {
        for folder in ["skills", ".agents/skills", ".claude/skills"] {
            if root.join(folder).is_dir() {
                discover(&root.join(folder), 1, found)?;
                if !found.is_empty() {
                    return Ok(());
                }
            }
        }
    }
    if depth >= 6 {
        return Ok(());
    }
    let mut entries = fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if matches!(
            entry.file_name().to_str(),
            Some(".git" | "node_modules" | "target" | ".shark-source.json")
        ) {
            continue;
        }
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            discover(&entry.path(), depth + 1, found)?;
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, target: &Path, budget: &mut (usize, u64)) -> Result<bool, String> {
    let metadata = fs::symlink_metadata(source).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
        return Err(format!(
            "{} contains a link or special file. Import regular files and directories only.",
            source.display()
        ));
    }
    budget.0 += 1;
    budget.1 += if metadata.is_file() {
        metadata.len()
    } else {
        0
    };
    if budget.0 > MAX_FILES || budget.1 > MAX_BYTES {
        return Err("Skills exceed the import limit of 2,000 entries or 32 MiB. Choose a smaller skill folder.".into());
    }
    let mut executable = metadata.is_file() && metadata.permissions().mode() & 0o111 != 0;
    if metadata.is_dir() {
        fs::create_dir(target).map_err(|e| e.to_string())?;
        for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if matches!(
                entry.file_name().to_str(),
                Some(".git" | ".shark-source.json")
            ) {
                continue;
            }
            executable |= copy_tree(&entry.path(), &target.join(entry.file_name()), budget)?;
        }
    } else {
        fs::copy(source, target).map_err(|e| format!("{}: {e}", source.display()))?;
    }
    fs::set_permissions(
        target,
        fs::Permissions::from_mode(metadata.permissions().mode() & 0o777),
    )
    .map_err(|e| e.to_string())?;
    Ok(executable)
}

// shortcut: read scalar preview metadata only; use a YAML parser when skills need richer YAML.
fn metadata(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut lines = text.lines();
    if lines.next() != Some("---") {
        return Err("SKILL.md needs YAML frontmatter with name and description.".into());
    }
    let mut fields = BTreeMap::new();
    let mut current: Option<(String, String, bool)> = None;
    let flush = |current: &mut Option<(String, String, bool)>,
                 fields: &mut BTreeMap<String, String>| {
        if let Some((key, value, _)) = current.take() {
            fields.insert(key, value.trim().to_string());
        }
    };
    for line in lines {
        if line == "---" {
            flush(&mut current, &mut fields);
            return Ok(fields);
        }
        if line.starts_with(char::is_whitespace) || line.is_empty() {
            if let Some((_, value, literal)) = &mut current {
                if !value.is_empty() {
                    value.push(if *literal { '\n' } else { ' ' });
                }
                value.push_str(line.trim());
            }
            continue;
        }
        flush(&mut current, &mut fields);
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if !matches!(key, "name" | "description" | "compatibility") {
            continue;
        }
        if fields.contains_key(key) {
            return Err(format!("Duplicate {key} in SKILL.md frontmatter."));
        }
        let value = value.trim();
        if value.starts_with(['"', '\'']) {
            let unsupported =
                || format!("Unsupported {key} quoting. Use a plain value or a YAML block scalar.");
            let (parsed, end) = if value.starts_with('"') {
                let mut values = serde_json::Deserializer::from_str(value).into_iter::<String>();
                let parsed = values
                    .next()
                    .ok_or_else(unsupported)?
                    .map_err(|_| unsupported())?;
                (parsed, values.byte_offset())
            } else {
                let mut end = 1;
                loop {
                    let next = value[end..].find('\'').ok_or_else(unsupported)? + end;
                    end = next + 1;
                    if value.as_bytes().get(end) != Some(&b'\'') {
                        break;
                    }
                    end += 1;
                }
                (value[1..end - 1].replace("''", "'"), end)
            };
            let tail = value[end..].trim();
            if !tail.is_empty() && !tail.starts_with('#') {
                return Err(unsupported());
            }
            fields.insert(key.into(), parsed);
        } else {
            let value = value.split(" #").next().unwrap().trim_end();
            if matches!(value, ">" | ">-" | ">+" | "|" | "|-" | "|+") {
                current = Some((key.into(), String::new(), value.starts_with('|')));
            } else if value.starts_with(['[', '{', '&', '*', '!']) || value.is_empty() {
                return Err(format!(
                    "{key} must be a plain, quoted or block text value."
                ));
            } else {
                current = Some((key.into(), value.into(), false));
            }
        }
    }
    Err("SKILL.md frontmatter needs a closing --- line.".into())
}

struct Github {
    repository: String,
    location: Vec<String>,
}

fn github_url(source: &str) -> Result<Github, String> {
    if source
        .split('/')
        .any(|part| part == "." || part == ".." || part.contains('%'))
    {
        return Err("Use a GitHub URL without encoded or relative path components.".into());
    }
    let url = url::Url::parse(source).map_err(|e| e.to_string())?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err("Use a normal HTTPS GitHub repository or skill URL.".into());
    }
    let parts: Vec<String> = url
        .path_segments()
        .ok_or("Invalid GitHub URL")?
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect();
    if parts.len() < 2
        || parts
            .iter()
            .any(|p| p.contains('%') || p == "." || p == ".." || p.contains('\\'))
    {
        return Err("Use a GitHub URL without encoded or relative path components.".into());
    }
    let location = match url.host_str() {
        Some("github.com") if parts.len() == 2 => vec![],
        Some("github.com") if parts.len() >= 4 && matches!(parts[2].as_str(), "tree" | "blob") => {
            parts[3..].to_vec()
        }
        Some("raw.githubusercontent.com") if parts.len() >= 4 => parts[2..].to_vec(),
        _ => return Err("Use a GitHub repository, folder or SKILL.md URL.".into()),
    };
    Ok(Github {
        repository: format!(
            "https://github.com/{}/{}.git",
            parts[0],
            parts[1].trim_end_matches(".git")
        ),
        location,
    })
}

fn github(source: &str, scratch: &Path) -> Result<(PathBuf, Option<String>, bool), String> {
    let github = github_url(source)?;
    let mut reference = "HEAD".to_string();
    let mut path = PathBuf::new();
    if !github.location.is_empty() {
        let location = github.location.join("/");
        let location = location
            .strip_prefix("refs/heads/")
            .or_else(|| location.strip_prefix("refs/tags/"))
            .unwrap_or(&location);
        let refs = crate::git::cmd_bounded(
            scratch,
            "git",
            &["ls-remote", "--heads", "--tags", &github.repository],
            30,
        )?;
        let mut matches = refs
            .lines()
            .filter_map(|line| {
                let (_, name) = line.split_once('\t')?;
                let name = name
                    .strip_prefix("refs/heads/")
                    .or_else(|| name.strip_prefix("refs/tags/"))?;
                (!name.ends_with("^{}")
                    && (location == name || location.starts_with(&format!("{name}/"))))
                .then_some(name)
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|name| std::cmp::Reverse(name.len()));
        reference = matches
            .first()
            .map(|r| r.to_string())
            .unwrap_or_else(|| github.location[0].clone());
        if matches.is_empty()
            && !(reference.len() >= 7 && reference.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(
                "The GitHub branch or tag was not found. Check the URL and repository access."
                    .into(),
            );
        }
        path = PathBuf::from(
            location
                .strip_prefix(&reference)
                .unwrap_or("")
                .trim_start_matches('/'),
        );
    }
    let repository = scratch.join("repository");
    fs::create_dir(&repository).map_err(|e| e.to_string())?;
    crate::git::git(&repository, &["init", "--bare", "--quiet"])?;
    for (key, value) in [
        ("core.hooksPath", "/dev/null"),
        ("remote.origin.url", github.repository.as_str()),
        ("remote.origin.promisor", "true"),
        ("remote.origin.partialclonefilter", "blob:none"),
    ] {
        crate::git::git(&repository, &["config", key, value])?;
    }
    crate::git::cmd_bounded(
        &repository,
        "git",
        &[
            "fetch",
            "--depth=1",
            "--no-tags",
            "--filter=blob:none",
            "--",
            "origin",
            &reference,
        ],
        60,
    )?;
    let revision = crate::git::git(&repository, &["rev-parse", "FETCH_HEAD"])?;
    let tree = crate::git::git_bytes_bounded(&repository, &["ls-tree", "-rz", "FETCH_HEAD"], 30)?;
    let mut files = vec![];
    for entry in tree.split(|&b| b == 0).filter(|entry| !entry.is_empty()) {
        let entry = std::str::from_utf8(entry).map_err(|_| "Skill paths must be UTF-8.")?;
        let (meta, name) = entry.split_once('\t').ok_or("Invalid Git tree entry")?;
        let parts: Vec<_> = meta.split_whitespace().collect();
        files.push((
            parts[0].to_string(),
            parts[2].to_string(),
            PathBuf::from(name),
        ));
    }
    let plugin = files.iter().any(|(_, _, p)| {
        p.starts_with(".claude-plugin")
            || p.starts_with(".codex-plugin")
            || p == Path::new("hooks/hooks.json")
    });
    if path.file_name().is_some_and(|n| n == "SKILL.md") {
        path.pop();
    }
    if path.as_os_str().is_empty() {
        for conventional in ["skills", ".agents/skills", ".claude/skills"] {
            if files.iter().any(|(_, _, p)| {
                p.starts_with(conventional) && p.file_name().is_some_and(|n| n == "SKILL.md")
            }) {
                path = conventional.into();
                break;
            }
        }
    }
    let roots: Vec<_> = files
        .iter()
        .filter(|(_, _, p)| p.starts_with(&path) && p.file_name().is_some_and(|n| n == "SKILL.md"))
        .map(|(_, _, p)| p.parent().unwrap().to_path_buf())
        .collect();
    if roots.is_empty() {
        return Err("No SKILL.md files found at that GitHub location.".into());
    }
    let root = scratch.join("download");
    fs::create_dir(&root).map_err(|e| e.to_string())?;
    let mut total = 0;
    let mut count = 0;
    for (mode, object, name) in files {
        if !roots.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        count += 1;
        if count > MAX_FILES {
            return Err("GitHub skills exceed the import limit of 2,000 files or 32 MiB.".into());
        }
        if !matches!(mode.as_str(), "100644" | "100755")
            || name
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(format!(
                "{} is a link, submodule or unsafe path. Import regular files only.",
                name.display()
            ));
        }
        let size: u64 =
            crate::git::cmd_bounded(&repository, "git", &["cat-file", "-s", &object], 30)?
                .parse()
                .map_err(|_| "Invalid Git blob size.")?;
        total += size;
        if total > MAX_BYTES {
            return Err("GitHub skills exceed the import limit of 32 MiB.".into());
        }
        let target = root.join(&name);
        fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
        let bytes = crate::git::git_bytes_bounded(&repository, &["cat-file", "blob", &object], 30)?;
        fs::write(&target, bytes).map_err(|e| e.to_string())?;
        if mode == "100755" {
            fs::set_permissions(&target, fs::Permissions::from_mode(0o755))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok((root.join(path), Some(revision), plugin))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(root: &Path, name: &str) -> PathBuf {
        let path = root.join(name);
        fs::create_dir_all(path.join("scripts")).unwrap();
        fs::write(path.join("SKILL.md"), format!("---\nname: {name}\ndescription: >\n  A useful\n  skill.\ncompatibility: Requires Python\n---\n# Instructions\n")).unwrap();
        fs::write(path.join("scripts/helper.sh"), "touch MUST_NOT_RUN").unwrap();
        path
    }

    #[test]
    fn import_snapshots_resources_and_never_overwrites() {
        let temp = Scratch::new(&std::env::temp_dir()).unwrap();
        let skill = fixture(&temp.0, "example");
        let import = prepare(skill.join("SKILL.md").to_str().unwrap()).unwrap();
        assert_eq!(import.skills[0].description, "A useful skill.");
        assert_eq!(import.skills[0].instructions, "# Instructions");
        assert!(import.skills[0].has_scripts);
        fs::write(skill.join("scripts/helper.sh"), "changed after preview").unwrap();
        let dest = fs::canonicalize(&temp.0).unwrap().join("installed");
        import.install(&dest, &[0]).unwrap();
        assert_eq!(
            fs::read_to_string(dest.join("example/scripts/helper.sh")).unwrap(),
            "touch MUST_NOT_RUN"
        );
        assert!(dest.join("example/.shark-source.json").is_file());
        assert!(!temp.0.join("MUST_NOT_RUN").exists());
        assert!(
            import
                .install(&dest, &[0])
                .unwrap_err()
                .contains("already installed")
        );
    }

    #[test]
    fn discovers_bundle_and_preflights_all_conflicts() {
        let temp = Scratch::new(&std::env::temp_dir()).unwrap();
        fixture(&temp.0.join("skills"), "first");
        fixture(&temp.0.join("skills"), "second");
        fs::create_dir(temp.0.join(".claude-plugin")).unwrap();
        let import = prepare(temp.0.to_str().unwrap()).unwrap();
        assert!(import.plugin);
        assert_eq!(import.skills.len(), 2);
        let dest = fs::canonicalize(&temp.0).unwrap().join("installed");
        fs::create_dir_all(dest.join("second")).unwrap();
        assert!(import.install(&dest, &[0, 1]).is_err());
        assert!(!dest.join("first").exists());
    }

    #[test]
    fn rejects_embedded_links_and_linked_destinations() {
        let temp = Scratch::new(&std::env::temp_dir()).unwrap();
        let skill = fixture(&temp.0, "example");
        std::os::unix::fs::symlink("/etc/passwd", skill.join("secret")).unwrap();
        assert!(
            prepare(skill.to_str().unwrap())
                .err()
                .unwrap()
                .contains("link")
        );
        fs::remove_file(skill.join("secret")).unwrap();
        let import = prepare(skill.to_str().unwrap()).unwrap();
        let target = temp.0.join("linked");
        std::os::unix::fs::symlink(&skill, &target).unwrap();
        assert!(import.install(&target, &[0]).is_err());
    }

    #[test]
    fn metadata_and_names_are_validated() {
        assert_eq!(
            metadata("---\nname: 'example'\ndescription: \"Use it.\"\n---\n").unwrap()["description"],
            "Use it."
        );
        assert!(metadata("---\nname: foo\nname: bar\n---").is_err());
        assert!(metadata("---\ndescription: [a, b]\n---").is_err());
        assert!(metadata("# No frontmatter").is_err());
        for name in ["../evil", "", "a/b", "A", "a--b", "-a", "a-"] {
            assert!(!valid_name(name));
        }
    }

    #[test]
    fn github_urls_resolve_to_repository_and_location() {
        let repo = github_url("https://github.com/dietrichgebert/ponytail").unwrap();
        assert_eq!(
            repo.repository,
            "https://github.com/dietrichgebert/ponytail.git"
        );
        for source in [
            "https://github.com/a/b/blob/main/skills/foo/SKILL.md",
            "https://raw.githubusercontent.com/a/b/main/skills/foo/SKILL.md",
        ] {
            assert_eq!(
                github_url(source).unwrap().location,
                ["main", "skills", "foo", "SKILL.md"]
            );
        }
        for source in [
            "https://github.com.evil.test/a/b",
            "https://user:pass@github.com/a/b",
            "https://github.com/a/b/tree/main/%2e%2e/private",
            "https://example.com/SKILL.md",
        ] {
            assert!(github_url(source).is_err());
        }
    }
    #[test]
    fn quoted_metadata_keeps_hashes_and_accepts_trailing_comments() {
        let fields = metadata("---\nname: 'review' # name\ndescription: \"Keep # hash\" # comment\ncompatibility: > # folded\n  Requires Git\n---\n").unwrap();
        assert_eq!(fields["name"], "review");
        assert_eq!(fields["description"], "Keep # hash");
        assert_eq!(fields["compatibility"], "Requires Git");
        assert_eq!(
            metadata("---\nname: foo\ndescription: 'It''s useful' # comment\n---").unwrap()["description"],
            "It's useful"
        );
        assert!(metadata("---\nname: 'unterminated\n---").is_err());
    }

    #[test]
    fn empty_conventional_folder_does_not_hide_skills() {
        let temp = Scratch::new(&std::env::temp_dir()).unwrap();
        fs::create_dir(temp.0.join("skills")).unwrap();
        fixture(&temp.0.join(".agents/skills"), "review");
        let import = prepare(temp.0.to_str().unwrap()).unwrap();
        assert_eq!(import.skills.len(), 1);
        assert_eq!(import.skills[0].name, "review");
    }

    #[test]
    fn preserves_private_files_and_readonly_skill_directory() {
        let temp = Scratch::new(&std::env::temp_dir()).unwrap();
        let skill = fixture(&temp.0, "private");
        fs::set_permissions(&skill, fs::Permissions::from_mode(0o500)).unwrap();
        fs::set_permissions(skill.join("scripts"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(
            skill.join("scripts/helper.sh"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let import = prepare(skill.to_str().unwrap()).unwrap();
        let destination = temp.0.join("agents/example/skills");
        import.install(&destination, &[0]).unwrap();
        for (path, mode) in [
            ("private", 0o500),
            ("private/scripts", 0o700),
            ("private/scripts/helper.sh", 0o600),
        ] {
            assert_eq!(
                fs::metadata(destination.join(path))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                mode
            );
        }
        fs::set_permissions(&skill, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(
            destination.join("private"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let snapshot = import._snapshot.0.clone();
        drop(import);
        assert!(
            !snapshot.exists(),
            "read-only snapshots must be removed on drop"
        );
    }

    #[test]
    #[ignore = "requires GitHub network access"]
    fn github_live_ponytail() {
        for source in [
            "https://github.com/DietrichGebert/ponytail",
            "https://raw.githubusercontent.com/DietrichGebert/ponytail/main/skills/ponytail/SKILL.md",
        ] {
            let import = prepare(source).unwrap();
            let selected = import
                .skills
                .iter()
                .position(|s| s.name == "ponytail")
                .unwrap();
            assert!(import.plugin);
            assert!(import.revision.as_ref().is_some_and(|r| r.len() == 40));
            let temp = Scratch::new(&std::env::temp_dir()).unwrap();
            import
                .install(&temp.0.join("agents/example/skills"), &[selected])
                .unwrap();
            assert!(
                temp.0
                    .join("agents/example/skills/ponytail/SKILL.md")
                    .is_file()
            );
            assert_eq!(
                fs::read_dir(temp.0.join("agents/example/skills"))
                    .unwrap()
                    .count(),
                1
            );
        }
    }
}

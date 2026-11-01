use super::*;
use std::os::unix::fs::symlink;
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "shark-roots-{}-{}",
            std::process::id(),
            crate::pokemon::seed()
        ));
        fs::create_dir_all(&p).unwrap();
        Self(p.canonicalize().unwrap())
    }
    fn dir(&self, relative: &str) -> PathBuf {
        let p = self.0.join(relative);
        fs::create_dir_all(&p).unwrap();
        p
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn extra_count(defaults: Vec<PathBuf>, custom: &str, home: &Path) -> usize {
    let defaults = normalized(defaults);
    with_custom(defaults.clone(), Some(custom), home)
        .0
        .iter()
        .filter(|p| !defaults.contains(p))
        .count()
}
fn map(items: &[(&str, &str)]) -> BTreeMap<String, String> {
    items
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn glob_expansion_sorts_matches_and_keeps_only_directories() {
    let t = Temp::new();
    let b = t.dir("instances/b-inst/projects");
    let a = t.dir("instances/a-inst/projects");
    t.dir("instances/empty");
    let literal = t.dir("literal");
    fs::write(t.0.join("file"), "x").unwrap();
    let raw = format!(
        "{}/instances/*/projects, {}\n{}/nope/*, {}/file, relative/path",
        t.0.display(),
        literal.display(),
        t.0.display(),
        t.0.display()
    );
    assert_eq!(expand(&raw, &t.0), [a, b, literal]);
    assert_eq!(
        expand(
            &format!("{}/instances/[ab]-inst/projects", t.0.display()),
            &t.0
        )
        .len(),
        2
    );
}
#[test]
fn tilde_and_blank_fragments() {
    let t = Temp::new();
    // A bare home directory would scan the whole account, so it is ignored.
    assert!(expand("  , ~/ , \n\t ", &t.0).is_empty());
    assert_eq!(expand("~/sub", &t.0).len(), 0);
    let sub = t.dir("sub");
    assert_eq!(expand(" ~/sub ", &t.0), [sub]);
}
#[test]
fn custom_roots_are_additive() {
    let t = Temp::new();
    let extra = t.dir("extra");
    let defaults = vec![
        t.0.join(".claude/projects"),
        t.0.join(".config/claude/projects"),
    ];
    assert_eq!(with_custom(defaults.clone(), None, &t.0).0, defaults);
    let union = with_custom(defaults.clone(), extra.to_str(), &t.0).0;
    assert!(defaults.iter().all(|p| union.contains(p)));
    assert!(union.contains(&extra));
}
#[test]
fn ancestor_custom_root_cannot_hide_curated_dot_directory() {
    let t = Temp::new();
    let projects = t.dir(".claude/projects");
    let file = projects.join("s.jsonl");
    fs::write(&file, "{}\n").unwrap();
    let elsewhere = Path::new("/nonexistent-home");
    let union = with_custom(vec![projects.clone()], t.0.to_str(), elsewhere);
    assert_eq!(union.0, [projects.clone()]);
    assert_eq!(fs::read_dir(&union.0[0]).unwrap().count(), 1);
    assert_eq!(
        extra_count(vec![projects], t.0.to_str().unwrap(), elsewhere),
        0
    );
}
#[test]
fn nested_extra_is_already_covered() {
    let t = Temp::new();
    let base = t.dir(".claude/projects");
    let extra = t.dir(".claude/projects/extra");
    assert_eq!(extra_count(vec![base], extra.to_str().unwrap(), &t.0), 0);
}
#[test]
fn filesystem_root_cannot_replace_curated_sources() {
    let t = Temp::new();
    let defaults = vec![t.dir(".claude/projects")];
    assert!(expand("/", &t.0).is_empty());
    assert!(parse_paths("/, ~, ~/", &t.0).is_empty());
    assert_eq!(with_custom(defaults.clone(), Some("/"), &t.0).0, defaults);
}
#[test]
fn sibling_extra_is_counted() {
    let t = Temp::new();
    let defaults = vec![t.dir(".claude/projects")];
    let extra = t.dir("elsewhere");
    assert_eq!(extra_count(defaults, extra.to_str().unwrap(), &t.0), 1);
}
#[test]
fn computed_roots_only_use_explicit_extras() {
    let t = Temp::new();
    let extra = t.dir("extra");
    let defaults = claude_roots(&t.0, None);
    assert!(!defaults.0.contains(&extra));
    assert!(
        with_custom(defaults.0, extra.to_str(), &t.0)
            .0
            .contains(&extra)
    );
}
#[test]
fn codex_roots_include_active_and_archived_under_injected_home() {
    let t = Temp::new();
    let roots = codex_roots(&t.0, Some("~/second"));
    for p in [
        ".codex/sessions",
        ".codex/archived_sessions",
        "second/sessions",
        "second/archived_sessions",
    ] {
        assert!(roots.0.contains(&t.0.join(p)));
    }
    let extra = t.dir("extra");
    assert!(
        with_custom(roots.0, extra.to_str(), &t.0)
            .0
            .contains(&extra)
    );
}
#[test]
fn config_directory_lists_trim_and_expand_tilde() {
    let t = Temp::new();
    let roots = claude_roots(&t.0, Some(" /a/one , ,~/two "));
    for p in [
        PathBuf::from("/a/one/projects"),
        t.0.join("two/projects"),
        t.0.join(CLAUDE_PROJECTS),
        t.0.join(".config/claude/projects"),
    ] {
        assert!(roots.0.contains(&p));
    }
    assert!(!roots.0.contains(&PathBuf::from("/projects")));
}
#[test]
fn symlinked_duplicates_fold_once() {
    let t = Temp::new();
    let real = t.dir("real/projects");
    symlink(t.0.join("real"), t.0.join("linked")).unwrap();
    assert_eq!(
        Roots::new(vec![real.clone(), t.0.join("linked/projects")]).0,
        [real]
    );
}
#[test]
fn normalization_preserves_distinct_prefix_siblings() {
    let root = PathBuf::from("/Users/x/.claude/projects");
    let other = PathBuf::from("/Users/x/.config/claude/projects");
    let sibling = PathBuf::from("/Users/x/.claude/projects-other");
    assert_eq!(
        normalized(vec![
            root.clone(),
            other.clone(),
            root.clone(),
            root.join("sub"),
            sibling.clone()
        ]),
        [root, other, sibling]
    );
}
#[test]
fn default_claude_path_uses_shared_constant_once() {
    let home = Path::new("/Users/testhome");
    let roots = claude_roots(home, None);
    assert!(roots.0.contains(&home.join(CLAUDE_PROJECTS)));
    assert_eq!(roots.0.len(), roots.0.iter().collect::<BTreeSet<_>>().len());
}
#[test]
fn embedded_roots_find_hidden_projects() {
    let t = Temp::new();
    let projects = t.dir("u1/u2/local_123/.claude/projects");
    t.dir("u1/u2/local_123/uploads");
    assert_eq!(embedded_claude(&t.0, 8), [projects]);
}
#[test]
fn embedded_roots_respect_depth_and_missing_bases() {
    let t = Temp::new();
    assert!(embedded_claude(&t.0.join("missing"), 8).is_empty());
    t.dir("a/b/c/d/e/f/g/.claude/projects");
    assert!(embedded_claude(&t.0, 4).is_empty());
}
#[test]
fn embedded_depth_boundary_matches_real_workspace_layout() {
    let t = Temp::new();
    t.dir("u1/u2/local_x/.claude/projects");
    assert_eq!(embedded_claude(&t.0, 4).len(), 0);
    assert_eq!(embedded_claude(&t.0, 5).len(), 1);
    t.dir("u1/u2/local_x/outputs/repo/.claude/projects");
    assert_eq!(embedded_claude(&t.0, 8).len(), 2);
    assert_eq!(embedded_claude(&t.0, 6).len(), 1);
}
#[test]
fn embedded_roots_skip_bulk_but_keep_real_work_directory_names() {
    let t = Temp::new();
    for name in ["outputs/repo", "uploads/repo", "build", "target"] {
        t.dir(&format!("u1/u2/local_x/{name}/.claude/projects"));
    }
    t.dir("u1/u2/local_x/node_modules/pkg/.claude/projects");
    let roots = embedded_claude(&t.0, 8);
    assert_eq!(roots.len(), 4);
    assert!(
        !roots
            .iter()
            .any(|p| p.to_string_lossy().contains("node_modules"))
    );
}

#[test]
fn complete_process_environment_never_spawns_shell() {
    let values = map(&[("A_HOME", "/a"), ("B_HOME", "/b")]);
    assert_eq!(
        resolve_environment(&["A_HOME", "B_HOME"], &values, |_| panic!(
            "unexpected shell"
        )),
        values
    );
}
#[test]
fn missing_names_are_looked_up_together() {
    let mut calls = 0;
    let values = resolve_environment(
        &["A_HOME", "B_HOME", "C_HOME"],
        &map(&[("B_HOME", "/b")]),
        |names| {
            calls += 1;
            assert_eq!(names, ["A_HOME", "C_HOME"]);
            map(&[
                ("A_HOME", "/shell/a"),
                ("C_HOME", "/shell/c"),
                ("B_HOME", "/wrong"),
            ])
        },
    );
    assert_eq!(calls, 1);
    assert_eq!(
        values,
        map(&[
            ("A_HOME", "/shell/a"),
            ("B_HOME", "/b"),
            ("C_HOME", "/shell/c")
        ])
    );
}
#[test]
fn blank_values_are_absent() {
    assert_eq!(
        resolve_environment(&["A_HOME"], &map(&[("A_HOME", "  ")]), |_| map(&[(
            "A_HOME", "/real"
        )])),
        map(&[("A_HOME", "/real")])
    );
    assert!(
        resolve_environment(&["A_HOME"], &BTreeMap::new(), |_| map(&[(
            "A_HOME", " \n "
        )]))
        .is_empty()
    );
}
#[test]
fn marker_parsing_handles_noise_and_prefixes() {
    let raw = "banner\n<<<BIN:A_HOME_EXTRA:/extra:BIN>>>noise<<<BIN:A_HOME:/first:BIN>>>end";
    assert_eq!(parse_marked_value(raw, "A_HOME"), Some("/first"));
    assert_eq!(parse_marked_value(raw, "A_HOME_EXTRA"), Some("/extra"));
    assert!(parse_marked_value(raw, "C_HOME").is_none());
    assert!(parse_marked_value("<<<BIN:A_HOME::BIN>>>", "A_HOME").is_none());
}
#[test]
fn environment_lookup_rejects_shell_injection_names() {
    for name in ["GROK_HOME", "A1_B2", "X"] {
        assert!(safe_environment_name(name));
    }
    for name in [
        "",
        "grok_home",
        "A-B",
        "A B",
        "A;rm -rf /",
        "Σ",
        "А",
        "٣",
        "A$B",
        "2BAD",
    ] {
        assert!(!safe_environment_name(name));
    }
    assert!(shell_values(&[]).is_empty());
    assert!(shell_values(&["bad name", "x"]).is_empty());
}
#[test]
fn location_registry_covers_supported_tools_without_credential_variables() {
    assert_eq!(LOCATION_VARIABLES, ["CLAUDE_CONFIG_DIR", "CODEX_HOME"]);
    assert_eq!(
        LOCATION_VARIABLES.iter().collect::<BTreeSet<_>>().len(),
        LOCATION_VARIABLES.len()
    );
    assert!(
        LOCATION_VARIABLES
            .iter()
            .all(|name| safe_environment_name(name))
    );
}

#[test]
fn companion_scanners_share_registered_environment_lookup() {
    for source in [
        include_str!("../logs.rs"),
        include_str!("../providers.rs"),
        include_str!("claude.rs"),
        include_str!("codex.rs"),
        include_str!("cache.rs"),
    ] {
        assert!(!source.contains("std::env::var("));
        assert!(!source.contains("std::env::var_os("));
    }
    let source = include_str!("roots.rs");
    for name in LOCATION_VARIABLES {
        assert!(source.contains(&format!("environment.get(\"{name}\")")));
    }
}
#[test]
fn tests_use_injected_locations_without_reading_user_environment() {
    assert!(environment().is_empty());
    let t = Temp::new();
    let a = t.dir("a");
    assert_eq!(Roots::new(vec![a.clone()]).0, [a]);
    let dirs = codex_roots(&t.0, Some("~/injected"));
    assert!(dirs.0.iter().all(|p| p.starts_with(&t.0)));
    assert!(dirs.0.contains(&t.0.join("injected/sessions")));
}

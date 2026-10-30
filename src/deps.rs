//! Launch check for the tools Shark shells out to: git, gh (logged in) and claude.
use gpui_kit::assets::IconName;
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::Path;
use std::sync::OnceLock;

/// Why `fix_path` couldn't borrow the login shell's PATH; set once at launch.
pub static PATH_ERROR: OnceLock<String> = OnceLock::new();

#[derive(Clone, Debug, PartialEq)]
pub struct Dep {
    pub name: &'static str,
    pub ok: bool,
    /// A command that fixes it, to copy into a terminal; empty when there is none.
    pub fix: &'static str,
    /// Why it's missing; empty when ok.
    pub detail: String,
}

/// The login shell's PATH. Bounded: an rc file that blocks (pinentry, a network call) must
/// not hang launch. In its own process group so a timeout also kills what the rc started.
pub fn login_path() -> Result<String, String> {
    let shell = std::env::var("SHELL").unwrap_or("/bin/zsh".into());
    let probe = ["-lic", "printf '__P__%s' \"$PATH\""];
    let c = std::process::Command::new(&shell);
    let out = crate::git::wait_bounded(c, Path::new("/"), &shell, &probe, 5, true)?;
    let path = (out.split("__P__").nth(1)).ok_or(format!("{shell} printed no PATH"))?;
    Ok(path.trim().to_string())
}

/// `gh auth status` for github.com's active account only: a stale token on another host or
/// account makes the plain command fail. ponytail: needs gh 2.40+ for `--active`.
fn gh_auth() -> Result<String, String> {
    let args = ["auth", "status", "--hostname", "github.com", "--active"];
    crate::git::cmd_bounded(Path::new("/"), "gh", &args, 10)
}

/// Blocking (spawns `gh auth status`, maybe a login shell); run it off the UI thread.
pub fn check() -> Vec<Dep> {
    let claude = std::env::var("SHARK_CLAUDE").unwrap_or("claude".into());
    let path = std::env::var("PATH").unwrap_or_default();
    let path_err = PATH_ERROR.get().map(|s| s.as_str());
    let mut deps = rows(path_err, |p| on_path(p, &path), &claude, gh_auth);
    restart_hints(&mut deps, &claude, login_path);
    // Azure DevOps is optional: checked only once az is installed.
    if on_path(&crate::azure::az(), &path) {
        deps.push(azure_dep());
    }
    deps
}

/// The azure-devops extension, then a login; the first missing one.
fn azure_dep() -> Dep {
    let az = crate::azure::az();
    let run = |args: &[&str]| crate::git::cmd_bounded(Path::new("/"), &az, args, 15);
    let (name, fix, result) =
        match run(&["extension", "show", "--name", "azure-devops", "-o", "none"]) {
            Err(e) => (
                "Azure DevOps extension",
                "az extension add --name azure-devops",
                Err(e),
            ),
            Ok(_) => (
                "Azure login",
                "az login",
                run(&["account", "show", "-o", "none"]),
            ),
        };
    let detail = match &result {
        Err(e) if e.contains("timed out") => e.clone(),
        Err(_) if name == "Azure login" => "Not logged in to Azure".into(),
        Err(_) => "Needed for Azure DevOps repos".into(),
        Ok(_) => String::new(),
    };
    Dep {
        name,
        ok: result.is_ok(),
        fix,
        detail,
    }
}

/// PATH is read once at launch, so a tool installed since then (or a fixed rc file) only
/// works after a restart. Say so instead of repeating the install command. `fresh` reads the
/// login shell's PATH now; it only runs when a missing program could be on it.
fn restart_hints(deps: &mut [Dep], claude: &str, fresh: impl FnOnce() -> Result<String, String>) {
    let missing = |d: &Dep| !d.ok && !d.fix.is_empty() && d.name != "GitHub login";
    if !deps.iter().any(missing) {
        return;
    }
    let fresh = fresh();
    for d in deps.iter_mut().filter(|d| missing(d)) {
        let prog = if d.name == "claude" { claude } else { d.name };
        match &fresh {
            Ok(p) if on_path(prog, p) => {
                d.fix = "";
                d.detail = "Installed after Shark started: restart Shark to use it".into();
            }
            Ok(_) => {}
            // Can't tell whether it's there now; a restart is needed either way.
            Err(_) => d.detail = format!("{}. Install it, then restart Shark", d.detail),
        }
    }
}

/// Something the user can still fix while Shark runs, so a recheck can change the result.
pub fn fixable(deps: &[Dep]) -> bool {
    deps.iter().any(|d| !d.ok && !d.fix.is_empty())
}

/// `found` says whether a program is on PATH; `auth` runs `gh auth status`, only when gh exists.
fn rows(
    path_err: Option<&str>,
    found: impl Fn(&str) -> bool,
    claude: &str,
    auth: impl FnOnce() -> Result<String, String>,
) -> Vec<Dep> {
    let dep = |name, ok, fix, detail: &str| Dep {
        name,
        ok,
        fix,
        detail: if ok { String::new() } else { detail.into() },
    };
    let mut out = vec![];
    if let Some(e) = path_err {
        out.push(dep(
            "Shell PATH",
            false,
            "",
            &format!("Couldn't read your shell PATH: {e}"),
        ));
    }
    let missing = "Not found on your PATH";
    out.push(dep("git", found("git"), "xcode-select --install", missing));
    let gh = found("gh");
    out.push(dep("gh", gh, "brew install gh", missing));
    if gh {
        let auth = auth();
        let detail = match &auth {
            Err(e) if e.contains("timed out") => e.as_str(),
            _ => "Not logged in to GitHub",
        };
        out.push(dep("GitHub login", auth.is_ok(), "gh auth login", detail));
    }
    let detail = if claude == "claude" {
        missing.into()
    } else {
        format!("Not found: {claude}")
    };
    let fix = "curl -fsSL https://claude.ai/install.sh | bash";
    out.push(dep("claude", found(claude), fix, &detail));
    out
}

/// Like `which`: a bare name is looked up in `path`, a path is checked as is.
fn on_path(prog: &str, path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let exe = |p: &Path| {
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if prog.contains('/') {
        return exe(Path::new(prog));
    }
    std::env::split_paths(path).any(|d| exe(&d.join(prog)))
}

/// One row per missing dependency: what, why, and the fix command with a copy button.
/// `None` when everything is there (or the check hasn't finished).
pub fn render_setup(deps: &[Dep], cx: &App) -> Option<Div> {
    let theme = cx.theme();
    let missing: Vec<&Dep> = deps.iter().filter(|d| !d.ok).collect();
    (!missing.is_empty()).then(|| {
        v_flex().gap_3().children(missing.into_iter().map(|d| {
            h_flex()
                .items_start()
                .gap_2()
                .text_sm()
                .child(
                    Icon::new(IconName::TriangleAlert)
                        .with_size(px(14.))
                        .mt(px(3.))
                        .text_color(theme.warning_foreground),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_color(theme.foreground).child(d.name))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(d.detail.clone()),
                        )
                        .when(!d.fix.is_empty(), |c| {
                            c.child(
                                h_flex()
                                    .h(px(28.))
                                    .pl_2()
                                    .pr_1()
                                    .gap_2()
                                    .rounded(theme.radius)
                                    .bg(theme.muted)
                                    .text_color(theme.foreground)
                                    .font_family(theme.mono_font_family.clone())
                                    .text_size(theme.mono_font_size)
                                    .child(div().flex_1().truncate().child(d.fix))
                                    .child(
                                        Clipboard::new(SharedString::from(format!(
                                            "fix-{}",
                                            d.name
                                        )))
                                        .value(d.fix)
                                        .tooltip("Copy"),
                                    ),
                            )
                        }),
                )
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::{Dep, on_path, restart_hints, rows};

    fn names(deps: &[Dep]) -> Vec<(&str, bool)> {
        deps.iter().map(|d| (d.name, d.ok)).collect()
    }

    #[test]
    fn rows_map_lookups_and_exit_status() {
        let all = rows(None, |_| true, "claude", || Ok(String::new()));
        assert!(all.iter().all(|d| d.ok && d.detail.is_empty()));

        let logged_out = rows(None, |_| true, "claude", || Err("gh auth status: …".into()));
        let login = logged_out
            .iter()
            .find(|d| d.name == "GitHub login")
            .unwrap();
        assert_eq!((login.ok, login.fix), (false, "gh auth login"));
        assert_eq!(login.detail, "Not logged in to GitHub");

        let slow = rows(
            None,
            |_| true,
            "claude",
            || Err("gh auth status: timed out after 10s".into()),
        );
        let login = slow.iter().find(|d| d.name == "GitHub login").unwrap();
        assert!(login.detail.contains("timed out"));

        // No gh: no login row, and gh auth status never runs.
        let no_gh = rows(None, |p| p != "gh", "claude", || panic!("ran gh"));
        assert_eq!(
            names(&no_gh),
            [("git", true), ("gh", false), ("claude", true)]
        );

        let fake = rows(
            Some("zsh: timed out"),
            |p| p != "/x/claude",
            "/x/claude",
            || Ok("".into()),
        );
        let shell = fake.iter().find(|d| d.name == "Shell PATH").unwrap();
        assert!(shell.detail.ends_with("zsh: timed out") && shell.fix.is_empty());
        let claude = fake.iter().find(|d| d.name == "claude").unwrap();
        assert_eq!(claude.detail, "Not found: /x/claude");
    }

    #[test]
    fn restart_hint_for_tools_installed_since_launch() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("shark-fresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gh = dir.join("gh");
        std::fs::write(&gh, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let fresh = dir.to_str().unwrap().to_string();
        let row = |deps: &[Dep], n| deps.iter().find(|d| d.name == n).unwrap().clone();

        // gh and claude are missing at launch; gh is on the login shell's PATH now.
        let mut deps = rows(None, |p| p == "git", "claude", || panic!("no gh"));
        restart_hints(&mut deps, "claude", || Ok(fresh.clone()));
        let gh_row = row(&deps, "gh");
        assert_eq!(gh_row.fix, "");
        assert!(gh_row.detail.starts_with("Installed after Shark started"));
        assert_eq!(row(&deps, "claude").detail, "Not found on your PATH");

        // The shell can't be read: keep the fix, and say a restart is needed.
        let mut deps = rows(None, |p| p == "git", "claude", || panic!("no gh"));
        restart_hints(&mut deps, "claude", || Err("timed out".into()));
        let claude = row(&deps, "claude");
        assert!(!claude.fix.is_empty());
        assert_eq!(
            claude.detail,
            "Not found on your PATH. Install it, then restart Shark"
        );

        // Only the login is missing: no login shell.
        let mut deps = rows(None, |_| true, "claude", || Err("x".into()));
        restart_hints(&mut deps, "claude", || panic!("ran the login shell"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn on_path_needs_an_executable() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("shark-deps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        let path = dir.to_str().unwrap();
        let set = |mode| std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(mode));
        set(0o644).unwrap();
        assert!(!on_path("tool", path) && !on_path(tool.to_str().unwrap(), ""));
        set(0o755).unwrap();
        assert!(
            on_path("tool", &format!("/nowhere:{path}")) && on_path(tool.to_str().unwrap(), "")
        );
        assert!(!on_path("tool", "/nowhere"));
        std::fs::remove_dir_all(&dir).ok();
    }
}

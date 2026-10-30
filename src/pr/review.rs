//! Publishing a local review to GitHub: anchors, verification, and the `gh api` calls.
use super::{gh, gh_post, last_line};
use crate::git::cmd_bounded;
use std::path::Path;

/// Posts `body` to PR `number` of the repo at `wt` as a conversation comment (a reviewer
/// note); returns its URL.
pub fn publish_note(wt: &Path, number: u64, body: &str) -> Result<String, String> {
    let n = number.to_string();
    let out = gh_post(wt, &["pr", "comment", &n, "--body", body], 60)?;
    Ok(last_line(&out))
}

/// Where an inline review comment sits in a PR: repo-relative path, last (and first) line,
/// and the side: the base's lines (`left`) or the head's.
pub struct Anchor {
    pub path: String,
    pub line: usize,
    pub start: Option<usize>,
    pub left: bool,
}

/// Verify against fresh GitHub data; any unavailable or changing evidence means excerpt-only.
pub fn verify_original_anchor(
    wt: &Path,
    number: u64,
    revision: &str,
    anchor: &Anchor,
) -> Option<(String, bool)> {
    let read = |endpoint: &str| -> Option<serde_json::Value> {
        serde_json::from_str(&cmd_bounded(wt, &gh(), &["api", endpoint], 30).ok()?).ok()
    };
    let endpoint = format!("repos/{{owner}}/{{repo}}/pulls/{number}");
    let commits = |v: &serde_json::Value| -> Option<(String, String)> {
        let sha = |v: &serde_json::Value| {
            v.as_str()
                .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(String::from)
        };
        Some((sha(&v["base"]["sha"])?, sha(&v["head"]["sha"])?))
    };
    let first = commits(&read(&endpoint)?)?;
    let comparison = read(&format!(
        "repos/{{owner}}/{{repo}}/compare/{}...{}",
        first.0, first.1
    ));
    let valid = comparison
        .as_ref()
        .is_some_and(|comparison| original_anchor_matches(comparison, revision, anchor));
    let unchanged = read(&endpoint).and_then(|v| commits(&v)).as_ref() == Some(&first);
    Some((first.1, valid && unchanged))
}

fn original_anchor_matches(
    comparison: &serde_json::Value,
    revision: &str,
    anchor: &Anchor,
) -> bool {
    if revision.is_empty() || comparison["merge_base_commit"]["sha"].as_str() != Some(revision) {
        return false;
    }
    let Some(file) = comparison["files"].as_array().and_then(|files| {
        files
            .iter()
            .find(|f| f["filename"].as_str() == Some(&anchor.path))
    }) else {
        return false;
    };
    if !matches!(file["status"].as_str(), Some("modified" | "removed"))
        || !file["previous_filename"].is_null()
    {
        return false;
    }
    let Some(patch) = file["patch"].as_str() else {
        return false;
    };
    let valid = || -> Option<bool> {
        let mut lines = patch.lines().peekable();
        let (mut found, mut added, mut removed) = (false, 0u64, 0u64);
        while let Some(line) = lines.next() {
            let (old, old_count, _, new_count) = crate::diff_view::hunk_header(line, 0).ok()?;
            let (mut olds, mut news) = (0, 0);
            while lines.peek().is_some_and(|l| !l.starts_with("@@ ")) {
                match lines.next()?.as_bytes().first() {
                    Some(b' ') => {
                        olds += 1;
                        news += 1;
                    }
                    Some(b'-') => {
                        olds += 1;
                        removed += 1;
                    }
                    Some(b'+') => {
                        news += 1;
                        added += 1;
                    }
                    Some(b'\\') => {}
                    _ => return None,
                }
            }
            if (olds, news) != (old_count, new_count) {
                return None;
            }
            let start = anchor.start.unwrap_or(anchor.line);
            found |= start > 0
                && start <= anchor.line
                && start >= old
                && anchor.line < old.checked_add(old_count)?;
        }
        // Counts also detect patches truncated at a hunk boundary.
        Some(
            found && file["additions"].as_u64()? == added && file["deletions"].as_u64()? == removed,
        )
    };
    valid().unwrap_or(false)
}

/// Posts a Comment review (neither approves nor requests changes) to PR `number` at commit
/// `head`: `body` alone, or with `inline` as a comment on its lines (then `body` is the
/// review's note). Returns the review's URL.
pub fn publish_review(
    wt: &Path,
    number: u64,
    head: &str,
    body: &str,
    inline: Option<(&Anchor, &str)>,
) -> Result<String, String> {
    let args = review_api_args(&number.to_string(), head, body, inline);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    gh_post(wt, &args, 60)
}

/// `gh api` for a review, with one inline comment when given; gh fills in `{owner}/{repo}`.
fn review_api_args(
    n: &str,
    head: &str,
    body: &str,
    inline: Option<(&Anchor, &str)>,
) -> Vec<String> {
    let mut v: Vec<String> = ["api", "--method", "POST"].map(String::from).into();
    v.push(format!("repos/{{owner}}/{{repo}}/pulls/{n}/reviews"));
    let mut field = |flag: &str, kv: String| v.extend([flag.to_string(), kv]);
    field("-f", "event=COMMENT".into());
    field("-f", format!("commit_id={head}"));
    field("-f", format!("body={body}"));
    if let Some((a, text)) = inline {
        let side = if a.left { "LEFT" } else { "RIGHT" };
        field("-f", format!("comments[][path]={}", a.path));
        field("-F", format!("comments[][line]={}", a.line));
        field("-f", format!("comments[][side]={side}"));
        if let Some(s) = a.start {
            field("-F", format!("comments[][start_line]={s}"));
            field("-f", format!("comments[][start_side]={side}"));
        }
        field("-f", format!("comments[][body]={text}"));
    }
    v.extend(["-q".into(), ".html_url".into()]);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "test-support")]
    #[test]
    fn original_anchor_rechecks_moving_github_commits() {
        use serde_json::json;
        use std::{fs, os::unix::fs::PermissionsExt};
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("shark-anchor-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let script = root.join("gh");
        fs::write(
            &script,
            r#"#!/bin/sh
case "$2" in
*/compare/*) cat comparison.json; if [ -e move ]; then cp moved.json pr.json; fi ;;
*/pulls/*) cat pr.json ;;
*) exit 1 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let previous = std::env::var_os("SHARK_GH");
        struct Restore(Option<std::ffi::OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                unsafe {
                    match &self.0 {
                        Some(v) => std::env::set_var("SHARK_GH", v),
                        None => std::env::remove_var("SHARK_GH"),
                    }
                }
            }
        }
        let _restore = Restore(previous);
        unsafe {
            std::env::set_var("SHARK_GH", &script);
        }
        let base = "a".repeat(40);
        let head = "b".repeat(40);
        fs::write(
            root.join("pr.json"),
            json!({"base":{"sha":base},"head":{"sha":head}}).to_string(),
        )
        .unwrap();
        fs::write(
            root.join("moved.json"),
            json!({"base":{"sha":base},"head":{"sha":"c".repeat(40)}}).to_string(),
        )
        .unwrap();
        fs::write(root.join("comparison.json"), json!({"merge_base_commit":{"sha":base},"files":[{"filename":"a.rs","status":"modified","patch":"@@ -1 +1 @@\n-old\n+new","additions":1,"deletions":1}]}).to_string()).unwrap();
        let anchor = Anchor {
            path: "a.rs".into(),
            line: 1,
            start: None,
            left: true,
        };
        assert_eq!(
            verify_original_anchor(&root, 1, &base, &anchor),
            Some((head.clone(), true))
        );
        assert_eq!(
            verify_original_anchor(&root, 1, "legacy", &anchor),
            Some((head.clone(), false))
        );
        fs::write(root.join("move"), "").unwrap();
        assert_eq!(
            verify_original_anchor(&root, 1, &base, &anchor),
            Some((head, false))
        );
        fs::remove_file(root.join("comparison.json")).unwrap();
        assert_eq!(
            verify_original_anchor(&root, 1, &base, &anchor),
            Some(("c".repeat(40), false))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn original_anchors_require_complete_matching_comparison() {
        use serde_json::json;
        let anchor = Anchor {
            path: "a.rs".into(),
            line: 3,
            start: Some(2),
            left: true,
        };
        let comparison = json!({"merge_base_commit":{"sha":"base"}, "files":[{
            "filename":"a.rs", "status":"modified", "patch":"@@ -2,2 +2,2 @@\n context\n-old\n+new", "additions":1, "deletions":1
        }]});
        assert!(original_anchor_matches(&comparison, "base", &anchor));
        for revision in ["", "HEAD", "different-base"] {
            assert!(!original_anchor_matches(&comparison, revision, &anchor));
        }
        for (key, value) in [
            ("status", json!("renamed")),
            ("patch", json!(null)),
            ("patch", json!("@@ -2,2 +2,2 @@\n context")),
            ("additions", json!(2)),
        ] {
            let mut changed = comparison.clone();
            changed["files"][0][key] = value;
            assert!(!original_anchor_matches(&changed, "base", &anchor));
        }
        let outside = Anchor { line: 4, ..anchor };
        assert!(!original_anchor_matches(&comparison, "base", &outside));
    }

    #[test]
    fn inline_review_args() {
        let a = Anchor {
            path: "src/app.rs".into(),
            line: 632,
            start: Some(631),
            left: false,
        };
        let args = review_api_args(
            "128",
            "abc",
            "app.rs:631-632",
            Some((&a, "Keep it aligned.")),
        );
        assert_eq!(
            args.join(" "),
            "api --method POST repos/{owner}/{repo}/pulls/128/reviews -f event=COMMENT \
             -f commit_id=abc -f body=app.rs:631-632 -f comments[][path]=src/app.rs \
             -F comments[][line]=632 -f comments[][side]=RIGHT \
             -F comments[][start_line]=631 -f comments[][start_side]=RIGHT \
             -f comments[][body]=Keep it aligned. -q .html_url"
        );
        let one = Anchor {
            start: None,
            left: true,
            ..a
        };
        let args = review_api_args("7", "h", "note", Some((&one, "x"))).join(" ");
        assert!(
            args.contains("-f body=note") && args.contains("side]=LEFT"),
            "{args}"
        );
        assert!(!args.contains("start_line"));
        // Without an inline comment: the review alone.
        let args = review_api_args("7", "h", "note", None).join(" ");
        assert!(args.ends_with("-f body=note -q .html_url"), "{args}");
    }
}

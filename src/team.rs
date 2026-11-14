//! Teams: the plan a lead writes, read as rows; which rows start next; and the activity log.
//! Nothing here touches the UI, so the rules are tested directly.
use crate::state::{Goal, GoalState, Row, RowState, Team, support_dir};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

/// What `parse` found under `## Tasks`.
#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    pub rows: Vec<Row>,
    /// Lines of the section that are neither a task nor its brief.
    pub unread: Vec<String>,
}

/// What a plan without tasks is told, by the reader and by `check`.
pub const NO_TASKS: &str = "The plan has no tasks under `## Tasks`.";

impl Parsed {
    /// What reading the plan turned up, in sentences: each line that is no task, or that there
    /// are no tasks at all.
    pub fn problems(&self) -> Vec<String> {
        let unread = self.unread.iter().map(|l| format!("Couldn't read: {l}"));
        unread
            .chain((self.rows.is_empty() && self.unread.is_empty()).then(|| NO_TASKS.to_string()))
            .collect()
    }
}

fn heading(line: &str) -> Option<&str> {
    let text = line.trim_start_matches('#');
    (text.len() < line.len() && text.starts_with(' ')).then(|| text.trim())
}

/// `T1 @coder [api, web] after T0, T2: Title` without its bullet.
fn row(line: &str) -> Option<Row> {
    let (head, title) = line.split_once(':')?;
    let title = title.trim();
    let (head, after) = head.split_once(" after ").unwrap_or((head, ""));
    let (head, repos) = match head.split_once('[') {
        Some((head, repos)) => (head, repos.trim().strip_suffix(']')?),
        None => (head, ""),
    };
    let mut words = head.split_whitespace();
    let marks = |w: &str| w.trim_matches(['*', '`']).to_string();
    let key = marks(words.next()?);
    let agent = marks(words.next()?).strip_prefix('@')?.to_string();
    let named = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if words.next().is_some() || title.is_empty() || !named(&key) || agent.is_empty() {
        return None;
    }
    let list = |s: &str| -> Vec<String> {
        s.split([',', ' '])
            .map(marks)
            .filter(|w| !w.is_empty() && w != "and")
            .collect()
    };
    Some(Row {
        key,
        title: title.into(),
        agent,
        repos: list(repos),
        after: list(after),
        ..Default::default()
    })
}

/// Reads the `## Tasks` section of a plan: one unindented bullet per task, indented lines below
/// it its brief. A task that names no repository gets the team's when the team has one.
pub fn parse(plan: &str, team_repos: &[String]) -> Parsed {
    let mut out = Parsed::default();
    let mut tasks = false;
    for line in plan.lines() {
        if let Some(text) = heading(line) {
            tasks = text.eq_ignore_ascii_case("tasks");
            continue;
        }
        if !tasks || line.trim().is_empty() {
            continue;
        }
        let bullet = (line.strip_prefix("- ")).or_else(|| line.strip_prefix("* "));
        match (bullet.and_then(row), out.rows.last_mut()) {
            (Some(mut row), _) => {
                if let ([], [only]) = (row.repos.as_slice(), team_repos) {
                    row.repos = vec![only.clone()];
                }
                out.rows.push(row)
            }
            (None, Some(last)) if line.starts_with([' ', '\t']) => {
                if !last.brief.is_empty() {
                    last.brief.push('\n');
                }
                last.brief.push_str(line.trim());
            }
            _ => out.unread.push(line.trim().to_string()),
        }
    }
    out
}

fn find<'a>(rows: &'a [Row], key: &str) -> Option<&'a Row> {
    rows.iter().find(|r| r.key == key)
}

/// Whether row `a` comes after row `b`, directly or through others.
fn comes_after(rows: &[Row], a: &str, b: &str) -> bool {
    let (mut seen, mut todo) = (HashSet::new(), vec![a]);
    while let Some(key) = todo.pop() {
        for parent in find(rows, key).into_iter().flat_map(|r| &r.after) {
            if parent == b {
                return true;
            }
            if seen.insert(parent.as_str()) {
                todo.push(parent);
            }
        }
    }
    false
}

/// The rows `row` comes after that have a branch in `repo`.
fn parents_in<'a>(rows: &'a [Row], row: &Row, repo: &str) -> Vec<&'a Row> {
    (row.after.iter())
        .filter_map(|k| find(rows, k))
        .filter(|p| p.state != RowState::Skipped && p.repos.iter().any(|r| r == repo))
        .collect()
}

/// The row whose branch `row` starts from in `repo`: the last of the rows it comes after there.
/// A worktree has one base, so those rows form a chain (`check`); `None` starts from the trunk.
pub fn base_row<'a>(rows: &'a [Row], row: &Row, repo: &str) -> Option<&'a Row> {
    let parents = parents_in(rows, row, repo);
    (parents.iter().copied())
        .find(|p| (parents.iter()).all(|q| q.key == p.key || comes_after(rows, &p.key, &q.key)))
}

/// Why a plan can't run as written, in sentences for the user and the lead. Empty when it can.
pub fn check(rows: &[Row], agents: &[String], repos: &[String], limit: usize) -> Vec<String> {
    let mut out = vec![];
    if rows.is_empty() {
        out.push(NO_TASKS.to_string());
    }
    if rows.len() > limit {
        out.push(format!(
            "The plan has {} tasks; the limit is {limit}.",
            rows.len()
        ));
    }
    let mut keys = HashSet::new();
    for r in rows {
        let key = &r.key;
        if !keys.insert(key) {
            out.push(format!("{key} is used twice."));
        }
        if !agents.contains(&r.agent) {
            out.push(format!("{key}: @{} is not an agent of this team.", r.agent));
        }
        if r.repos.is_empty() {
            out.push(format!("{key}: name its repositories in brackets."));
        }
        for repo in r.repos.iter().filter(|repo| !repos.contains(repo)) {
            out.push(format!("{key}: `{repo}` is not a repository of this team."));
        }
        for parent in &r.after {
            if parent == key || find(rows, parent).is_none() {
                out.push(format!("{key}: it can't come after {parent}."));
            }
        }
        if comes_after(rows, key, key) {
            out.push(format!("{key} comes after itself."));
        }
        for repo in &r.repos {
            let parents = parents_in(rows, r, repo);
            if parents.len() > 1 && base_row(rows, r, repo).is_none() {
                let names: Vec<_> = parents.iter().map(|p| p.key.as_str()).collect();
                out.push(format!(
                    "{key} comes after {} in `{repo}`, which are independent; a branch has one base, so order them with `after`.",
                    names.join(" and ")
                ));
            }
        }
    }
    out
}

/// What a row asks for; what happened to it since (its task, state and result) doesn't count.
fn asks(r: &Row) -> impl PartialEq + '_ {
    (&r.key, &r.title, &r.brief, &r.agent, &r.repos, &r.after)
}

/// The approved plan after the user approves `draft`: rows that already started stay as they
/// are, queued ones are replaced by the draft's, and queued ones the draft dropped go. A failed
/// row the lead rewrote is replaced too, and starts over.
pub fn merge(approved: &[Row], draft: Vec<Row>) -> Vec<Row> {
    let kept = |d: &Row| {
        find(approved, &d.key).filter(|r| match r.state {
            RowState::Queued => false,
            RowState::Failed => asks(r) == asks(d),
            _ => true,
        })
    };
    let mut rows: Vec<Row> = (draft.iter())
        .map(|d| kept(d).cloned().unwrap_or_else(|| d.clone()))
        .collect();
    // A started row the draft left out stays where it was, after the nearest row before it
    // that is still there: the plan that runs, resubmitted without what is done, is unchanged.
    for (i, r) in approved.iter().enumerate() {
        if r.state == RowState::Queued || find(&draft, &r.key).is_some() {
            continue;
        }
        let before = approved[..i].iter().rev();
        let at = (before.filter_map(|p| rows.iter().position(|x| x.key == p.key))).next();
        rows.insert(at.map_or(0, |at| at + 1), r.clone());
    }
    rows
}

/// The plan as it would run if the user approved now: the approved rows with the draft, when
/// one waits, merged in.
pub fn proposed(goal: &Goal) -> Vec<Row> {
    match goal.draft.is_empty() {
        true => goal.rows.clone(),
        false => merge(&goal.rows, goal.draft.clone()),
    }
}

/// Whether two plans ask for the same work in the same order.
pub fn same_plan(a: &[Row], b: &[Row]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| asks(a) == asks(b))
}

/// Rows in an order where each comes after the rows it depends on; a cycle keeps plan order.
pub fn in_order(rows: &[Row]) -> Vec<&Row> {
    let mut out: Vec<&Row> = vec![];
    while out.len() < rows.len() {
        let placed = |key: &String| out.iter().any(|r| &r.key == key) || find(rows, key).is_none();
        let next = (rows.iter())
            .find(|r| !out.iter().any(|o| o.key == r.key) && r.after.iter().all(placed))
            .or_else(|| rows.iter().find(|r| !out.iter().any(|o| o.key == r.key)));
        out.extend(next);
    }
    out
}

/// A goal's agent time limit in seconds.
pub fn time_limit(goal: &Goal, team: &Team) -> u64 {
    u64::from(team.limits.minutes + goal.extra_minutes) * 60
}

/// Finished tasks of those still in the plan.
pub fn progress(goal: &Goal) -> (usize, usize) {
    let rows = || goal.rows.iter().filter(|r| r.state != RowState::Skipped);
    (
        rows().filter(|r| r.state == RowState::Finished).count(),
        rows().count(),
    )
}

/// Every task of a plan that has any is finished or skipped.
pub fn all_settled(goal: &Goal) -> bool {
    !goal.rows.is_empty() && goal.rows.iter().all(|r| r.state.settled())
}

/// Why a queued row whose turn it is doesn't start.
#[derive(Clone, Debug, PartialEq)]
pub enum Hold {
    /// A row it comes after isn't finished.
    After(String),
    /// A row it comes after waits for the user's review (`Team::approve_tasks`).
    Review(String),
    AgentPaused,
    /// Its harness has less allowance left than the floor.
    Allowance(u8),
    /// The allowance couldn't be read.
    Unknown,
}

/// The row `row` comes after that holds it: one that isn't finished, or under
/// `Team::approve_tasks` one the user hasn't reviewed.
pub fn parent_hold(goal: &Goal, team: &Team, row: &Row) -> Option<Hold> {
    (row.after.iter()).find_map(|key| match find(&goal.rows, key) {
        Some(p) if !p.state.settled() => Some(Hold::After(key.clone())),
        Some(p) if team.approve_tasks && p.state == RowState::Finished && !p.reviewed => {
            Some(Hold::Review(key.clone()))
        }
        _ => None,
    })
}

/// What keeps `agent` from taking a turn in `team`: its pause there, or the allowance of its
/// harness. A floor of zero asks for no allowance check, so a provider that reports none runs.
pub fn agent_hold(
    team: &Team,
    agent: &str,
    allowance: &impl Fn(&str) -> Option<u8>,
) -> Option<Hold> {
    if team.paused_agents.iter().any(|a| a == agent) {
        return Some(Hold::AgentPaused);
    }
    if team.limits.floor == 0 {
        return None;
    }
    match allowance(agent) {
        None => Some(Hold::Unknown),
        Some(left) if left < team.limits.floor => Some(Hold::Allowance(left)),
        Some(_) => None,
    }
}

/// What keeps a queued `row` from starting, the team's and goal's own pause and limits aside.
pub fn hold(
    goal: &Goal,
    team: &Team,
    row: &Row,
    allowance: &impl Fn(&str) -> Option<u8>,
) -> Option<Hold> {
    parent_hold(goal, team, row).or_else(|| agent_hold(team, &row.agent, allowance))
}

/// The keys of the rows to start now, in plan order: the goal runs, the team isn't paused, time
/// is left, and with `running` chats of the team in a turn there is room under the limit.
pub fn next_starts(
    goal: &Goal,
    team: &Team,
    running: usize,
    allowance: impl Fn(&str) -> Option<u8>,
) -> Vec<String> {
    if team.paused || goal.state != GoalState::Running || goal.secs >= time_limit(goal, team) {
        return vec![];
    }
    let room = usize::from(team.limits.running).saturating_sub(running);
    (goal.rows.iter())
        .filter(|r| r.state == RowState::Queued && hold(goal, team, r, &allowance).is_none())
        .take(room)
        .map(|r| r.key.clone())
        .collect()
}

/// What the lead is told about its team with its next message: who is on it, where they work,
/// and the format Shark reads the plan's tasks from.
pub fn lead_note(goal: &Goal, repos: &[&str]) -> String {
    let agents: String = (goal.roster.iter().skip(1))
        .map(|a| {
            let about: String = a.instructions.chars().take(300).collect();
            format!("- @{} ({}): {}\n", a.slug, a.name, about.replace('\n', " "))
        })
        .collect();
    format!(
        "[shark] You lead a team. You plan and summarise; you never edit code, and you stay in plan mode. \
Read the repositories, then submit a plan. After the user approves it, Shark starts each task as its own \
task on its own branch with the agent you named, and tells that agent only its own brief and the result \
of the tasks it comes after.\n\n\
Agents:\n{agents}\n\
Repositories: {}\n\
At most {} tasks.\n\n\
End the plan with a `## Tasks` section, one line per task: key, `@agent`, repositories in brackets, \
optional `after` keys, a colon, the title. Indented lines below a task are its brief: write there \
everything its agent needs, because it sees nothing else of this plan.\n\n\
Give each task its own files. When two tasks touch the same files, order them with `after`: a task \
that comes after another in the same repository starts from its branch. A task that comes after several \
tasks in one repository needs them in one chain, each after the previous. Nothing follows the section:\n\n\
## Tasks\n\
- T1 @agent [repo]: Title of the first task\n\
\x20\x20What to do and what to leave alone.\n\
- T2 @agent [repo] after T1: Title of a task that builds on T1",
        repos.join(", "),
        goal.max_tasks,
    )
}

/// One line of what happened in a team.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Activity {
    /// Unix milliseconds.
    pub at: u64,
    /// `You`, `Shark` or an agent's name.
    pub who: String,
    pub what: String,
    /// `Task::name` of the goal.
    #[serde(default)]
    pub goal: String,
    /// The row's key; empty for the goal itself.
    #[serde(default)]
    pub key: String,
}

/// The team's folder, for a slug `create_team` could have made: a hand-edited `..` or empty
/// one names somebody else's folder.
fn team_dir(team: &str) -> Option<PathBuf> {
    let safe = !team.is_empty() && team.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    safe.then(|| support_dir().join("teams").join(team))
}

fn activity_path(team: &str) -> Option<PathBuf> {
    Some(team_dir(team)?.join("activity.jsonl"))
}

/// Appends `event` to the team's log. Append only; a failed write loses that line and nothing else.
pub fn log(team: &str, event: &Activity) {
    use std::io::Write;
    let (Some(path), Ok(mut line)) = (activity_path(team), serde_json::to_string(event)) else {
        return;
    };
    line.push('\n');
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path);
    if let Ok(mut file) = file {
        let _ = file.write_all(line.as_bytes());
    }
}

/// The team's log, oldest first; unreadable lines are skipped.
pub fn activity(team: &str) -> Vec<Activity> {
    (activity_path(team).and_then(|p| std::fs::read_to_string(p).ok()))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// With a deleted team.
pub fn forget(team: &str) {
    if let Some(dir) = team_dir(team) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Limits, Why};

    const PLAN: &str = "# Paginate the Saved list\n\nProse the lead wrote.\n\n## Tasks\n\
- T1 @coder [api]: Add cursor pagination to GET /saved\n\
\x20\x20Keep the existing page-size parameter working.\n\
\x20\x20- and the tests\n\
- T2 @coder [api] after T1: Index saved_items: by user and created_at\n\
* **T3** `@designer` [web-main, api] after T1 and T2: Paging states\n\n\
## Notes\n- not a task\n";

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn rows(spec: &[(&str, &str, &[&str], &[&str])]) -> Vec<Row> {
        (spec.iter())
            .map(|(key, agent, repos, after)| Row {
                key: key.to_string(),
                title: format!("Title {key}"),
                agent: agent.to_string(),
                repos: s(repos),
                after: s(after),
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn parses_a_valid_plan() {
        let p = parse(PLAN, &[]);
        assert_eq!(p.unread, Vec::<String>::new());
        let keys: Vec<_> = p.rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["T1", "T2", "T3"]);
        assert_eq!(p.rows[0].agent, "coder");
        assert_eq!(p.rows[0].repos, ["api"]);
        assert_eq!(
            p.rows[0].brief,
            "Keep the existing page-size parameter working.\n- and the tests"
        );
        assert_eq!(p.rows[1].after, ["T1"]);
        assert_eq!(p.rows[1].title, "Index saved_items: by user and created_at");
        assert_eq!(p.rows[2].agent, "designer");
        assert_eq!(p.rows[2].repos, ["web-main", "api"]);
        assert_eq!(p.rows[2].after, ["T1", "T2"]);
        let agents = s(&["coder", "designer"]);
        assert_eq!(
            check(&p.rows, &agents, &s(&["api", "web-main"]), 8),
            Vec::<String>::new()
        );
    }

    #[test]
    fn unreadable_lines_are_listed_and_a_single_repo_is_implied() {
        let p = parse(
            "## Tasks\n  stray brief\nT1 @a: no bullet\n- T1 @a: Fine\n- T2 a [x]: no at\n- T3 @a [x: open\n- T4 @a extra [x]: words\n- T5 @a [x]:\n",
            &s(&["only"]),
        );
        assert_eq!(p.rows.len(), 1);
        assert_eq!(p.rows[0].repos, ["only"], "the team's one repository");
        assert_eq!(
            p.unread,
            [
                "stray brief",
                "T1 @a: no bullet",
                "- T2 a [x]: no at",
                "- T3 @a [x: open",
                "- T4 @a extra [x]: words",
                "- T5 @a [x]:"
            ]
        );
        assert_eq!(parse("No tasks here", &[]), Parsed::default());
        assert_eq!(
            parse(
                "## Tasks\n- T1 @a: Two repos, none named\n",
                &s(&["x", "y"])
            )
            .rows[0]
                .repos,
            Vec::<String>::new()
        );
    }

    #[test]
    fn check_reports_agents_repos_cycles_and_the_limit() {
        let (agents, repos) = (s(&["a"]), s(&["x", "y"]));
        let has = |rows: &[Row], limit, needle: &str| {
            let problems = check(rows, &agents, &repos, limit);
            assert!(
                problems.iter().any(|p| p.contains(needle)),
                "{needle:?} not in {problems:?}"
            );
        };
        has(&[], 8, "no tasks");
        has(
            &rows(&[("T1", "ghost", &["x"], &[])]),
            8,
            "@ghost is not an agent",
        );
        has(
            &rows(&[("T1", "a", &["z"], &[])]),
            8,
            "`z` is not a repository",
        );
        has(&rows(&[("T1", "a", &[], &[])]), 8, "name its repositories");
        has(
            &rows(&[("T1", "a", &["x"], &["T9"])]),
            8,
            "can't come after T9",
        );
        has(
            &rows(&[("T1", "a", &["x"], &["T1"])]),
            8,
            "can't come after T1",
        );
        has(
            &rows(&[("T1", "a", &["x"], &[]), ("T1", "a", &["x"], &[])]),
            8,
            "used twice",
        );
        let cycle = rows(&[
            ("T1", "a", &["x"], &["T3"]),
            ("T2", "a", &["x"], &["T1"]),
            ("T3", "a", &["x"], &["T2"]),
        ]);
        has(&cycle, 8, "T2 comes after itself");
        let three = rows(&[
            ("T1", "a", &["x"], &[]),
            ("T2", "a", &["x"], &[]),
            ("T3", "a", &["x"], &[]),
        ]);
        has(&three, 2, "3 tasks; the limit is 2");
        assert!(check(&three, &agents, &repos, 3).is_empty());
    }

    #[test]
    fn two_independent_parents_in_one_repository_are_rejected() {
        let (agents, repos) = (s(&["a"]), s(&["x", "y"]));
        let fork = rows(&[
            ("T1", "a", &["x"], &[]),
            ("T2", "a", &["x"], &[]),
            ("T3", "a", &["x"], &["T1", "T2"]),
        ]);
        let problems = check(&fork, &agents, &repos, 8);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("T3 comes after T1 and T2 in `x`"));
        assert_eq!(base_row(&fork, &fork[2], "x"), None);
        // One chain, each after the previous: it starts from the last.
        let chain = rows(&[
            ("T1", "a", &["x"], &[]),
            ("T2", "a", &["x"], &["T1"]),
            ("T3", "a", &["x"], &["T1", "T2"]),
        ]);
        assert!(check(&chain, &agents, &repos, 8).is_empty());
        assert_eq!(base_row(&chain, &chain[2], "x").unwrap().key, "T2");
        // Parents in different repositories don't share a base.
        let apart = rows(&[
            ("T1", "a", &["x"], &[]),
            ("T2", "a", &["y"], &[]),
            ("T3", "a", &["x", "y"], &["T1", "T2"]),
        ]);
        assert!(check(&apart, &agents, &repos, 8).is_empty());
        assert_eq!(base_row(&apart, &apart[2], "x").unwrap().key, "T1");
        assert_eq!(base_row(&apart, &apart[2], "y").unwrap().key, "T2");
        assert_eq!(base_row(&apart, &apart[0], "x"), None, "the trunk");
    }

    fn team() -> Team {
        Team {
            slug: "t".into(),
            agents: s(&["a", "b"]),
            limits: Limits {
                running: 2,
                floor: 20,
                minutes: 60,
                tasks: 8,
            },
            ..Default::default()
        }
    }

    fn goal(rows: Vec<Row>) -> Goal {
        Goal {
            rows,
            state: GoalState::Running,
            ..Default::default()
        }
    }

    const FULL: fn(&str) -> Option<u8> = |_| Some(100);

    #[test]
    fn next_starts_follows_order_and_the_running_limit() {
        let mut g = goal(rows(&[
            ("T1", "a", &["x"], &[]),
            ("T2", "a", &["x"], &["T1"]),
            ("T3", "b", &["x"], &[]),
            ("T4", "b", &["x"], &[]),
        ]));
        let t = team();
        assert_eq!(next_starts(&g, &t, 0, FULL), ["T1", "T3"], "two at once");
        assert_eq!(next_starts(&g, &t, 1, FULL), ["T1"]);
        assert_eq!(next_starts(&g, &t, 2, FULL), Vec::<String>::new());
        assert_eq!(next_starts(&g, &t, 5, FULL), Vec::<String>::new());
        g.rows[0].state = RowState::Started;
        assert_eq!(next_starts(&g, &t, 1, FULL), ["T3"], "T2 waits for T1");
        g.rows[0].state = RowState::Committing;
        assert_eq!(
            next_starts(&g, &t, 1, FULL),
            ["T3"],
            "a commit turn isn't finished"
        );
        g.rows[0].state = RowState::Failed;
        assert_eq!(
            next_starts(&g, &t, 0, FULL),
            ["T3", "T4"],
            "nothing after a failure"
        );
        g.rows[0].state = RowState::Finished;
        assert_eq!(next_starts(&g, &t, 0, FULL), ["T2", "T3"]);
        g.rows[0].state = RowState::Skipped;
        assert_eq!(
            next_starts(&g, &t, 0, FULL),
            ["T2", "T3"],
            "a removed parent doesn't hold"
        );
    }

    #[test]
    fn next_starts_respects_pause_time_floor_and_review() {
        let g = goal(rows(&[("T1", "a", &["x"], &[]), ("T2", "b", &["x"], &[])]));
        let t = team();
        let paused = Team {
            paused: true,
            ..t.clone()
        };
        assert!(next_starts(&g, &paused, 0, FULL).is_empty());
        for state in [
            GoalState::Planning,
            GoalState::Review,
            GoalState::Paused(Why::You),
            GoalState::Paused(Why::Time),
            GoalState::Ready,
            GoalState::Done,
            GoalState::Stopped,
        ] {
            let g = Goal { state, ..g.clone() };
            assert!(next_starts(&g, &t, 0, FULL).is_empty(), "{state:?}");
        }
        let spent = Goal {
            secs: 3600,
            ..g.clone()
        };
        assert!(next_starts(&spent, &t, 0, FULL).is_empty(), "time used");
        let extended = Goal {
            extra_minutes: 60,
            ..spent
        };
        assert_eq!(next_starts(&extended, &t, 0, FULL).len(), 2, "Add 1 h");
        // The floor is per agent: each one's harness has its own allowance.
        let low = |agent: &str| Some(if agent == "a" { 19 } else { 20 });
        assert_eq!(next_starts(&g, &t, 0, low), ["T2"]);
        assert_eq!(hold(&g, &t, &g.rows[0], &low), Some(Hold::Allowance(19)));
        let missing = |agent: &str| (agent == "b").then_some(100);
        assert_eq!(next_starts(&g, &t, 0, missing), ["T2"], "unknown holds");
        assert_eq!(hold(&g, &t, &g.rows[0], &missing), Some(Hold::Unknown));
        let no_floor = Team {
            limits: Limits {
                floor: 0,
                ..t.limits.clone()
            },
            ..t.clone()
        };
        assert_eq!(next_starts(&g, &no_floor, 0, |_| None).len(), 2, "no check");
        let one = Team {
            paused_agents: s(&["b"]),
            ..t.clone()
        };
        assert_eq!(next_starts(&g, &one, 0, FULL), ["T1"]);
        // Stricter approval: a finished parent holds its dependents until reviewed.
        let strict = Team {
            approve_tasks: true,
            ..t.clone()
        };
        let mut g = goal(rows(&[
            ("T1", "a", &["x"], &[]),
            ("T2", "a", &["x"], &["T1"]),
        ]));
        g.rows[0].state = RowState::Finished;
        assert!(next_starts(&g, &strict, 0, FULL).is_empty());
        assert_eq!(
            hold(&g, &strict, &g.rows[1], &FULL),
            Some(Hold::Review("T1".into()))
        );
        assert_eq!(next_starts(&g, &t, 0, FULL), ["T2"]);
        g.rows[0].reviewed = true;
        assert_eq!(next_starts(&g, &strict, 0, FULL), ["T2"]);
    }

    #[test]
    fn merge_keeps_started_rows_and_replaces_queued_ones() {
        let mut approved = rows(&[
            ("T1", "a", &["x"], &[]),
            ("T2", "a", &["x"], &[]),
            ("T3", "a", &["x"], &[]),
        ]);
        approved[0].state = RowState::Finished;
        approved[0].result = Some("done".into());
        approved[2].state = RowState::Started;
        let draft = rows(&[("T1", "b", &["y"], &[]), ("T4", "b", &["x"], &["T1"])]);
        let merged = merge(&approved, draft);
        let keys: Vec<_> = merged.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            ["T1", "T3", "T4"],
            "queued T2 was dropped by the lead; started T3 stays after T1"
        );
        assert_eq!(merged[1].state, RowState::Started);
        assert_eq!(merged[0].agent, "a", "a started row isn't redefined");
        assert_eq!(merged[0].result.as_deref(), Some("done"));
        assert_eq!(merged[2].agent, "b");
        assert_eq!(merge(&[], approved.clone()), approved);
        // A draft that leaves a started row out keeps it where it was: resubmitting the plan
        // that runs, minus what is done, is the same plan.
        let mut running = rows(&[
            ("T1", "a", &["x"], &[]),
            ("T2", "a", &["x"], &[]),
            ("T3", "a", &["x"], &[]),
        ]);
        running[1].state = RowState::Finished;
        let rest = vec![running[0].clone(), running[2].clone()];
        assert!(same_plan(&merge(&running, rest), &running));
        // A failed row the lead rewrote starts over; one it left as it was stays failed.
        approved[1].state = RowState::Failed;
        approved[1].task = Some(Default::default());
        let mut draft = approved.clone();
        draft[1].state = RowState::Queued;
        draft[1].task = None;
        assert_eq!(merge(&approved, draft.clone())[1].state, RowState::Failed);
        draft[1].brief = "Another way".into();
        let merged = merge(&approved, draft);
        assert_eq!(
            (merged[1].state, merged[1].brief.as_str(), &merged[1].task),
            (RowState::Queued, "Another way", &None)
        );
    }

    #[test]
    fn same_plan_ignores_progress_and_in_order_puts_parents_first() {
        let a = rows(&[
            ("T2", "a", &["x"], &["T1"]),
            ("T3", "a", &["x"], &["T2"]),
            ("T1", "a", &["x"], &[]),
        ]);
        let mut b = a.clone();
        b[0].state = RowState::Finished;
        b[0].result = Some("r".into());
        assert!(same_plan(&a, &b));
        b[0].agent = "b".into();
        assert!(!same_plan(&a, &b));
        assert!(!same_plan(&a, &a[..2]));
        let keys = |rows: &[Row]| -> Vec<String> {
            in_order(rows).iter().map(|r| r.key.clone()).collect()
        };
        assert_eq!(keys(&a), ["T1", "T2", "T3"]);
        let cycle = rows(&[("T1", "a", &["x"], &["T2"]), ("T2", "a", &["x"], &["T1"])]);
        assert_eq!(keys(&cycle), ["T1", "T2"], "a cycle still ends");
    }

    #[test]
    fn progress_and_settled_ignore_skipped_rows() {
        let mut g = goal(rows(&[
            ("T1", "a", &["x"], &[]),
            ("T2", "a", &["x"], &[]),
            ("T3", "a", &["x"], &[]),
        ]));
        assert_eq!(progress(&g), (0, 3));
        assert!(!all_settled(&g));
        g.rows[0].state = RowState::Finished;
        g.rows[1].state = RowState::Skipped;
        assert_eq!(progress(&g), (1, 2));
        assert!(!all_settled(&g));
        g.rows[2].state = RowState::Finished;
        assert!(all_settled(&g));
        assert!(!all_settled(&goal(vec![])), "an empty plan isn't finished");
    }

    #[test]
    fn the_lead_note_lists_the_team_and_its_own_format_parses() {
        use crate::state::AgentLink;
        let link = |slug: &str, instructions: &str| AgentLink {
            slug: slug.into(),
            name: slug.to_uppercase(),
            instructions: instructions.into(),
            ..Default::default()
        };
        let g = Goal {
            roster: vec![link("lead", "Plan."), link("coder", &"x".repeat(400))],
            max_tasks: 5,
            ..Default::default()
        };
        let note = lead_note(&g, &["api", "web"]);
        assert!(note.contains("- @coder (CODER): "));
        assert!(!note.contains("@lead"), "the lead takes no task");
        assert!(note.contains(&"x".repeat(300)) && !note.contains(&"x".repeat(301)));
        assert!(note.contains("Repositories: api, web") && note.contains("At most 5 tasks"));
        let example = parse(&note, &[]);
        assert_eq!(example.unread, Vec::<String>::new());
        assert_eq!(example.rows.len(), 2);
        assert_eq!(example.rows[1].after, ["T1"]);
    }

    #[test]
    fn activity_appends_and_skips_bad_lines() {
        // The UI tests wipe the test home this writes under.
        #[cfg(feature = "test-support")]
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let team = format!("activity-test-{}", std::process::id());
        forget(&team);
        assert!(activity(&team).is_empty());
        let ev = |what: &str| Activity {
            at: 1,
            who: "Shark".into(),
            what: what.into(),
            goal: "g".into(),
            key: "T1".into(),
        };
        log(&team, &ev("Started"));
        let path = activity_path(&team).unwrap();
        let line = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{line}not json\n")).unwrap();
        // A slug nobody could have created names no folder.
        for bad in ["", "..", "a/b"] {
            assert_eq!(team_dir(bad), None, "{bad:?}");
            log(bad, &ev("x"));
            forget(bad);
        }
        assert!(support_dir().join("teams").exists());
        log(&team, &ev("Finished"));
        let what: Vec<_> = activity(&team).into_iter().map(|a| a.what).collect();
        assert_eq!(what, ["Started", "Finished"]);
        forget(&team);
        assert!(activity(&team).is_empty());
    }
}

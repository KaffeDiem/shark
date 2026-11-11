//! Memory-only facts shared by task rows and Quick Look.
use super::*;
use home::Bucket;

pub(super) struct TaskFacts<'a> {
    pub bucket: Bucket,
    pub line: String,
    pub warning: bool,
    pub failed: bool,
    pub running: usize,
    pub time: String,
    pub repos: Option<&'a [crate::sync::Repo]>,
}

impl Shark {
    pub(super) fn task_waiting(&self, task: &Task, cx: &App) -> Option<&'static str> {
        task.chats
            .iter()
            .filter(|meta| {
                self.chats.get(&meta.id).is_none_or(|chat| {
                    let chat = chat.read(cx);
                    !chat.busy && !chat.steered
                })
            })
            .filter_map(|meta| self.home.waiting.get(&meta.id).copied())
            .min_by_key(|label| *label != crate::transcript::PLAN_TO_APPROVE)
    }

    pub(super) fn task_facts<'a>(&'a self, task: &Task, cx: &'a App) -> TaskFacts<'a> {
        let chats: Vec<_> = task
            .chats
            .iter()
            .filter_map(|m| self.chats.get(&m.id))
            .map(|c| c.read(cx))
            .collect();
        let running = self.running_chats(&task.name, cx);
        let attn = self.attention_of(&task.name, cx);
        let waiting = self.task_waiting(task, cx).filter(|_| !task.backlog);
        let (bucket, _) = home::bucket(running, &attn, waiting);
        let needs = match &attn {
            Attn::Needs(s) => Some(s.as_str()),
            _ => waiting,
        };
        let failed = chats.iter().rev().find_map(|c| c.failed.as_deref());
        let agent = |meta: &ChatMeta| {
            meta.agent
                .as_deref()
                .and_then(|slug| self.agent(slug))
                .map_or("Agent", |a| a.name.as_str())
                .to_string()
        };
        let repos = self.overview_repos(&task.name, cx);
        let repo_line = || {
            let Some(first) = task.repos.first() else {
                return "No repository".into();
            };
            let name = if task.repos.len() > 1 {
                format!("{} +{}", first.name, task.repos.len() - 1)
            } else {
                first.name.clone()
            };
            let fact = repos
                .into_iter()
                .flatten()
                .filter_map(|r| {
                    let l = r.local.as_ref().ok()?;
                    if let Some(op) = &l.op {
                        return Some((0, format!("{op:?} in progress")));
                    }
                    if let Some((n, _)) = l.remote.filter(|(n, _)| *n > 0) {
                        return Some((1, format!("{n} to push")));
                    }
                    (l.base.1 > 0).then(|| {
                        (
                            2,
                            format!(
                                "{} behind {}",
                                l.base.1,
                                r.base.trim_start_matches("origin/")
                            ),
                        )
                    })
                })
                .min_by_key(|(priority, _)| *priority);
            fact.map_or(name.clone(), |(_, fact)| format!("{name} · {fact}"))
        };
        let line = if let Some(needs) = needs {
            needs.into()
        } else if let Some(repo) = task.attaching.first() {
            format!("Setting up {}…", repo.repo)
        } else if running == 1 {
            chats
                .iter()
                .find(|c| c.busy)
                .map(|c| {
                    format!(
                        "{} · {}",
                        agent(&c.meta),
                        c.activity().unwrap_or_else(|| "Thinking".into())
                    )
                })
                .unwrap_or_else(|| "1 chat running".into())
        } else if running > 1 {
            format!("{running} chats running")
        } else if let Some(reason) = failed {
            format!("Turn failed · {reason}")
        } else if matches!(attn, Attn::Unread) {
            format!(
                "{} finished",
                task.chats
                    .iter()
                    .find(|c| self.unread.contains(&c.id))
                    .map(&agent)
                    .unwrap_or_else(|| "Agent".into())
            )
        } else if task.backlog {
            "Backlog".into()
        } else if let Some(issue) = task.primary_issue() {
            format!("{} · {}", issue.identifier, issue.status)
        } else {
            repo_line()
        };
        let elapsed = chats
            .iter()
            .filter(|c| c.busy)
            .filter_map(|c| c.started)
            .min()
            .map(|t| t.elapsed());
        let age = elapsed.or_else(|| {
            self.home
                .activity
                .get(&task.name)
                .and_then(|t| t.elapsed().ok())
        });
        TaskFacts {
            bucket: if task.backlog {
                Bucket::Backlog
            } else {
                bucket
            },
            line,
            warning: needs.is_some(),
            failed: needs.is_none()
                && task.attaching.is_empty()
                && running == 0
                && failed.is_some(),
            running,
            time: age.map(crate::sync::compact_age).unwrap_or_default(),
            repos,
        }
    }
}

//! Shared, connection-scoped issue sources and attachment reads for both composers.
use super::home::Destination;
use super::task_draft::{BranchSpec, DraftRepo};
use super::*;
use crate::complete::Reference;
use crate::linear::{self, Hit, Target};
use std::collections::HashSet;
use std::time::Duration;

#[derive(Default)]
pub(super) struct ReferenceCompletion {
    epoch: Option<u64>,
    hits: Vec<Hit>,
    searched: HashSet<String>,
    generation: u64,
    next_paste: u64,
    pastes: Vec<PastedReference>,
}

struct PastedReference {
    id: u64,
    destination: Destination,
    text: String,
    repos: Vec<DraftRepo>,
    repos_edited: bool,
    pending: bool,
    applied: Option<PastedMutation>,
}

enum PastedMutation {
    Issue(Snapshot),
    Repo(DraftRepo),
}

impl Shark {
    pub(super) fn reference_hits(&self) -> Vec<Hit> {
        if !self.linear_in_use() {
            return vec![];
        }
        let mut hits = self.reference_completion.hits.clone();
        for snapshot in self
            .state
            .tasks
            .iter()
            .flat_map(|t| &t.issues)
            .chain(self.home.draft.issues.iter())
            .chain(self.home.agent_draft.issues.iter())
            .filter(|snapshot| Some(snapshot.org.as_str()) == self.linear_org_id())
        {
            if !hits.iter().any(|hit| hit.id == snapshot.id) {
                hits.push(snapshot.into());
            }
        }
        hits
    }

    pub(super) fn sync_reference_completion(&mut self, agent: bool, cx: &mut Context<Self>) {
        let epoch = self.linear_connection_epoch();
        if self.reference_completion.epoch != epoch {
            self.reference_completion.epoch = epoch;
            self.reference_completion.hits.clear();
            self.reference_completion.searched.clear();
            self.reference_completion.generation += 1;
        }
        let hits = self.reference_hits();
        let composer = if agent {
            &self.home.agent_composer
        } else {
            &self.home.composer
        };
        composer.update(cx, |c, cx| c.set_issues(hits.clone(), cx));
        for chat in self.chats.values() {
            chat.update(cx, |chat, cx| chat.set_completion_issues(hits.clone(), cx));
        }
    }

    pub(super) fn search_reference_issues(&mut self, query: String, cx: &mut Context<Self>) {
        self.sync_reference_completion(self.draft_target(), cx);
        self.reference_completion.generation += 1;
        let generation = self.reference_completion.generation;
        if !self.linear_in_use()
            || self.linear_org_id().is_none()
            || self.linear_needs_attention().is_some()
        {
            return;
        }
        let query = query.trim().to_string();
        if self.reference_completion.searched.contains(&query) {
            return;
        }
        let epoch = self.linear_connection_epoch();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let cache_key = query.clone();
            let task = this
                .update(cx, |s, cx| {
                    if s.reference_completion.generation != generation
                        || s.linear_connection_epoch() != epoch
                    {
                        return None;
                    }
                    Some(s.linear_request(
                        move |http, credential| {
                            if query.is_empty() {
                                linear::lists(http, credential).map(|lists| {
                                    lists
                                        .assigned
                                        .into_iter()
                                        .chain(lists.recent)
                                        .collect::<Vec<_>>()
                                })
                            } else {
                                linear::find(http, credential, &query).map(|page| page.hits)
                            }
                        },
                        cx,
                    ))
                })
                .ok()
                .flatten();
            let Some(task) = task else {
                return;
            };
            let result = task.await;
            this.update(cx, |s, cx| {
                if s.linear_connection_epoch() != epoch {
                    return;
                }
                if let Ok(hits) = result {
                    s.reference_completion.searched.insert(cache_key);
                    for hit in hits {
                        s.reference_completion
                            .hits
                            .retain(|known| known.id != hit.id);
                        s.reference_completion.hits.push(hit);
                    }
                    s.sync_reference_completion(s.draft_target(), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn attach_draft_reference(
        &mut self,
        agent: bool,
        reference: Reference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.draft(agent).starting {
            return;
        }
        match reference {
            Reference::Pr { repo, pr, label } => {
                let Some(repo) = self
                    .state
                    .repos
                    .iter()
                    .find(|known| known.name == repo)
                    .cloned()
                else {
                    return;
                };
                let draft = self.draft_mut(agent);
                if let Some(existing) = draft
                    .repos
                    .iter_mut()
                    .find(|existing| existing.repo.name == repo.name)
                {
                    existing.branch = BranchSpec::Pr(pr);
                    existing.pr_label = Some(label);
                } else {
                    draft.repos.push(DraftRepo {
                        repo,
                        branch: BranchSpec::Pr(pr),
                        pr_label: Some(label),
                    });
                }
                draft.repos_edited = true;
                self.focus_draft(agent, window, cx);
            }
            Reference::Issue(hit) => {
                let destination = if agent {
                    let Some(slug) = self.draft_slug(true) else {
                        return;
                    };
                    Destination::Agent(slug.to_string())
                } else {
                    Destination::Home
                };
                self.draft_mut(agent).staging += 1;
                let org = self.linear_org_id().map(str::to_string);
                let epoch = self.linear_connection_epoch();
                let target = linear::parse_issue(&hit.url).unwrap_or(Target {
                    id: hit.identifier,
                    org: None,
                });
                let task = self.linear_request(
                    move |http, credential| linear::get_issue(http, credential, &target),
                    cx,
                );
                cx.spawn_in(window, async move |this, cx| {
                    let result = task.await;
                    this.update_in(cx, |s, window, cx| {
                        if let Some(draft) = s.draft_at_mut(&destination) {
                            draft.staging = draft.staging.saturating_sub(1);
                        }
                        if s.linear_connection_epoch() != epoch
                            || s.linear_org_id() != org.as_deref()
                        {
                            return;
                        }
                        match result {
                            Ok(snapshot) if Some(snapshot.org.as_str()) == org.as_deref() => {
                                s.add_draft_issues_to(&destination, vec![snapshot], window, cx)
                            }
                            Ok(_) => notify_err(
                                window,
                                cx,
                                "The issue belongs to a different Linear workspace",
                            ),
                            Err(error) => notify_err(window, cx, error.to_string()),
                        }
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    pub(super) fn paste_draft_reference(
        &mut self,
        agent: bool,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.draft(agent).starting || crate::complete::pasted_reference(&text).is_none() {
            return;
        }
        let destination = if agent {
            let Some(slug) = self.draft_slug(true) else {
                return;
            };
            Destination::Agent(slug.to_string())
        } else {
            Destination::Home
        };
        self.reference_completion.next_paste += 1;
        let id = self.reference_completion.next_paste;
        let draft = self.draft(agent);
        let undo = PastedReference {
            id,
            destination: destination.clone(),
            text: text.clone(),
            repos: draft.repos.clone(),
            repos_edited: draft.repos_edited,
            pending: true,
            applied: None,
        };
        self.reference_completion.pastes.push(undo);
        self.draft_mut(agent).staging += 1;
        if let Some(target) = linear::parse_issue(text.trim()) {
            let org = self.linear_org_id().map(str::to_string);
            let epoch = self.linear_connection_epoch();
            let task = self.linear_request(
                move |http, credential| linear::get_issue(http, credential, &target),
                cx,
            );
            cx.spawn_in(window, async move |this, cx| {
                let result = task.await;
                this.update_in(cx, |s, window, cx| {
                    if !s.finish_pasted_reference(id) {
                        return;
                    }
                    if s.linear_connection_epoch() != epoch || s.linear_org_id() != org.as_deref() {
                        s.fail_pasted_reference(
                            id,
                            "The Linear connection changed. Try again.".into(),
                            window,
                            cx,
                        );
                        return;
                    }
                    match result {
                        Ok(snapshot) if Some(snapshot.org.as_str()) == org.as_deref() => {
                            let added = s.draft_at_mut(&destination).is_some_and(|draft| {
                                !draft.issues.iter().any(|issue| {
                                    issue.id == snapshot.id && issue.org == snapshot.org
                                })
                            });
                            if added
                                && let Some(paste) = s
                                    .reference_completion
                                    .pastes
                                    .iter_mut()
                                    .find(|paste| paste.id == id)
                            {
                                paste.applied = Some(PastedMutation::Issue(snapshot.clone()));
                            }
                            s.add_draft_issues_to(&destination, vec![snapshot], window, cx)
                        }
                        Ok(_) => s.fail_pasted_reference(
                            id,
                            "The issue belongs to a different Linear workspace".into(),
                            window,
                            cx,
                        ),
                        Err(error) => s.fail_pasted_reference(id, error.to_string(), window, cx),
                    }
                })
                .ok();
            })
            .detach();
            return;
        }
        let repos = self.state.repos.clone();
        let url = text.trim().to_string();
        let task = cx.background_executor().spawn(async move {
            let parsed = url::Url::parse(&url).map_err(|error| error.to_string())?;
            let parts = parsed
                .path_segments()
                .ok_or("Invalid pull request URL")?
                .collect::<Vec<_>>();
            let slug = format!("{}/{}", parts[0], parts[1]);
            let repo = repos
                .into_iter()
                .find(|repo| {
                    git::git(&repo.path, &["remote", "get-url", "origin"])
                        .ok()
                        .is_some_and(|remote| {
                            remote
                                .trim()
                                .trim_end_matches(".git")
                                .ends_with(&format!("/{slug}"))
                                || remote
                                    .trim()
                                    .trim_end_matches(".git")
                                    .ends_with(&format!(":{slug}"))
                        })
                })
                .ok_or("Add this pull request's repository in Settings first")?;
            let json = git::cmd_bounded(
                &repo.path,
                "gh",
                &[
                    "pr",
                    "view",
                    &url,
                    "--json",
                    "number,url,baseRefName,title,state,isDraft",
                ],
                30,
            )?;
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Pr {
                number: u64,
                url: String,
                base_ref_name: String,
                title: String,
                state: String,
                is_draft: bool,
            }
            let pr: Pr = serde_json::from_str(&json).map_err(|error| error.to_string())?;
            let status = if pr.is_draft {
                " · draft".to_string()
            } else if pr.state != "OPEN" {
                format!(" · {}", pr.state.to_lowercase())
            } else {
                String::new()
            };
            let label = format!("#{} {}{status}", pr.number, pr.title);
            Ok::<_, String>((
                repo,
                label,
                git::PullRequest {
                    number: pr.number,
                    url: pr.url,
                    base: pr.base_ref_name,
                },
            ))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |s, window, cx| {
                if !s.finish_pasted_reference(id) {
                    return;
                }
                match result {
                    Ok((repo, label, pr)) => {
                        let applied_name = repo.name.clone();
                        let Some(draft) =
                            s.draft_at_mut(&destination).filter(|draft| !draft.starting)
                        else {
                            return;
                        };
                        if let Some(existing) = draft
                            .repos
                            .iter_mut()
                            .find(|existing| existing.repo.name == repo.name)
                        {
                            existing.branch = BranchSpec::Pr(pr.clone());
                            existing.pr_label = Some(label.clone());
                        } else {
                            draft.repos.push(DraftRepo {
                                repo,
                                branch: BranchSpec::Pr(pr.clone()),
                                pr_label: Some(label.clone()),
                            });
                        }
                        draft.repos_edited = true;
                        let applied = draft
                            .repos
                            .iter()
                            .find(|existing| existing.repo.name == applied_name)
                            .cloned();
                        if let Some(paste) = s
                            .reference_completion
                            .pastes
                            .iter_mut()
                            .find(|paste| paste.id == id)
                        {
                            paste.applied = applied.map(PastedMutation::Repo);
                        }
                        if s.home.destination == destination {
                            s.focus_draft(agent, window, cx);
                        }
                        cx.notify();
                    }
                    Err(error) => s.fail_pasted_reference(id, error, window, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    fn fail_pasted_reference(
        &mut self,
        id: u64,
        error: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self
            .reference_completion
            .pastes
            .iter()
            .position(|paste| paste.id == id)
        else {
            return;
        };
        let paste = self.reference_completion.pastes.remove(index);
        let current_input = match &paste.destination {
            Destination::Home => Some(self.home.composer.clone()),
            Destination::Agent(slug) if self.home.agent_prompt_for.as_ref() == Some(slug) => {
                Some(self.home.agent_composer.clone())
            }
            Destination::Agent(_) => None,
        };
        if let Some(composer) = current_input {
            composer.update(cx, |composer, cx| {
                composer.restore_failed_reference(&paste.text, window, cx)
            });
        } else {
            let recovery = self.home.agent_composer.update(cx, |composer, _| {
                composer.take_failed_reference(&paste.text)
            });
            if let Some(draft) = self.draft_at_mut(&paste.destination) {
                let content = draft
                    .content
                    .clone()
                    .unwrap_or_else(|| draft.text.clone().into());
                let restored = match recovery {
                    Some((removed, restored)) if content.text().as_ref() == removed => restored,
                    _ if content.text().contains(paste.text.trim()) => content,
                    _ => {
                        let text = format!(
                            "{}{}{}",
                            content.text(),
                            if content.text().is_empty() || content.text().ends_with('\n') {
                                ""
                            } else {
                                "\n"
                            },
                            paste.text
                        );
                        let mut restored = gpui_kit::component::input::InputContent::new(text);
                        for span in content.tokens() {
                            restored = restored
                                .with_token(span.range(), span.token().clone())
                                .expect("appending a new line preserves existing token ranges");
                        }
                        restored
                    }
                };
                draft.text = restored.text().to_string();
                draft.content = Some(restored);
            }
        }
        notify_err(
            window,
            cx,
            format!("Couldn't attach {}: {error}", paste.text),
        );
        cx.notify();
    }

    fn finish_pasted_reference(&mut self, id: u64) -> bool {
        let Some(paste) = self
            .reference_completion
            .pastes
            .iter_mut()
            .find(|paste| paste.id == id)
        else {
            return false;
        };
        paste.pending = false;
        let destination = paste.destination.clone();
        if let Some(draft) = self.draft_at_mut(&destination) {
            draft.staging = draft.staging.saturating_sub(1);
        }
        true
    }

    pub(super) fn undo_draft_reference(
        &mut self,
        agent: bool,
        text: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let destination = if agent {
            let Some(slug) = self.draft_slug(true) else {
                return;
            };
            Destination::Agent(slug.to_string())
        } else {
            Destination::Home
        };
        let Some(index) = self
            .reference_completion
            .pastes
            .iter()
            .rposition(|paste| paste.destination == destination && paste.text == text)
        else {
            return;
        };
        let before = self.reference_completion.pastes.remove(index);
        if let Some(draft) = self
            .draft_at_mut(&destination)
            .filter(|draft| !draft.starting)
        {
            if before.pending {
                draft.staging = draft.staging.saturating_sub(1);
            }
            match before.applied {
                Some(PastedMutation::Issue(issue)) => {
                    draft.issues.retain(|current| current != &issue)
                }
                Some(PastedMutation::Repo(applied)) => {
                    if let Some(index) = draft.repos.iter().position(|current| {
                        current.repo.name == applied.repo.name
                            && current.repo.path == applied.repo.path
                            && current.branch == applied.branch
                            && current.pr_label == applied.pr_label
                    }) {
                        if let Some(previous) = before
                            .repos
                            .iter()
                            .find(|previous| previous.repo.name == applied.repo.name)
                        {
                            draft.repos[index] = previous.clone();
                        } else {
                            draft.repos.remove(index);
                        }
                        if draft.repos.len() == before.repos.len()
                            && draft
                                .repos
                                .iter()
                                .zip(&before.repos)
                                .all(|(current, previous)| {
                                    current.repo.name == previous.repo.name
                                        && current.repo.path == previous.repo.path
                                        && current.branch == previous.branch
                                        && current.pr_label == previous.pr_label
                                })
                        {
                            draft.repos_edited = before.repos_edited;
                        }
                    }
                }
                None => {}
            }
        }
        self.refresh_draft_sources(agent, cx);
        cx.notify();
    }

    pub(super) fn attach_chat_reference_issue(
        &mut self,
        name: String,
        hit: Hit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let org = self.linear_org_id().map(str::to_string);
        let epoch = self.linear_connection_epoch();
        let target = linear::parse_issue(&hit.url).unwrap_or(Target {
            id: hit.identifier,
            org: None,
        });
        let task = self.linear_request(
            move |http, credential| linear::get_issue(http, credential, &target),
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |s, window, cx| {
                if s.linear_connection_epoch() != epoch || s.linear_org_id() != org.as_deref() {
                    return;
                }
                match result {
                    Ok(snapshot) if Some(snapshot.org.as_str()) == org.as_deref() => {
                        s.attach_issues(name, vec![snapshot], window, cx)
                    }
                    Ok(_) => notify_err(
                        window,
                        cx,
                        "The issue belongs to a different Linear workspace",
                    ),
                    Err(error) => notify_err(window, cx, error.to_string()),
                }
            })
            .ok();
        })
        .detach();
    }
}

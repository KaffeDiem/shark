//! Attaching repositories to an agent: one operation at a time, repo by repo. It waits until
//! nothing runs in the agent, and holds new work until it is over (`Workspace::attaching`, which
//! is saved before Git changes anything, so a restart can tell what happened).
use super::new_workspace::NewWorkspaceForm;
use super::*;
use crate::scripts::{self, Phase};
use crate::state::{AttachStage, Attachment};
use git::Found;

/// How an agent's attachment operation has gone. `attached` and `untold` are told when it ends;
/// `failed` stays in the header until dismissed.
#[derive(Default)]
pub(super) struct Results {
    attached: Vec<String>,
    failed: Vec<(String, String)>,
    untold: Vec<(String, String)>,
}

/// What the end of an operation says, if anything, and whether it all went well. `new` failed
/// since the last report; `stuck` wait on the user. A setup failure alone has told already.
pub(super) fn report(
    title: &str,
    attached: &str,
    new: &[(String, String)],
    stuck: &[String],
) -> Option<(bool, String)> {
    if new.is_empty() && attached.is_empty() {
        return None;
    }
    let done = (!attached.is_empty()).then(|| format!("Attached {attached} to {title}."));
    let failed = new.iter().map(|(r, e)| format!("{r} failed: {e}"));
    let waiting = (!stuck.is_empty()).then(|| format!("Waiting on you: {}", stuck.join(", ")));
    let lines: Vec<String> = done.into_iter().chain(failed).chain(waiting).collect();
    Some((new.is_empty() && stuck.is_empty(), lines.join("\n")))
}

/// Why a restart left a repo's setup undecided.
const SETUP_INTERRUPTED: &str = "Shark quit before its setup finished";

impl Shark {
    /// Every Attach repository offer lands here: the dialog, or with an operation under way, what
    /// can be done about it. Attaching leaves Plan mode and plans as they are.
    pub(super) fn attach_repository(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.attach_form(name, vec![], window, cx)
    }

    /// The dialog with the repos `name` doesn't have yet, `picked` ticked; nothing happens until
    /// it is confirmed.
    pub(super) fn attach_form(
        &mut self,
        name: String,
        picked: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.ws(&name) else { return };
        if let Some(a) = ws.attaching.first() {
            let repo = a.repo.clone();
            return self.attachment_menu(name, repo, window, cx);
        }
        let missing: Vec<Repo> = (self.state.repos.iter())
            .filter(|r| !ws.repos.iter().any(|w| w.name == r.name))
            .cloned()
            .collect();
        let attach = Some((name, ws.default_branch()));
        let shark = cx.entity().downgrade();
        let form = cx.new(|cx| {
            NewWorkspaceForm::new(missing, &picked, None, &[], None, attach, shark, window, cx)
        });
        let input = form.read(cx).branch.clone();
        window.open_dialog(cx, move |d, _, _| {
            let f = form.clone();
            crate::ui::dialog(d)
                .title("Attach repository")
                .w(px(640.))
                .child(form.clone())
                .on_ok(move |_, window, cx| f.update(cx, |f, cx| f.submit(window, cx)))
        });
        input.update(cx, |i, cx| i.focus(window, cx));
    }

    pub(super) fn attach_local_repo(
        &mut self,
        name: String,
        picked: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Attach repository".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            this.update_in(cx, |s, window, cx| {
                s.attach_paths(name, picked, &paths, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Registers the folders and asks about attaching them to `name`.
    pub(super) fn attach_paths(
        &mut self,
        name: String,
        mut picked: Vec<String>,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let added = self.add_repo_paths(paths, window, cx);
        picked.extend(added.into_iter().map(|r| r.name));
        self.attach_form(name, picked, window, cx);
    }

    /// Starts an operation on `picks`: saved first, or nothing happens. From here on the agent's
    /// chats and Git steps wait.
    pub(super) fn queue_attachment(
        &mut self,
        name: String,
        picks: Vec<Attachment>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self.ws_title(&name);
        if self.archiving.contains(&name) {
            return notify_err(window, cx, format!("{title} is being deleted"));
        }
        let Some(ws) = self.ws_mut(&name) else { return };
        if !ws.attaching.is_empty() {
            let msg = format!("{title}: a repository is already being attached");
            return notify_err(window, cx, msg);
        }
        ws.attaching = picks;
        if let Err(e) = self.state.save() {
            if let Some(ws) = self.ws_mut(&name) {
                ws.attaching.clear();
            }
            let msg = format!("Nothing was attached: couldn't save the agent: {e}");
            return notify_err(window, cx, msg);
        }
        self.attach_results.remove(&name);
        self.publish_workspace(&name, cx);
        self.advance_attachment(&name, window, cx);
    }

    /// Hands the saved agent to its loaded chats and Git button. Chats that aren't loaded need
    /// nothing: they read it when they open.
    pub(super) fn publish_workspace(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(ws) = self.ws(name).cloned() else {
            return;
        };
        for chat in self.started_chats(name) {
            chat.update(cx, |c, cx| c.workspace_changed(&ws, cx));
        }
        if let Some(sync) = self.syncs.get(name) {
            sync.update(cx, |g, _| g.set_attaching(ws.attaching));
        }
        self.refresh_attention(cx);
        cx.notify();
    }

    /// Nothing runs in `name` that an attachment must wait for. Git steps and chats are the
    /// state changes this hears about: each calls `advance_attachment`.
    fn attach_idle(&self, name: &str, cx: &App) -> bool {
        !self.archiving.contains(name)
            && !self.adding_repos.contains(name)
            && !self.setting_up.iter().any(|n| n == name)
            && self.syncs.get(name).is_none_or(|g| !g.read(cx).running())
            && self.started_chats(name).iter().all(|c| c.read(cx).idle())
    }

    /// Starts the next queued repo if the agent is idle. The check and the start are this one
    /// update, with no await between them; the hold keeps new work out from queueing on.
    // ponytail: a turn the CLI starts by itself can't be held; it only sees a directory appear.
    pub(super) fn advance_attachment(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let queued = |a: &Attachment| a.stage == AttachStage::Queued;
        if !self
            .ws(name)
            .is_some_and(|w| w.attaching.iter().any(queued))
            || !self.attach_idle(name, cx)
        {
            return;
        }
        let Some(ws) = self.ws_mut(name) else { return };
        let Some(a) = ws.attaching.iter_mut().find(|a| queued(a)) else {
            return;
        };
        a.stage = AttachStage::Creating;
        let a = a.clone();
        let ws = ws.clone();
        if let Err(e) = self.state.save() {
            if let Some(ws) = self.ws_mut(name) {
                ws.attaching
                    .retain(|x| x.stuck() || x.stage == AttachStage::Setup);
            }
            let msg = format!("Nothing more was attached: couldn't save the agent: {e}");
            notify_err(window, cx, msg);
            return self.attach_settled(name, window, cx);
        }
        self.add_repo_to_workspace(ws, a, window, cx);
        self.publish_workspace(name, cx);
    }

    /// Creates `a`'s worktree off the UI thread. Only `advance_attachment` calls this: its intent
    /// is saved by now.
    fn add_repo_to_workspace(
        &mut self,
        ws: Workspace,
        a: Attachment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = ws.name.clone();
        let title = ws.display_title().to_string();
        self.adding_repos.insert(name.clone());
        window.push_notification(format!("Attaching {} to {title}…", a.repo), cx);
        cx.spawn_in(window, async move |this, cx| {
            let dir = ws.dir();
            let (repo, fresh) = (
                Repo {
                    name: a.repo.clone(),
                    path: a.src.clone(),
                },
                a.fresh.then_some(a.base.clone()),
            );
            let branch = a.branch.clone();
            let result =
                cx.background_executor()
                    .spawn(async move {
                        git::add_to_workspace(&ws, &dir, &repo, &branch, fresh.as_deref())
                    })
                    .await;
            this.update_in(cx, |s, window, cx| {
                s.adding_repos.remove(&name);
                match result {
                    Ok(ws) => s.repo_added(ws, &a.repo, window, cx),
                    Err(e) => s.repo_refused(&name, &a.repo, e, window, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Creation failed and rolled back what it made: the next repo goes on, and this one stays in
    /// the header until dismissed.
    fn repo_refused(
        &mut self,
        name: &str,
        repo: &str,
        e: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ws) = self.ws_mut(name) {
            ws.attaching.retain(|a| a.repo != repo);
        }
        let _ = self.state.save();
        let results = self.attach_results.entry(name.into()).or_default();
        results.failed.push((repo.into(), e.clone()));
        results.untold.push((repo.into(), e));
        self.attach_settled(name, window, cx);
    }

    /// Saves the new repo and its branch together, then runs its setup scripts. Readiness waits
    /// for them.
    fn repo_added(
        &mut self,
        ws: Workspace,
        repo: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(slot) = self.ws_mut(&ws.name) else {
            return;
        };
        // Only the repos change; session, model and viewed may have moved on meanwhile.
        let Some(r) = ws.repos.iter().find(|r| r.name == repo).cloned() else {
            return;
        };
        slot.branch = ws.branch.clone();
        slot.repos.push(r);
        if let Some(a) = slot.attaching.iter_mut().find(|a| a.repo == repo) {
            a.stage = AttachStage::Setup;
        }
        let ws = slot.clone();
        self.record_interaction(&ws.name, cx);
        // The intent is still on disk, as Creating: a restart would adopt this worktree.
        if let Err(e) = self.state.save() {
            notify_err(window, cx, format!("Couldn't save the agent: {e}"));
        }
        self.repos_published(&ws, window, cx);
        if !self.run_setup(&ws, Some(repo), None, window, cx) {
            self.attach_done(&ws.name, repo, window, cx);
        }
    }

    /// Everything reading the repo list picks up a new one.
    fn repos_published(&mut self, ws: &Workspace, window: &mut Window, cx: &mut Context<Self>) {
        self.publish_workspace(&ws.name, cx);
        if let Some(sync) = self.syncs.get(&ws.name) {
            sync.update(cx, |g, cx| g.set_workspace(ws.clone(), cx));
        }
        if let Some(git) = self.git_features.get(&ws.name).cloned() {
            let git_ws = self.effective_workspace(ws.clone(), cx);
            git.update(cx, |g, cx| g.set_workspace(git_ws, window, cx));
        }
        self.refresh_diff_if_active(&ws.name, cx);
        // An empty agent's Diff or Git tab may be up, the offer in its place and no Git view made.
        if let (Tab::Feature(feature), 1, false, true) = (
            self.tab,
            ws.repos.len(),
            self.home_shown(),
            self.active.as_ref() == Some(&ws.name),
        ) {
            self.activate_feature(feature, window, cx);
        }
    }

    /// How `repo`'s setup scripts went: a failure keeps the worktree and asks the user.
    pub(super) fn setup_finished(
        &mut self,
        name: &str,
        repo: &str,
        result: Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Err(e) = result else {
            return self.attach_done(name, repo, window, cx);
        };
        let slot =
            (self.ws_mut(name)).and_then(|w| w.attaching.iter_mut().find(|a| a.repo == repo));
        if let Some(a) = slot {
            a.stage = AttachStage::SetupFailed;
            a.error = Some(e);
        }
        let _ = self.state.save();
        self.attach_settled(name, window, cx);
    }

    /// `repo` is ready: its attachment is over.
    fn attach_done(&mut self, name: &str, repo: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ws) = self.ws_mut(name) {
            ws.attaching.retain(|a| a.repo != repo);
        }
        let _ = self.state.save();
        let results = self.attach_results.entry(name.into()).or_default();
        results.attached.push(repo.into());
        self.attach_settled(name, window, cx);
    }

    /// One repo is through: on to the next, and once none is left running, the report. A chat
    /// goes free when nothing is left at all.
    fn attach_settled(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.publish_workspace(name, cx);
        self.advance_attachment(name, window, cx);
        let Some(ws) = self
            .ws(name)
            .filter(|w| w.attaching.iter().all(Attachment::stuck))
        else {
            return;
        };
        let title = ws.display_title().to_string();
        let stuck: Vec<String> = ws.attaching.iter().map(|a| a.repo.clone()).collect();
        let Some(results) = self.attach_results.get_mut(name) else {
            return;
        };
        let attached = std::mem::take(&mut results.attached).join(", ");
        let new = std::mem::take(&mut results.untold);
        match report(&title, &attached, &new, &stuck) {
            Some((true, msg)) => window.push_notification(Notification::success(msg), cx),
            Some((false, msg)) => notify_err(window, cx, msg),
            None => {}
        }
    }

    /// Takes back everything still queued; what has started is not affected.
    pub(super) fn cancel_attachment(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.ws_mut(name) else { return };
        ws.attaching.retain(|a| a.stage != AttachStage::Queued);
        // Unsaved, a restart clears the queued entries too: nothing exists for them.
        let _ = self.state.save();
        window.push_notification("Attachment cancelled", cx);
        self.attach_settled(name, window, cx);
    }

    /// Runs a failed setup again.
    fn retry_setup(&mut self, name: &str, repo: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.attach_idle(name, cx) {
            return notify_err(window, cx, "Wait for what is running in this agent");
        }
        let slot =
            (self.ws_mut(name)).and_then(|w| w.attaching.iter_mut().find(|a| a.repo == repo));
        let Some(a) = slot.filter(|a| a.stage == AttachStage::SetupFailed) else {
            return;
        };
        a.stage = AttachStage::Setup;
        a.error = None;
        let _ = self.state.save();
        self.publish_workspace(name, cx);
        let Some(ws) = self.ws(name).cloned() else {
            return;
        };
        if !self.run_setup(&ws, Some(repo), None, window, cx) {
            self.attach_done(name, repo, window, cx); // its scripts were removed meanwhile
        }
    }

    /// Stops tracking what recovery couldn't identify. Nothing is deleted.
    fn forget_attachment(
        &mut self,
        name: &str,
        repo: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.ws_mut(name) else { return };
        let what = (ws.attaching.iter().find(|a| a.repo == repo))
            .and_then(|a| a.error.clone())
            .unwrap_or_default();
        ws.attaching.retain(|a| a.repo != repo);
        // With its worktree gone it can't stay listed: `ready` would pass on nothing.
        let (dir, had) = (ws.dir(), ws.repos.len());
        ws.repos
            .retain(|r| r.name != repo || dir.join(&r.name).exists());
        let dropped = ws.repos.len() != had;
        let ws = ws.clone();
        if dropped {
            git::write_claude_md(&ws);
        }
        let _ = self.state.save();
        window.push_notification(format!("{repo}: nothing was deleted. {what}"), cx);
        if dropped {
            self.repos_published(&ws, window, cx);
        }
        self.attach_settled(name, window, cx);
    }

    /// Launch: settles what an earlier run left in `Workspace::attaching`. Before any chat opens,
    /// so they all start on the outcome.
    // ponytail: a few git reads on the UI thread, only for agents interrupted mid-attachment.
    pub(super) fn reconcile_attachments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let names: Vec<String> = (self.state.workspaces.iter())
            .filter(|w| !w.attaching.is_empty())
            .map(|w| w.name.clone())
            .collect();
        for name in &names {
            self.reconcile_agent(name);
        }
        if !names.is_empty() {
            if let Err(e) = self.state.save() {
                notify_err(window, cx, format!("Couldn't save the agents: {e}"));
            }
            self.refresh_attention(cx);
        }
    }

    /// Decides each of `name`'s recorded attachments from what is on disk. Finished work is
    /// adopted, untouched intent cleared, the rest kept for the user. Setup never reruns here.
    fn reconcile_agent(&mut self, name: &str) {
        let Some(mut ws) = self.ws(name).cloned() else {
            return;
        };
        for mut a in std::mem::take(&mut ws.attaching) {
            if a.stage == AttachStage::Queued {
                continue;
            }
            if ws.repos.iter().any(|r| r.name == a.repo) {
                if !ws.dir().join(&a.repo).exists() {
                    a.stage = AttachStage::Uncertain;
                    a.error = Some(format!("the worktree of `{}` is gone", a.repo));
                    ws.attaching.push(a);
                    continue;
                }
            } else {
                match git::find_attachment(&ws, &a) {
                    Found::Nothing => continue,
                    Found::Unclear(why) => {
                        a.stage = AttachStage::Uncertain;
                        a.error = Some(why);
                        ws.attaching.push(a);
                        continue;
                    }
                    Found::Worktree(mut wr) => {
                        if ws.repos.is_empty() {
                            ws.branch = a.branch.clone();
                        }
                        wr.branch = (a.branch != ws.branch).then(|| a.branch.clone());
                        ws.repos.push(wr);
                        git::write_claude_md(&ws);
                    }
                }
            }
            if a.stage == AttachStage::SetupFailed {
                ws.attaching.push(a);
            } else if !scripts::jobs(&self.state, &ws, Phase::Setup, Some(&a.repo)).is_empty() {
                a.stage = AttachStage::SetupFailed;
                a.error = Some(SETUP_INTERRUPTED.into());
                ws.attaching.push(a);
            }
        }
        if let Some(slot) = self.ws_mut(name) {
            (slot.repos, slot.branch, slot.attaching) = (ws.repos, ws.branch, ws.attaching);
        }
    }

    /// Check again: the user may have sorted out what recovery found.
    fn recheck_attachment(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.attach_idle(name, cx) {
            return notify_err(window, cx, "Wait for what is running in this agent");
        }
        let before = self.ws(name).map(|w| w.repos.len());
        self.reconcile_agent(name);
        let _ = self.state.save();
        if let Some(ws) = self
            .ws(name)
            .cloned()
            .filter(|w| Some(w.repos.len()) != before)
        {
            self.repos_published(&ws, window, cx);
        }
        self.attach_settled(name, window, cx);
    }

    /// The label the attention system shows while `name` waits on a decision about a repo.
    pub(crate) fn attach_needs(&self, name: &str) -> Option<String> {
        let stuck = (self.ws(name)?.attaching.iter()).find(|a| a.stuck());
        match stuck {
            Some(a) if a.stage == AttachStage::Uncertain => Some(format!("Recover {}", a.repo)),
            Some(a) => Some(format!("Setup failed: {}", a.repo)),
            None => (self.attach_results.get(name)?.failed.first())
                .map(|(r, _)| format!("Couldn't attach {r}")),
        }
    }

    /// What can be done about `repo` right now, as a keyboard menu.
    pub(super) fn attachment_menu(
        &mut self,
        name: String,
        repo: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        type Act = fn(&mut Shark, &str, &str, &mut Window, &mut Context<Shark>);
        let log: Act = |s, n, _, w, cx| s.show_log_of(n, w, cx);
        let stage = (self.ws(&name).into_iter())
            .flat_map(|w| &w.attaching)
            .find(|a| a.repo == repo)
            .map(|a| a.stage);
        let actions: Vec<(IconName, &str, Act)> = match stage {
            Some(AttachStage::Queued) => {
                vec![(IconName::X, "Cancel attachment", |s, n, _, w, cx| {
                    s.cancel_attachment(n, w, cx)
                })]
            }
            Some(AttachStage::Creating) => vec![],
            Some(AttachStage::Setup) => vec![(IconName::ScrollText, "Show setup log", log)],
            Some(AttachStage::SetupFailed) => vec![
                (IconName::ScrollText, "Show setup log", log),
                (IconName::RefreshCw, "Retry setup", |s, n, r, w, cx| {
                    s.retry_setup(n, r, w, cx)
                }),
                (
                    IconName::Check,
                    "Continue without successful setup",
                    |s, n, r, w, cx| s.attach_done(n, r, w, cx),
                ),
            ],
            Some(AttachStage::Uncertain) => vec![
                (IconName::RefreshCw, "Check again", |s, n, _, w, cx| {
                    s.recheck_attachment(n, w, cx)
                }),
                (
                    IconName::X,
                    "Stop tracking it (deletes nothing)",
                    |s, n, r, w, cx| s.forget_attachment(n, r, w, cx),
                ),
            ],
            None => vec![(IconName::X, "Dismiss", |s, n, r, _, cx| {
                if let Some(results) = s.attach_results.get_mut(n) {
                    results.failed.retain(|(x, _)| x != r);
                }
                s.refresh_attention(cx);
                cx.notify();
            })],
        };
        if actions.is_empty() {
            return window.push_notification(format!("{repo} is being created"), cx);
        }
        let items = (actions.iter())
            .map(|(icon, label, _)| CommandItem::new().label(*label).icon(*icon))
            .collect();
        open_menu(
            cx.entity().downgrade(),
            "Repository attachment…",
            items,
            0,
            move |s, row, w, cx| actions[row].2(s, &name, &repo, w, cx),
            window,
            cx,
        );
    }

    /// A chip in the header's attachment row.
    pub(super) fn chip(
        id: impl Into<ElementId>,
        icon: IconName,
        label: String,
        theme: &Theme,
    ) -> Stateful<Div> {
        h_flex()
            .id(id)
            .flex_none()
            .h(px(22.))
            .px_1p5()
            .gap_1()
            .rounded(theme.radius)
            .bg(theme.muted)
            .text_xs()
            .child(Icon::new(icon).with_size(px(12.)))
            .child(label)
    }

    /// The header's attachments: a chip per repository, then one per attachment under way or
    /// failed, then one per Linear issue, then Attach repository and Attach Linear issue.
    // ponytail: past what fits, chips clip; the Attach buttons stay and the sidebar lists the
    // repositories, Show Linear issue the issues.
    pub(super) fn render_attachments(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(ws) = self.active_ws() else {
            return div().into_any_element();
        };
        let name = ws.name.clone();
        let theme = cx.theme();
        let settled = (ws.repos.iter())
            .filter(|r| !ws.attaching.iter().any(|a| a.repo == r.name))
            .map(|r| {
                Self::chip(
                    SharedString::from(format!("repo-chip-{}", r.name)),
                    IconName::FolderGit2,
                    r.name.clone(),
                    theme,
                )
                .tooltip({
                    let tip = format!("{} on {}", r.name, ws.branch_of(r));
                    move |w, cx| Tooltip::new(tip.clone()).build(w, cx)
                })
                .test_support()
                .into_any_element()
            })
            .collect::<Vec<_>>();
        let failed = self.attach_results.get(&name).into_iter();
        let failed = failed.flat_map(|r| {
            r.failed
                .iter()
                .map(|(r, e)| (r.clone(), "failed", Some(e.clone())))
        });
        let pending = (ws.attaching.iter())
            .map(|a| {
                let state = match a.stage {
                    AttachStage::Queued => "waiting",
                    AttachStage::Creating => "creating",
                    AttachStage::Setup => "setting up",
                    AttachStage::SetupFailed => "setup failed",
                    AttachStage::Uncertain => "needs recovery",
                };
                (a.repo.clone(), state, a.error.clone())
            })
            .chain(failed)
            .map(|(repo, state, error)| {
                let tip = error.unwrap_or_else(|| format!("{repo}: {state}"));
                let warn = matches!(state, "setup failed" | "needs recovery" | "failed");
                let (n, r) = (name.clone(), repo.clone());
                Self::chip(
                    SharedString::from(format!("attach-chip-{repo}")),
                    if warn {
                        IconName::TriangleAlert
                    } else {
                        IconName::Clock
                    },
                    format!("{repo} · {state}"),
                    cx.theme(),
                )
                .when(warn, |d| d.text_color(cx.theme().warning_foreground))
                .cursor_pointer()
                .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                .on_click(
                    cx.listener(move |s, _, w, cx| s.attachment_menu(n.clone(), r.clone(), w, cx)),
                )
                .test_support()
                .into_any_element()
            })
            .collect::<Vec<_>>();
        let issues = self.issue_chips(ws, cx);
        h_flex()
            .flex_shrink_1()
            .min_w_0()
            .gap_1()
            .child(
                h_flex()
                    .flex_shrink_1()
                    .min_w_0()
                    .overflow_hidden()
                    .gap_1()
                    .children(settled)
                    .children(pending)
                    .children(issues),
            )
            .child(
                self.attach_button("header-attach", cx)
                    .ghost()
                    .xsmall()
                    .flex_none(),
            )
            .child(
                Button::new("header-attach-issue")
                    .label("Attach Linear issue")
                    .ghost()
                    .xsmall()
                    .flex_none()
                    .on_click(
                        cx.listener(move |s, _, w, cx| s.attach_linear_issue(name.clone(), w, cx)),
                    ),
            )
            .into_any_element()
    }
}

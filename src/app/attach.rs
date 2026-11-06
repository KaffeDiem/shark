//! Attaching repositories to an agent: one operation at a time, repo by repo. It waits until
//! nothing runs in the agent, and holds new work until it is over (`Workspace::attaching`, which
//! is saved before Git changes anything, so a restart can tell what happened).
use super::interaction::now_ms;
use super::new_workspace::{AttachTo, NewWorkspaceForm};
use super::*;
use crate::scripts::{self, Phase};
use crate::state::{AttachStage, Attachment};
use crate::ui::{menu_dialog, open_grouped};
use git::Found;
use std::cell::Cell;
use std::time::Duration;

/// How an agent's attachment operation has gone. `attached` and `untold` are told when it ends;
/// `failed` stays in the header until dismissed.
#[derive(Default)]
pub(super) struct Results {
    attached: Vec<String>,
    pub(super) failed: Vec<(String, String)>,
    untold: Vec<(String, String)>,
}

/// How a repo stands, in the order the header lists them: what needs the user comes first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Standing {
    Failed,
    Waiting,
    Running,
    Queued,
    Ready,
}

impl Standing {
    /// The status mark, the one colored thing on a chip.
    fn mark(self, theme: &Theme) -> Icon {
        let (icon, color) = match self {
            Standing::Failed => (IconName::CircleX, theme.danger_foreground),
            Standing::Waiting => (IconName::CircleAlert, theme.warning_foreground),
            Standing::Running => (IconName::RefreshCw, theme.warning_foreground),
            Standing::Queued => (IconName::Clock, theme.muted_foreground),
            Standing::Ready => (IconName::CircleCheck, theme.success_foreground),
        };
        Icon::new(icon).text_color(color)
    }
}

/// A repo as the header shows it.
struct RepoRow {
    repo: String,
    standing: Standing,
    state: &'static str,
    tip: String,
}

/// A step of an attachment, as its panel marks it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Step {
    Done,
    Current,
    Pending,
    Failed,
}

/// What a row of Show attachments opens.
pub(super) enum Pick {
    Repo(String),
    Issue(crate::linear::IssueRef),
    Attach,
    AttachIssue,
}

/// Its label, status mark and target.
pub(super) type ListRow = (String, Icon, Pick);

type Act = fn(&mut Shark, &str, &str, &mut Window, &mut Context<Shark>);

/// The last lines of `repo`'s part of an agent's script log. Repos share the log, and the next
/// repo's setup runs right after a failure, so its plain tail can be someone else's.
pub(super) fn setup_tail(log: &str, repo: &str) -> String {
    let Some(start) = log.rfind(&format!("$ {repo}: ")) else {
        return String::new();
    };
    let part = &log[start..];
    let part = &part[..part.find("\n\n$ ").unwrap_or(part.len())];
    let lines: Vec<&str> = part.lines().collect();
    lines[lines.len().saturating_sub(8)..].join("\n")
}

/// The end of `name`'s script log: a dev server's output can make it long.
fn log_end(name: &str) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut bytes = vec![];
    if let Ok(mut f) = std::fs::File::open(scripts::log_path(name)) {
        let len = f.metadata().map_or(0, |m| m.len());
        let _ = f.seek(SeekFrom::Start(len.saturating_sub(65536)));
        let _ = f.read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// What a chip opens: where `repo`'s attachment is and what can be done about it. Not
/// `open_headed`: its rows are fixed when it opens, and a panel left open on a running setup must
/// offer Retry once the setup fails.
struct AttachPanel {
    name: String,
    repo: String,
    shark: WeakEntity<Shark>,
    picker: Entity<CommandState>,
    /// The failed setup's log lines, read when the failure first shows.
    log: Option<String>,
    _updates: Subscription,
}

impl Render for AttachPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(shark) = self.shark.upgrade() else {
            return div().into_any_element();
        };
        let s = shark.read(cx);
        let summary = s.attach_summary(&self.name, &self.repo);
        let steps = s.attach_steps(&self.name, &self.repo, cx);
        let actions = s.attach_actions(&self.name, &self.repo);
        if steps[2].1 == Step::Failed && self.log.is_none() {
            self.log = Some(setup_tail(&log_end(&self.name), &self.repo));
        }
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger_foreground);
        let rows = steps
            .into_iter()
            .enumerate()
            .map(|(i, (label, step, detail))| {
                let icon = |name: IconName, color| {
                    let icon = Icon::new(name).text_color(color);
                    icon.with_size(px(14.)).into_any_element()
                };
                let mark = match step {
                    Step::Done => icon(IconName::Check, theme.success_foreground),
                    Step::Current => Spinner::new().with_size(px(14.)).into_any_element(),
                    Step::Pending => icon(IconName::Circle, muted),
                    Step::Failed => icon(IconName::X, danger),
                };
                let log = (self.log.as_ref())
                    .filter(|l| step == Step::Failed && i == 2 && !l.is_empty())
                    .map(|log| {
                        v_flex()
                            .mt_1()
                            .p_2()
                            .rounded(theme.radius)
                            .bg(theme.muted)
                            .font_family(theme.mono_font_family.clone())
                            .text_size(theme.mono_font_size)
                            .text_color(muted)
                            .children(log.lines().map(|l| div().truncate().child(l.to_string())))
                    });
                v_flex()
                    .id(("attach-step", i))
                    .aria_label(format!("{label}: {step:?}"))
                    .test_support()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_none().size(px(14.)).child(mark))
                            .child(label),
                    )
                    .child(
                        v_flex()
                            .pl(px(22.))
                            .gap_0p5()
                            .text_xs()
                            .text_color(if step == Step::Failed { danger } else { muted })
                            .children(detail)
                            .children(log),
                    )
            });
        let header = v_flex()
            .id("attach-panel")
            .test_support()
            .px_3()
            .py_2()
            .gap_2()
            .text_sm()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.repo.clone()),
                    )
                    .children(summary.map(|s| div().text_xs().text_color(muted).child(s))),
            )
            .children(rows);
        let items: Vec<CommandItem> = (actions.iter())
            .map(|(icon, label, _)| CommandItem::new().label(*label).icon(*icon))
            .collect();
        let (name, repo, target) = (self.name.clone(), self.repo.clone(), self.shark.clone());
        let command = Command::new(&self.picker)
            .bordered(false)
            .placeholder("Repository attachment…")
            .items(items)
            .on_confirm(move |ix, window, cx| {
                window.close_dialog(cx);
                let act = actions[ix.row].2;
                target
                    .update(cx, |s, cx| act(s, &name, &repo, window, cx))
                    .ok();
            });
        v_flex().child(header).child(command).into_any_element()
    }
}

/// How the header's chips fit, as last measured. `hidden` didn't fit, and `+N` goes `more_at` into
/// the row; `tight` is set once the Attach button gave up its label for them: at this window
/// width and chip count.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(super) struct Fit {
    pub(super) hidden: usize,
    more_at: Pixels,
    tight: Option<(Pixels, usize)>,
}

/// About what the Attach button's label takes.
const LABEL: Pixels = px(60.);
/// What `+N` needs at the end of the row.
const MORE: Pixels = px(56.);

impl Fit {
    /// After a frame with `clipped` of `chips` past the row's end. The label goes first and comes
    /// back only once it would fit again, so the row doesn't flip between the two.
    pub(super) fn next(self, clipped: usize, chips: usize, width: Pixels) -> Self {
        match self.tight {
            None if clipped > 0 => Fit {
                hidden: 0,
                tight: Some((width, chips)),
                ..self
            },
            Some((at, n)) if chips < n || width > at + LABEL => Fit::default(),
            tight => Fit {
                hidden: clipped,
                tight,
                ..self
            },
        }
    }
}

/// The header's chips and Attach. Lays every chip out, hiding what doesn't fit, so that what is
/// measured doesn't depend on what is shown; `render_attachments` built them.
// ponytail: `+N` sits by the last chip shown, so up to a chip's width stays empty before the
// button; closing that means laying chips out by measured widths instead.
#[derive(IntoElement)]
struct AttachRow {
    name: String,
    chips: Vec<AnyElement>,
    /// The first this many chips are failures.
    failed: usize,
    /// Outside the chips: it says something about several, so it never goes under `+N`.
    disconnected: Option<AnyElement>,
    linear: bool,
    shark: WeakEntity<Shark>,
}

impl RenderOnce for AttachRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let key = SharedString::from(format!("attach-fit-{}", self.name));
        let fit = window.use_keyed_state(key, cx, |_, _| Fit::default());
        let was = *fit.read(cx);
        let count = self.chips.len();
        let shown = count - was.hidden.min(count);
        let compact = was.tight.is_some();
        let key = SharedString::from(format!("attach-at-{}", self.name));
        let at = window.use_keyed_state(key, cx, |_, _| Rc::<Cell<Bounds<Pixels>>>::default());
        let at = at.read(cx).clone();
        let attach = {
            let (shark, name, linear, at) = (
                self.shark.clone(),
                self.name.clone(),
                self.linear,
                at.clone(),
            );
            Button::new("header-attach")
                .icon(IconName::Paperclip)
                .ghost()
                .xsmall()
                .dropdown_caret(linear && !compact)
                .accessibility_label("Attach")
                .map(|b| match compact {
                    true => b.tooltip("Attach"),
                    false => b.label("Attach"),
                })
                .on_click(move |_, w, cx| {
                    let name = name.clone();
                    shark
                        .update(cx, |s, cx| match linear {
                            true => s.attach_menu(name, at.get(), w, cx),
                            false => s.attach_repository(name, w, cx),
                        })
                        .ok();
                })
        };
        let more = (shown < count).then(|| {
            let (shark, name) = (self.shark.clone(), self.name.clone());
            let icon = match shown < self.failed {
                true => Standing::Failed.mark(cx.theme()),
                false => Icon::new(IconName::Ellipsis),
            };
            let hidden = count - shown;
            Shark::chip("attach-more", icon, format!("+{hidden}"), cx.theme())
                .absolute()
                .left(was.more_at)
                .cursor_pointer()
                .aria_label(format!("Show {hidden} more"))
                .tooltip(move |w, cx| Tooltip::new(format!("Show {hidden} more")).build(w, cx))
                .on_click(move |_, w, cx| {
                    let name = name.clone();
                    shark
                        .update(cx, |s, cx| s.attachment_list(name, shown, w, cx))
                        .ok();
                })
                .test_support()
        });
        h_flex()
            .flex_shrink_1()
            .min_w_0()
            .gap_1()
            .child(
                h_flex()
                    .relative()
                    .flex_shrink_1()
                    .min_w_0()
                    .overflow_hidden()
                    .gap_1()
                    // The first child, so the measuring below gets the row's own bounds.
                    .child(canvas(|_, _, _| {}, |_, _, _, _| {}).absolute().size_full())
                    .children(self.chips.into_iter().enumerate().map(|(i, chip)| {
                        div()
                            .flex_none()
                            .when(i >= shown, |d| d.invisible())
                            .child(chip)
                    }))
                    .children(more)
                    .on_children_prepainted(move |bounds, window, cx| {
                        let Some((row, rest)) = bounds.split_first() else {
                            return;
                        };
                        let chips = &rest[..count];
                        let end = row.right() + px(0.5);
                        let over = chips.iter().any(|c| c.right() > end);
                        let edge = if over { end - MORE } else { end };
                        let clipped = chips.iter().filter(|c| c.right() > edge).count();
                        let last = chips[..count - clipped].last();
                        let next = Fit {
                            more_at: last.map_or(px(0.), |c| c.right() - row.left() + px(4.)),
                            ..was.next(clipped, count, window.viewport_size().width)
                        };
                        if next != was {
                            // Too late for this frame: the next one is drawn with it.
                            let fit = fit.clone();
                            cx.defer(move |cx| {
                                fit.update(cx, |f, cx| {
                                    *f = next;
                                    cx.notify();
                                })
                            });
                        }
                    }),
            )
            .children(self.disconnected)
            .child(
                div().relative().flex_none().child(attach).child(
                    canvas(move |b, _, _| at.set(b), |_, _, _, _| {})
                        .absolute()
                        .size_full(),
                ),
            )
    }
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

    /// The dialog with the repos, `picked` marked; nothing happens until it is confirmed. With an operation under way or a failure to deal with, those come first,
    /// so that everything on the header's chips can be reached from cmd-k.
    pub(super) fn attach_form(
        &mut self,
        name: String,
        picked: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.ws(&name) else { return };
        let failed = (self.attach_results.get(&name)).is_some_and(|r| !r.failed.is_empty());
        match (ws.attaching.as_slice(), failed) {
            ([], false) => {}
            ([], true) if !picked.is_empty() => {}
            ([a], false) => {
                let repo = a.repo.clone();
                return self.attachment_menu(name, repo, window, cx);
            }
            _ => return self.attachment_list(name, 0, window, cx),
        }
        self.attach_dialog(name, picked, window, cx)
    }

    pub(super) fn attach_dialog(
        &mut self,
        name: String,
        picked: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Nothing to list: straight to the folder panel.
        if self.state.repos.is_empty() {
            return self.attach_local_repo(name, None, window, cx);
        }
        let Some(ws) = self.ws(&name) else { return };
        let attached: Vec<String> = ws.repos.iter().map(|r| r.name.clone()).collect();
        // A repo's last use is the last interaction with an agent that has it.
        let last = |r: &Repo| {
            (self.state.workspaces.iter())
                .filter(|w| w.repos.iter().any(|x| x.name == r.name))
                .filter_map(|w| w.last_interacted_at)
                .max()
        };
        let mut repos = self.state.repos.clone();
        repos.sort_by_key(|r| (attached.contains(&r.name), std::cmp::Reverse(last(r))));
        let now = now_ms();
        let used = (repos.iter())
            .filter_map(|r| {
                let ms = now.saturating_sub(last(r)?);
                let ago = crate::sync::ago(Duration::from_millis(ms));
                Some((r.name.clone(), ago))
            })
            .collect();
        let attach = Some(AttachTo {
            name,
            branch: ws.default_branch(),
            follows: !ws.repos.is_empty(),
            attached,
            used,
        });
        let shark = cx.entity().downgrade();
        let form = cx.new(|cx| {
            NewWorkspaceForm::new(repos, &picked, None, &[], None, attach, shark, window, cx)
        });
        let opened = form.clone();
        window.open_dialog(cx, move |d, _, cx| {
            let f = form.clone();
            menu_dialog(d, cx)
                .child(form.clone())
                .on_ok(move |_, window, cx| f.update(cx, |f, cx| f.submit(window, cx)))
        });
        opened.update(cx, |f, cx| f.focus(window, cx));
    }

    /// The folder panel. Over the dialog (`form`), what it adds arrives there, and cancelling
    /// leaves the dialog as it was. Without one, the dialog opens on what it adds.
    pub(super) fn attach_local_repo(
        &mut self,
        name: String,
        form: Option<WeakEntity<NewWorkspaceForm>>,
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
            this.update_in(cx, |s, window, cx| match form.and_then(|f| f.upgrade()) {
                Some(form) => {
                    let added = s.add_repo_paths(&paths, window, cx);
                    form.update(cx, |f, cx| f.added(added, window, cx));
                }
                None => s.attach_paths(name, &paths, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Registers the folders and asks about attaching them to `name`. Folders it refused are told
    /// and end here: the panel doesn't reopen on its own.
    pub(super) fn attach_paths(
        &mut self,
        name: String,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let picked: Vec<String> = (self.add_repo_paths(paths, window, cx).into_iter())
            .map(|r| r.name)
            .collect();
        if !picked.is_empty() {
            self.attach_form(name, picked, window, cx);
        }
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
        let queued: Vec<&str> = (self.ws(&name).into_iter())
            .flat_map(|w| &w.attaching)
            .filter(|a| a.stage == AttachStage::Queued)
            .map(|a| a.repo.as_str())
            .collect();
        if !queued.is_empty() {
            let msg = format!(
                "{} queued: waiting for {}…",
                queued.join(", "),
                self.attach_waits(&name, cx)
            );
            window.push_notification(msg, cx);
        }
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

    /// What `attach_idle` is waiting on, in words.
    pub(super) fn attach_waits(&self, name: &str, cx: &App) -> String {
        let chats = self.started_chats(name);
        let chats = |test: &dyn Fn(&Chat) -> bool, what: &str| {
            let n = chats.iter().filter(|c| test(c.read(cx))).count();
            (n > 0).then(|| format!("{n} chat{} {what}", if n == 1 { "" } else { "s" }))
        };
        let moving = (self.ws(name).into_iter())
            .flat_map(|w| &w.attaching)
            .find(|a| matches!(a.stage, AttachStage::Creating | AttachStage::Setup));
        let setup = self.setting_up.iter().any(|n| n == name);
        let git = self.syncs.get(name).is_some_and(|g| g.read(cx).running());
        let why: Vec<String> = [
            moving.map(|a| a.repo.clone()),
            (moving.is_none() && setup).then(|| "setup scripts".into()),
            git.then(|| "a Git step".into()),
            chats(&|c| !c.idle() && !c.asking(), "running"),
            chats(&Chat::asking, "asking you something"),
        ]
        .into_iter()
        .flatten()
        .collect();
        match why.is_empty() {
            true => "its turn".into(),
            false => why.join(", "),
        }
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

    /// Creation failed: keep anything rollback couldn't safely delete in recovery; an ordinary
    /// refusal ends here. Either way the next repo goes on and the error is reported.
    fn repo_refused(
        &mut self,
        name: &str,
        repo: &str,
        e: git::AttachError,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ws) = self.ws_mut(name) {
            if e.preserved {
                if let Some(a) = ws.attaching.iter_mut().find(|a| a.repo == repo) {
                    a.stage = AttachStage::Uncertain;
                    a.error = Some(e.message.clone());
                }
            } else {
                ws.attaching.retain(|a| a.repo != repo);
            }
        }
        let _ = self.state.save();
        let results = self.attach_results.entry(name.into()).or_default();
        if !e.preserved {
            results.failed.push((repo.into(), e.message.clone()));
        }
        results.untold.push((repo.into(), e.message));
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
        self.cancel_queued(name, None, window, cx)
    }

    /// Takes `repo`, or without one all, back from the queue. Once started it can't be.
    pub(super) fn cancel_queued(
        &mut self,
        name: &str,
        repo: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.ws_mut(name) else { return };
        let had = ws.attaching.len();
        (ws.attaching)
            .retain(|a| a.stage != AttachStage::Queued || repo.is_some_and(|r| r != a.repo));
        if ws.attaching.len() == had {
            return notify_err(window, cx, "It has started, so it can't be cancelled");
        }
        // Unsaved, a restart clears the queued entries too: nothing exists for them.
        let _ = self.state.save();
        let done = repo.map_or("Attachment cancelled".into(), |r| {
            format!("{r}: attachment cancelled")
        });
        window.push_notification(done, cx);
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
        let Some(a) = ws
            .attaching
            .iter()
            .find(|a| a.repo == repo && a.stage == AttachStage::Uncertain)
        else {
            return;
        };
        let what = a.error.clone().unwrap_or_default();
        ws.attaching.retain(|a| a.repo != repo);
        // An unverified worktree can't stay listed as ready; its files stay on disk.
        let had = ws.repos.len();
        ws.repos.retain(|r| r.name != repo);
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
            let recorded = ws.repos.iter().any(|r| r.name == a.repo);
            match git::find_attachment(&ws, &a) {
                Found::Nothing if !recorded => continue,
                Found::Nothing => {
                    a.stage = AttachStage::Uncertain;
                    a.error = Some(format!("the worktree of `{}` is gone", a.repo));
                    ws.attaching.push(a);
                    continue;
                }
                Found::Unclear(why) => {
                    a.stage = AttachStage::Uncertain;
                    a.error = Some(why);
                    ws.attaching.push(a);
                    continue;
                }
                Found::Worktree(mut wr) if !recorded => {
                    if ws.repos.is_empty() {
                        ws.branch = a.branch.clone();
                    }
                    wr.branch = (a.branch != ws.branch).then(|| a.branch.clone());
                    ws.repos.push(wr);
                    git::write_claude_md(&ws);
                }
                Found::Worktree(_) => {}
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

    /// The branch and base `repo` gets or got, for its panel.
    fn attach_summary(&self, name: &str, repo: &str) -> Option<String> {
        let ws = self.ws(name)?;
        if let Some(a) = ws.attaching.iter().find(|a| a.repo == repo) {
            return Some(match (a.fresh, a.base.as_str()) {
                (false, _) => a.branch.clone(),
                (true, "") => format!("{} from the default branch", a.branch),
                (true, base) => format!("{} from {base}", a.branch),
            });
        }
        let r = ws.repos.iter().find(|r| r.name == repo)?;
        Some(format!("{} from {}", ws.branch_of(r), r.base))
    }

    /// Where `repo` is, step by step, with what the current step waits for or the failed one's
    /// error.
    pub(super) fn attach_steps(
        &self,
        name: &str,
        repo: &str,
        cx: &App,
    ) -> [(&'static str, Step, Option<String>); 3] {
        use Step::*;
        let a = (self.ws(name).into_iter())
            .flat_map(|w| &w.attaching)
            .find(|a| a.repo == repo);
        let refused = (self.attach_results.get(name).into_iter())
            .flat_map(|r| &r.failed)
            .find(|(r, _)| r == repo)
            .map(|(_, e)| e.clone());
        let steps = match (a.map(|a| a.stage), &refused) {
            (Some(AttachStage::Queued), _) => [Current, Pending, Pending],
            (Some(AttachStage::Creating), _) => [Done, Current, Pending],
            (Some(AttachStage::Setup), _) => [Done, Done, Current],
            (Some(AttachStage::SetupFailed), _) => [Done, Done, Failed],
            (Some(AttachStage::Uncertain), _) | (None, Some(_)) => [Done, Failed, Pending],
            (None, None) => [Done, Done, Done],
        };
        let error = a.and_then(|a| a.error.clone()).or(refused);
        let detail = |i: usize| match steps[i] {
            Failed => error.clone(),
            Current if i == 0 => Some(self.attach_waits(name, cx)),
            _ => None,
        };
        [
            ("Waiting for idle", steps[0], detail(0)),
            ("Creating worktree", steps[1], detail(1)),
            ("Running setup", steps[2], detail(2)),
        ]
    }

    /// What can be done about `repo` right now. Dismissing a failed setup only closes the panel:
    /// the chats go on waiting for Retry or Continue anyway.
    pub(super) fn attach_actions(
        &self,
        name: &str,
        repo: &str,
    ) -> Vec<(IconName, &'static str, Act)> {
        let log: Act = |s, n, _, w, cx| s.show_log_of(n, w, cx);
        let close: Act = |_, _, _, _, _| {};
        let ws = self.ws(name);
        let stage = (ws.into_iter())
            .flat_map(|w| &w.attaching)
            .find(|a| a.repo == repo)
            .map(|a| a.stage);
        match stage {
            Some(AttachStage::Queued) => {
                let cancel: Act = |s, n, r, w, cx| s.cancel_queued(n, Some(r), w, cx);
                let all: Act = |s, n, _, w, cx| s.cancel_attachment(n, w, cx);
                let queued = (ws.into_iter())
                    .flat_map(|w| &w.attaching)
                    .filter(|a| a.stage == AttachStage::Queued);
                let mut rows = vec![(IconName::X, "Cancel attachment", cancel)];
                rows.extend((queued.count() > 1).then_some((
                    IconName::X,
                    "Cancel all queued",
                    all,
                )));
                rows
            }
            Some(AttachStage::Creating) => vec![(IconName::X, "Close", close)],
            Some(AttachStage::Setup) => vec![(IconName::ExternalLink, "Open setup log", log)],
            Some(AttachStage::SetupFailed) => vec![
                (IconName::ExternalLink, "Open setup log", log),
                (IconName::RefreshCw, "Retry", |s, n, r, w, cx| {
                    s.retry_setup(n, r, w, cx)
                }),
                (IconName::Check, "Continue anyway", |s, n, r, w, cx| {
                    s.attach_done(n, r, w, cx)
                }),
                (IconName::X, "Dismiss", close),
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
            None if (self.attach_results.get(name).into_iter())
                .flat_map(|r| &r.failed)
                .any(|(r, _)| r == repo) =>
            {
                let dismiss: Act = |s, n, r, _, cx| {
                    if let Some(results) = s.attach_results.get_mut(n) {
                        results.failed.retain(|(x, _)| x != r);
                    }
                    s.refresh_attention(cx);
                    cx.notify();
                };
                // ponytail: a refused attachment keeps no branch or base, so Retry goes through
                // the dialog; keep the `Attachment` in `Results` to retry it as it was.
                let retry: Act =
                    |s, n, r, w, cx| s.attach_form(n.to_string(), vec![r.to_string()], w, cx);
                let idle = ws.is_some_and(|w| w.attaching.is_empty());
                let mut rows = vec![(IconName::X, "Dismiss", dismiss)];
                rows.extend(idle.then_some((IconName::RefreshCw, "Retry…", retry)));
                rows
            }
            None => vec![(IconName::X, "Close", close)],
        }
    }

    /// Opens `repo`'s panel: its steps, and what can be done about it, as a keyboard menu.
    pub(super) fn attachment_menu(
        &mut self,
        name: String,
        repo: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shark = cx.entity();
        let panel = cx.new(|cx| AttachPanel {
            name,
            repo,
            shark: shark.downgrade(),
            picker: cx.new(|cx| CommandState::new(window, cx)),
            log: None,
            _updates: cx.observe(&shark, |_, _, cx| cx.notify()),
        });
        let picker = panel.read(cx).picker.clone();
        window.open_dialog(cx, move |d, _, cx| menu_dialog(d, cx).child(panel.clone()));
        picker.update(cx, |p, cx| p.focus(window, cx));
    }

    /// Every repo the header shows, what needs the user first.
    fn repo_rows(&self, ws: &Workspace) -> Vec<RepoRow> {
        let pending = (ws.attaching.iter()).map(|a| {
            let (standing, state) = match a.stage {
                AttachStage::Queued => (Standing::Queued, "queued"),
                AttachStage::Creating => (Standing::Running, "creating its worktree"),
                AttachStage::Setup => (Standing::Running, "running setup"),
                AttachStage::SetupFailed => (Standing::Failed, "setup failed"),
                AttachStage::Uncertain => (Standing::Waiting, "check what happened"),
            };
            let tip = (a.error.clone()).unwrap_or_else(|| format!("{}: {state}", a.repo));
            (a.repo.clone(), standing, state, tip)
        });
        let refused = (self.attach_results.get(&ws.name).into_iter())
            .flat_map(|r| &r.failed)
            .map(|(r, e)| (r.clone(), Standing::Failed, "couldn't attach", e.clone()));
        let settled = (ws.repos.iter())
            .filter(|r| !ws.attaching.iter().any(|a| a.repo == r.name))
            .map(|r| {
                let tip = format!("{} on {}", r.name, ws.branch_of(r));
                (r.name.clone(), Standing::Ready, "ready", tip)
            });
        let mut rows: Vec<RepoRow> = (pending.chain(refused).chain(settled))
            .map(|(repo, standing, state, tip)| RepoRow {
                repo,
                standing,
                state,
                tip,
            })
            .collect();
        rows.sort_by_key(|r| r.standing);
        rows
    }

    /// The header's Attach with Linear in use: which of the two dialogs, hanging from its button
    /// `b`. They stay two: a repository is a form queued behind idle, an issue a search that works
    /// mid-turn.
    fn attach_menu(
        &mut self,
        name: String,
        b: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shark = cx.entity().downgrade();
        let row = |icon, label, open: fn(&mut Self, String, &mut Window, &mut Context<Self>)| {
            let (shark, name) = (shark.clone(), name.clone());
            menu::item(icon, label, move |w, cx| {
                shark.update(cx, |s, cx| open(s, name.clone(), w, cx)).ok();
            })
        };
        let entries = vec![
            row(IconName::FolderPlus, "Repository…", Self::attach_repository),
            row(
                IconName::TagPlus,
                "Linear issue…",
                Self::attach_linear_issue,
            ),
        ];
        let pos = point(b.right(), b.bottom() + px(4.));
        self.show_menu_at(entries, pos, Anchor::TopRight, window, cx);
        if let Some((m, _)) = &self.menu {
            m.update(cx, |m, _| m.select(0));
        }
    }

    /// Branch actions remain reachable when the toolbar is hidden or its chips overflow.
    pub(super) fn open_branch_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active_ws() else {
            return notify_err(window, cx, "Open an agent first");
        };
        let name = ws.name.clone();
        let repos = self.repo_rows(ws);
        match repos.len() {
            0 => self.attach_repository(name, window, cx),
            1 => self.repository_menu(name, repos[0].repo.clone(), None, window, cx),
            _ => {
                let items = repos
                    .iter()
                    .map(|r| {
                        let branch = ws
                            .repos
                            .iter()
                            .find(|repo| repo.name == r.repo)
                            .filter(|_| r.standing == Standing::Ready)
                            .map(|repo| {
                                menus::repo_branch(self, ws, repo, cx)
                                    .unwrap_or_else(|| "Detached HEAD".into())
                            });
                        CommandItem::new()
                            .label(format!(
                                "{} · {}",
                                r.repo,
                                branch.as_deref().unwrap_or(r.state)
                            ))
                            .icon(r.standing.mark(cx.theme()))
                    })
                    .collect();
                open_menu(
                    cx.entity().downgrade(),
                    "Repository branch…",
                    items,
                    0,
                    move |s, row, w, cx| {
                        s.repository_menu(name.clone(), repos[row].repo.clone(), None, w, cx)
                    },
                    window,
                    cx,
                );
            }
        }
    }

    /// The same destination from a chip, overflow, and the keyboard picker.
    pub(super) fn repository_menu(
        &mut self,
        name: String,
        repo: String,
        position: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.ws(&name) else { return };
        let Some(row) = self.repo_rows(ws).into_iter().find(|r| r.repo == repo) else {
            return;
        };
        if row.standing != Standing::Ready {
            return self.attachment_menu(name, repo, window, cx);
        }
        let entries = menus::branch_entries(self, ws, Some(&repo), cx);
        let position =
            position.unwrap_or_else(|| point(window.viewport_size().width / 2., px(48.)));
        self.show_menu(position, entries, window, cx);
    }

    fn repository_tip(
        &self,
        ws: &Workspace,
        repo: &crate::state::WsRepo,
        branch: &str,
        cx: &App,
    ) -> String {
        let attached = ws.branch_of(repo);
        let context = if branch == attached {
            format!("{} ← {branch}", repo.base)
        } else {
            format!("Checked out: {branch}\nBase: {} ← {attached}", repo.base)
        };
        let mut tip = format!(
            "{}\n{context}\nWorktree: {}",
            repo.name,
            ws.dir().join(&repo.name).display()
        );
        let probed = self
            .syncs
            .get(&ws.name)
            .and_then(|sync| sync.read(cx).repos())
            .and_then(|repos| repos.iter().find(|r| r.name == repo.name));
        match probed {
            None => tip.push_str("\nGit status not yet checked"),
            Some(r) => {
                if let Err(error) = &r.local {
                    tip.push_str(&format!("\nGit status unavailable: {error}"));
                }
            }
        }
        if let Some(stack) = menus::stacks(self, ws, cx)
            .iter()
            .find(|s| s.repo == repo.name)
        {
            tip.push_str(&format!(
                "\nStack: {} of {}",
                stack.me + 1,
                stack.nodes.len()
            ));
        }
        tip
    }

    /// What the header shows from chip `from` on, grouped with the available Attach offers.
    pub(super) fn attachment_rows(
        &self,
        ws: &Workspace,
        from: usize,
        cx: &App,
    ) -> Vec<(Option<&'static str>, Vec<ListRow>)> {
        let theme = cx.theme();
        let rows = self.repo_rows(ws);
        let past_repos = from.saturating_sub(rows.len());
        let repos = rows.into_iter().skip(from).map(|r| {
            let branch = ws
                .repos
                .iter()
                .find(|repo| repo.name == r.repo)
                .filter(|_| r.standing == Standing::Ready)
                .map(|repo| {
                    menus::repo_branch(self, ws, repo, cx).unwrap_or_else(|| "Detached HEAD".into())
                });
            let label = format!("{} · {}", r.repo, branch.as_deref().unwrap_or(r.state));
            (label, r.standing.mark(theme), Pick::Repo(r.repo))
        });
        let issues = (ws.linear.ordered()).skip(past_repos).map(|r| {
            let (label, icon) = self.issue_row(ws, r, theme);
            (label, icon, Pick::Issue(r.clone()))
        });
        let muted = theme.muted_foreground;
        let offer =
            |label: &str, icon, pick| (label.into(), Icon::new(icon).text_color(muted), pick);
        let repository = (from == 0 && ws.attaching.is_empty())
            .then(|| offer("Attach repository…", IconName::FolderPlus, Pick::Attach));
        let issue = (from == 0 && self.linear_in_use())
            .then(|| offer("Attach Linear issue…", IconName::TagPlus, Pick::AttachIssue));
        let groups = [
            (Some("Repositories"), repos.collect::<Vec<_>>()),
            (Some("Linear issues"), issues.collect()),
            (None, repository.into_iter().chain(issue).collect()),
        ];
        (groups.into_iter())
            .filter(|(_, rows)| !rows.is_empty())
            .collect()
    }

    /// Show attachments, and `+N` from the first chip it hides.
    pub(super) fn attachment_list(
        &mut self,
        name: String,
        from: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.ws(&name) else { return };
        let mut picks = vec![];
        let groups = (self.attachment_rows(ws, from, cx).into_iter())
            .map(|(heading, rows)| {
                let items = rows.into_iter().map(|(label, icon, pick)| {
                    picks.push(pick);
                    // `Command` paints an item's own icon muted, and the mark is the status.
                    let text = SharedString::from(label);
                    CommandItem::new().label(text.clone()).child(move |_, _| {
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .child(icon.clone().size_4().flex_none())
                            .child(div().flex_1().min_w_0().truncate().child(text.clone()))
                    })
                });
                (heading, items.collect())
            })
            .collect();
        open_grouped(
            cx.entity().downgrade(),
            "Attachments…",
            groups,
            0,
            move |s, row, w, cx| match &picks[row] {
                Pick::Repo(repo) => s.repository_menu(name.clone(), repo.clone(), None, w, cx),
                Pick::Issue(r) => s.issue_details(name.clone(), r.clone(), w, cx),
                Pick::Attach => s.attach_dialog(name.clone(), vec![], w, cx),
                Pick::AttachIssue => s.attach_linear_issue(name.clone(), w, cx),
            },
            window,
            cx,
        );
    }

    /// A chip in the header's attachment row.
    pub(super) fn chip(
        id: impl Into<ElementId>,
        icon: impl Into<Icon>,
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
            .child(icon.into().with_size(px(12.)))
            .child(label)
    }

    /// The header's attachments: a chip per repository, what needs the user first, then one per
    /// Linear issue, then Attach, a menu of the two kinds with Linear in use and the repository
    /// dialog without. `+N` lists what doesn't fit; Show attachments in cmd-k lists all of it, and
    /// Attach repository does whenever an attachment needs anything.
    pub(super) fn render_attachments(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(ws) = self.active_ws() else {
            return div().into_any_element();
        };
        let name = ws.name.clone();
        let rows = self.repo_rows(ws);
        let failed = rows
            .iter()
            .filter(|r| r.standing == Standing::Failed)
            .count();
        let mut chips: Vec<AnyElement> = (rows.into_iter())
            .map(|r| {
                let ready = r.standing == Standing::Ready;
                let (n, repo) = (name.clone(), r.repo.clone());
                if !ready {
                    return Self::chip(
                        SharedString::from(format!("attach-chip-{}", r.repo)),
                        r.standing.mark(cx.theme()),
                        r.repo.clone(),
                        cx.theme(),
                    )
                    .aria_label(format!("{}: {}", r.repo, r.state))
                    .tooltip(move |w, cx| Tooltip::new(r.tip.clone()).build(w, cx))
                    .cursor_pointer()
                    .on_click(cx.listener(move |s, _, w, cx| {
                        s.attachment_menu(n.clone(), repo.clone(), w, cx)
                    }))
                    .test_support()
                    .into_any_element();
                }
                let attached = ws.repos.iter().find(|repo| repo.name == r.repo).unwrap();
                let branch = menus::repo_branch(self, ws, attached, cx)
                    .unwrap_or_else(|| "Detached HEAD".into());
                let tip = self.repository_tip(ws, attached, &branch, cx);
                let (right_name, right_repo) = (name.clone(), r.repo.clone());
                Button::new(SharedString::from(format!("repo-chip-{}", r.repo)))
                    .ghost()
                    .xsmall()
                    .h(px(22.))
                    .max_w(px(280.))
                    .px_1p5()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted)
                    .icon(r.standing.mark(cx.theme()))
                    .label(format!("{} · {branch}", r.repo))
                    .dropdown_caret(true)
                    .accessibility_label(format!(
                        "{}: ready · {}",
                        r.repo,
                        tip.split_once('\n').unwrap().1
                    ))
                    .tooltip(tip)
                    .on_click(cx.listener(move |s, e: &ClickEvent, w, cx| {
                        s.repository_menu(n.clone(), repo.clone(), Some(e.position()), w, cx)
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |s, e: &MouseDownEvent, w, cx| {
                            s.repository_menu(
                                right_name.clone(),
                                right_repo.clone(),
                                Some(e.position),
                                w,
                                cx,
                            )
                        }),
                    )
                    .into_any_element()
            })
            .collect();
        chips.extend(self.issue_chips(ws, cx));
        AttachRow {
            name,
            chips,
            failed,
            disconnected: self.disconnected_chip(ws, cx),
            linear: self.linear_in_use(),
            shark: cx.entity().downgrade(),
        }
        .into_any_element()
    }
}

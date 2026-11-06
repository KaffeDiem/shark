//! ⇧C: the send sheet (A·03). Pending threads as a checklist, an overall note, an optional
//! PR review; what is ticked goes to the chat as one message, the rest stays pending.
use super::review_comment::{Comment, Status, prompt};
use super::review_share::{github_body, unposted_by_repo};
use super::*;
use crate::chat::{ReviewCard, ReviewItem, effort_label, model_entries, model_label};
use crate::models::effort_of;
use crate::state::ChatMeta;
use crate::ui::plural;
use crate::{CycleTarget, PickModel};
use gpui_kit::component::Disableable as _;
use gpui_kit::component::input::{Enter, Escape};
use gpui_kit::component::radio::{Radio, RadioGroup};

/// Where a review goes: a chat of the workspace, or a new one on the picked model and
/// thinking level (only `model` and `effort` of the meta are read).
#[derive(Clone)]
pub(crate) enum Target {
    Chat(String),
    New(ChatMeta),
}

#[derive(Clone)]
pub(crate) struct ReviewRequest {
    pub workspace: String,
    pub target: Target,
    pub card: ReviewCard,
    comments: Vec<Comment>,
    note: String,
    publish: bool,
}

pub(crate) struct Sheet {
    /// The pending threads when it opened, ticked or not.
    pub(crate) rows: Vec<(u64, bool)>,
    sel: usize,
    /// Where it can go: `(chat id, label)`; `None` is a new chat. The shown chat first.
    pub(crate) targets: Vec<(Option<String>, String)>,
    pub(crate) target: usize,
    /// The new chat's model and thinking level; starts as the shown chat's.
    pub(crate) pick: ChatMeta,
    pub(crate) note: Entity<InputState>,
    /// Also post the ticked comments to the PR as a Comment review.
    pub(crate) publish: bool,
    pub(crate) preview: bool,
    pub(crate) focus: FocusHandle,
}

impl Differ {
    /// `shift-c`, the bar's Send review…: the sheet over the pending threads.
    pub(crate) fn open_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.keep_draft(window, cx); // the words just typed are in the review
        let rows: Vec<(u64, bool)> = (self.ws_comments().iter())
            .filter(|c| c.status == Status::Pending)
            .map(|c| (c.id, true))
            .collect();
        if rows.is_empty() {
            return notify_err(window, cx, "No pending comments (c on a diff line)");
        }
        let note = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Overall note (optional) — e.g. “Run cargo test after fixing.”")
        });
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let ws = self.active_ws();
        let cur = ws.and_then(|ws| ws.chats.get(ws.active_chat));
        let title = |c: &ChatMeta| match c.title.is_empty() {
            true => "Untitled chat".to_string(),
            false => c.title.clone(),
        };
        let targets = (cur
            .map(|c| (Some(c.id.clone()), "This chat".to_string()))
            .into_iter())
        .chain([(None, "New chat".to_string())])
        .chain(
            (ws.map(|ws| &ws.chats[..]).unwrap_or_default().iter())
                .filter(|c| Some(*c) != cur)
                .map(|c| (Some(c.id.clone()), title(c))),
        )
        .collect();
        let pick = ChatMeta {
            model: cur.and_then(|c| c.model.clone()),
            effort: cur.and_then(|c| c.effort.clone()),
            ..Default::default()
        };
        self.review.sheet = Some(Sheet {
            rows,
            sel: 0,
            targets,
            target: 0,
            pick,
            note,
            publish: false,
            preview: false,
            focus,
        });
        cx.notify();
    }

    pub(crate) fn close_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.review.sheet = None;
        self.focus_diff(window, cx);
        cx.notify();
    }

    /// `space`: tick or untick the row under the cursor.
    pub(crate) fn sheet_toggle(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = &mut self.review.sheet
            && let Some(r) = s.rows.get_mut(s.sel)
        {
            r.1 = !r.1;
            cx.notify();
        }
    }

    pub(crate) fn sheet_move(&mut self, d: isize, cx: &mut Context<Self>) {
        if let Some(s) = &mut self.review.sheet {
            s.sel = s.sel.saturating_add_signed(d).min(s.rows.len() - 1);
            cx.notify();
        }
    }

    /// `enter`: the row's thread in the diff.
    pub(crate) fn sheet_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = (self.review.sheet.as_ref())
            .and_then(|s| s.rows.get(s.sel))
            .map(|r| r.0);
        self.close_sheet(window, cx);
        if let Some(id) = id {
            self.jump_to_comment(id, window, cx);
        }
    }

    /// `tab`: the next target, round and round.
    pub(crate) fn sheet_cycle_target(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = &mut self.review.sheet {
            s.target = (s.target + 1) % s.targets.len();
            cx.notify();
        }
    }

    /// `cmd-/`, the model chip: the new chat's model and thinking level, picked into the
    /// sheet (the chat gets them when it is made).
    pub(crate) fn sheet_model_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = &self.review.sheet else { return };
        let set = |f: fn(&mut ChatMeta, &'static str)| {
            let d = cx.entity().downgrade();
            move |v: &'static str, cx: &mut App| {
                d.update(cx, |d, cx| {
                    if let Some(s) = &mut d.review.sheet {
                        f(&mut s.pick, v);
                    }
                    cx.notify();
                })
                .ok();
            }
        };
        let (entries, sel) = model_entries(
            &s.pick,
            set(|m, v| m.model = Some(v.into())),
            set(|m, v| m.effort = Some(v.into())),
        );
        let m = menu::open(entries, window.mouse_position(), window, cx);
        m.update(cx, |m, _| m.select(sel));
        menu::hold(self, m, |s| &mut s.menu, cx);
    }

    /// `cmd-enter`: sends the ticked rows where the sheet points.
    pub(crate) fn sheet_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = &self.review.sheet else { return };
        let ids: Vec<u64> = s.rows.iter().filter(|r| r.1).map(|r| r.0).collect();
        let note = s.note.read(cx).value().to_string();
        // Also publish shows only while a ticked row's repo has a PR.
        let publish = s.publish && !self.sheet_prs().is_empty();
        let target = match s.targets.get(s.target).and_then(|t| t.0.clone()) {
            Some(id) => Target::Chat(id),
            None => Target::New(s.pick.clone()),
        };
        self.send_review_to(target, &ids, &note, publish, window, cx);
    }

    /// Pending comments `ids` as one message in the shown chat (queued while it works); they
    /// become Sent there. With `publish`, also a Comment review on the PR.
    pub(crate) fn send_review(
        &mut self,
        ids: &[u64],
        note: &str,
        publish: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = (self.active_ws()).and_then(|ws| ws.chats.get(ws.active_chat)) else {
            return notify_err(window, cx, "Open a workspace first");
        };
        let target = Target::Chat(id.id.clone());
        self.send_review_to(target, ids, note, publish, window, cx);
    }

    /// `send_review` to `target`.
    pub(crate) fn send_review_to(
        &mut self,
        target: Target,
        ids: &[u64],
        note: &str,
        publish: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.active_ws() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        let workspace = ws.name.clone();
        self.reanchor_comments(); // the lines as the agent will read them
        let comments: Vec<Comment> = (self.ws_comments().iter())
            .filter(|c| c.status == Status::Pending && ids.contains(&c.id))
            .cloned()
            .collect();
        if comments.is_empty() {
            return notify_err(window, cx, "Tick a comment to send (space)");
        }
        if let Some(c) = comments.iter().find(|c| self.unsaved_edit(c)) {
            let msg = format!(
                "Save {} first (⌘S): the agent reads the file on disk",
                c.path
            );
            return notify_err(window, cx, msg);
        }
        let card = ReviewCard {
            prompt: prompt(&comments, note),
            items: (comments.iter())
                .map(|c| ReviewItem {
                    id: c.id,
                    place: c.card_place(),
                    text: c.text.clone(),
                })
                .collect(),
            // "3 files · +5 −3"
            changes: {
                let ((_, a, d), n) = (self.changes_stats(), self.diff_files.len());
                format!("{n} file{} · +{a} −{d}", plural(n))
            },
        };
        cx.emit(DifferEvent::SendReview(ReviewRequest {
            workspace,
            target,
            card,
            comments,
            note: note.into(),
            publish,
        }));
    }

    pub(crate) fn review_request_valid(&self, request: &ReviewRequest) -> bool {
        self.workspace_name() == Some(request.workspace.as_str())
            && request
                .comments
                .iter()
                .all(|c| self.comment(c.id) == Some(c) && !self.unsaved_edit(c))
    }

    /// The host calls this synchronously after chat `chat_id` accepts, before queued Chat
    /// events run; `queued` says it waits behind the chat's turn.
    pub(crate) fn review_accepted(
        &mut self,
        request: &ReviewRequest,
        chat_id: &str,
        queued: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ReviewRequest {
            comments,
            note,
            publish,
            ..
        } = request;
        // Queued until the chat's ReviewSent (right after this, or when its turn ends).
        let ids: Vec<u64> = comments.iter().map(|c| c.id).collect();
        self.edit_comments(cx, |l| {
            for c in l.iter_mut().filter(|c| ids.contains(&c.id)) {
                c.status = Status::Queued;
                c.chat = Some(chat_id.to_string());
            }
        });
        let n = comments.len();
        let s = plural(n);
        let verb = if queued { "Queued" } else { "Sent" };
        window.push_notification(format!("{verb} {n} comment{s} for the agent"), cx);
        self.close_sheet(window, cx);
        if *publish {
            self.publish(comments, note, true, window, cx);
        }
    }

    /// The repo and PR number of each repo of the ticked rows with an open PR: where Also
    /// publish posts.
    pub(crate) fn sheet_prs(&self) -> Vec<(String, u64)> {
        let Some(s) = &self.review.sheet else {
            return vec![];
        };
        let mut out: Vec<(String, u64)> = vec![];
        for c in (s.rows.iter().filter(|r| r.1)).filter_map(|r| self.comment(r.0)) {
            let repo = c.repo();
            if !out.iter().any(|p| p.0 == repo)
                && let Some((pr, _)) = self.pr_of(c)
            {
                out.push((repo.to_string(), pr.number));
            }
        }
        out
    }

    /// What the sheet sends, as the preview shows it: the prompt, then with Also publish one
    /// review per repo, without the threads already on GitHub, as `publish` posts.
    fn sheet_preview(&self, cx: &App) -> Option<String> {
        let s = self.review.sheet.as_ref().filter(|s| s.preview)?;
        let picked: Vec<Comment> = (s.rows.iter())
            .filter(|r| r.1)
            .filter_map(|r| self.comment(r.0).cloned())
            .collect();
        let note = s.note.read(cx).value();
        let mut p = prompt(&picked, &note);
        let groups = unposted_by_repo(&picked);
        for (repo, n) in self.sheet_prs().iter().filter(|_| s.publish) {
            let group = (groups.iter()).find(|g| g[0].repo() == repo);
            let body = github_body(group.map_or(&[][..], |g| &g[..]), &note);
            p += &format!("\n— {repo} PR #{n} Comment review —\n\n{body}");
        }
        Some(p)
    }

    /// Ticks Also publish; the preview opens, so what GitHub gets is seen first.
    pub(crate) fn sheet_publish(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(s) = &mut self.review.sheet {
            s.publish = on;
            s.preview |= on;
        }
        cx.notify();
    }

    /// The sheet, over the bottom right of the diff.
    pub(crate) fn render_sheet(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let s = self.review.sheet.as_ref()?;
        let theme = cx.theme();
        let comments = self.ws_comments();
        let files: HashSet<&str> = (s.rows.iter())
            .filter_map(|r| Some(comments.iter().find(|c| c.id == r.0)?.path.as_str()))
            .collect();
        let ticked = s.rows.iter().filter(|r| r.1).count();
        let focused = s.focus.is_focused(window);
        let prs = self.sheet_prs();
        // One PR as the design says it; several by repo.
        let to = match &prs[..] {
            [] => None,
            [(_, n)] => Some(format!("PR #{n}")),
            more => Some(
                (more.iter().map(|(repo, n)| format!("{repo} #{n}")))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        };
        let rows = s.rows.iter().enumerate().map(|(ix, &(id, on))| {
            let c = comments.iter().find(|c| c.id == id);
            let name = c.map_or(String::new(), |c| {
                let name = c.path.rsplit('/').next().unwrap_or(&c.path);
                format!("{name}:{}", c.lines().replace('–', "-"))
            });
            let text = c.map_or(String::new(), |c| {
                let first = c.text.lines().next().unwrap_or_default();
                match c.suggestion {
                    Some(_) => format!("Suggested change · {first}"),
                    None => first.to_string(),
                }
            });
            h_flex()
                .id(("sheet-row", ix))
                .h(px(36.))
                .px_2()
                .gap_3()
                .rounded(theme.radius)
                .when(ix == s.sel && focused, |d| d.bg(theme.list_active))
                .hover(|d| d.bg(theme.list_hover))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(s) = &mut this.review.sheet {
                        s.sel = ix;
                    }
                    this.sheet_toggle(cx);
                }))
                .child(Checkbox::new(("sheet-tick", ix)).small().checked(on))
                .child(
                    div()
                        .w(px(140.))
                        .flex_none()
                        .truncate()
                        .font_family(theme.mono_font_family.clone())
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(name),
                )
                .child(div().flex_1().min_w_0().truncate().child(text))
        });
        let preview = self.sheet_preview(cx);
        let unpushed = s.publish
            && (s.rows.iter().filter(|r| r.1))
                .filter_map(|r| self.comment(r.0))
                .any(|c| self.pr_of(c).is_some_and(|(_, pushed)| !pushed));
        let n = s.rows.len();
        let new_chat = s.targets.get(s.target).is_some_and(|t| t.0.is_none());
        let sheet = v_flex()
            .id("review-sheet")
            .test_support()
            .w(px(500.))
            .max_w_full()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .text_sm()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            // cmd-/ anywhere in the sheet (the note too) picks the new chat's model, not the
            // shown chat's.
            .on_action(cx.listener(|this, _: &PickModel, w, cx| {
                if this.review.sheet.as_ref().is_some_and(|s| s.targets[s.target].0.is_none()) {
                    this.sheet_model_menu(w, cx)
                }
            }))
            // In the note: cmd-enter sends and Esc closes, as everywhere in the sheet.
            .capture_action(cx.listener(|this, a: &Enter, w, cx| {
                if a.secondary {
                    cx.stop_propagation();
                    this.sheet_send(w, cx);
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, w, cx| {
                cx.stop_propagation();
                this.close_sheet(w, cx);
            }))
            .child(
                h_flex()
                    .px_4()
                    .pt_3()
                    .pb_2()
                    .gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Send review"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{n} comment{} · {} file{}",
                                plural(n),
                                files.len(),
                                plural(files.len())
                            )),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(faint(theme))
                            .child("space toggle · ↵ go to thread · ⇥ target"),
                    ),
            )
            // The keys live on the list, so the note below types j, k and space.
            .child(
                v_flex()
                    .id("sheet-rows")
                    .key_context("ReviewSheet")
                    .track_focus(&s.focus)
                    .px_2()
                    .pb_2()
                    .on_action(cx.listener(|this, _: &ToggleRow, _, cx| this.sheet_toggle(cx)))
                    .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.sheet_move(1, cx)))
                    .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.sheet_move(-1, cx)))
                    .on_action(cx.listener(|this, _: &Confirm, w, cx| this.sheet_open(w, cx)))
                    .on_action(cx.listener(|this, _: &CancelEdit, w, cx| this.close_sheet(w, cx)))
                    .on_action(cx.listener(|this, _: &SubmitForm, w, cx| this.sheet_send(w, cx)))
                    .on_action(cx.listener(|this, _: &CycleTarget, _, cx| this.sheet_cycle_target(cx)))
                    .children(rows),
            )
            .child(
                v_flex()
                    .px_4()
                    .py_3()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(Input::new(&s.note))
                    .child(
                        h_flex()
                            .gap_3()
                            .items_start()
                            .child(
                                RadioGroup::vertical("sheet-target")
                                    .children((s.targets.iter().enumerate()).map(|(ix, t)| {
                                        Radio::new(("sheet-target", ix)).label(t.1.clone())
                                    }))
                                    .selected_index(Some(s.target))
                                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                        if let Some(s) = &mut this.review.sheet {
                                            s.target = *ix;
                                        }
                                        cx.notify();
                                    })),
                            )
                            .when(new_chat, |d| {
                                d.child(
                                    Button::new("sheet-model")
                                        .outline()
                                        .xsmall()
                                        .label(format!(
                                            "{} · {} ⌘/",
                                            model_label(s.pick.model()),
                                            effort_label(effort_of(&s.pick))
                                        ))
                                        .on_click(cx.listener(|this, _, w, cx| {
                                            this.sheet_model_menu(w, cx)
                                        })),
                                )
                            }),
                    )
                    .when_some(to.clone(), |d, to| {
                        d.child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Checkbox::new("sheet-publish")
                                        .small()
                                        .checked(s.publish)
                                        .label(format!("Also publish to {to} as a Comment review"))
                                        .on_click(cx.listener(|this, on: &bool, _, cx| {
                                            this.sheet_publish(*on, cx)
                                        })),
                                )
                        )
                    })
                    .when(unpushed, |d| {
                        d.child(div().text_xs().text_color(theme.warning_foreground).child(
                            "Some lines are not pushed yet. GitHub will receive the file path and code excerpt, without an inline anchor.",
                        ))
                    }),
            )
            .when_some(preview, |d, p| {
                d.child(
                    div()
                        .id("sheet-preview")
                        .max_h(px(220.))
                        .overflow_y_scroll()
                        .px_4()
                        .py_2()
                        .border_t_1()
                        .border_color(theme.border)
                        .bg(theme.muted)
                        .font_family(theme.mono_font_family.clone())
                        .text_xs()
                        .whitespace_normal()
                        .child(p),
                )
            })
            .child(
                h_flex()
                    .px_4()
                    .py_2()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        Button::new("sheet-preview-toggle")
                            .ghost()
                            .small()
                            .label(if s.preview {
                                "Hide prompt"
                            } else {
                                "Preview prompt"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(s) = &mut this.review.sheet {
                                    s.preview = !s.preview;
                                }
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("sheet-esc")
                            .ghost()
                            .small()
                            .label("Cancel esc")
                            .on_click(cx.listener(|this, _, w, cx| this.close_sheet(w, cx))),
                    )
                    .child(
                        Button::new("sheet-send")
                            .primary()
                            .small()
                            .disabled(ticked == 0)
                            .label(format!(
                                "Send {ticked} comment{} ⌘↵",
                                plural(ticked)
                            ))
                            .on_click(cx.listener(|this, _, w, cx| this.sheet_send(w, cx))),
                    ),
            );
        // The diff dims behind it; a click there closes it.
        Some(
            h_flex()
                .id("review-sheet-backdrop")
                .absolute()
                .inset_0()
                .px_4()
                .pb(px(48.))
                .items_end()
                .justify_end()
                .bg(theme.overlay)
                .occlude() // clicks and scrolls stop here, not in the diff
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, w, cx| this.close_sheet(w, cx)),
                )
                .child(sheet)
                .into_any_element(),
        )
    }
}

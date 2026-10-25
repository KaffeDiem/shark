//! The composer's `@` file and `/` command completion menu.
use super::*;
use crate::complete::Kind;
use gpui_kit::component::input::InlineToken;

/// The open `@`/`/` completion menu.
pub(super) struct Menu {
    /// Bytes of the `@query` in the composer that a pick replaces.
    range: std::ops::Range<usize>,
    command: bool,
    pub(super) matches: Vec<usize>,
    pub(super) sel: usize,
}

const MENU_ROWS: usize = 50;

impl Chat {
    fn source(&self, command: bool) -> &[Item] {
        if command { &self.commands } else { &self.files }
    }

    pub(super) fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// Opens, filters or closes the menu from the text before the cursor.
    pub(super) fn update_menu(&mut self, cx: &mut Context<Self>) {
        // Normal-mode edits change the text too, but only Insert completes.
        if self.vim_on && self.vim.mode != crate::vim::Mode::Insert {
            return self.close_menu(cx);
        }
        let c = self.composer.read(cx);
        let (text, cursor) = (c.value(), c.cursor());
        let found = complete::trigger(&text, cursor).filter(|(r, _)| {
            // Text right after a chip belongs to the chip, not to a new query.
            !c.tokens()
                .iter()
                .any(|t| t.range().start < r.end && r.start < t.range().end)
        });
        let Some((range, command)) = found else {
            return self.close_menu(cx);
        };
        let opening = self.menu.as_ref().is_none_or(|m| m.command != command);
        if opening {
            // Sources may have changed since the last menu; refresh behind the cached list.
            match command {
                true => self.reprobe(cx),
                false => self.reindex(cx),
            }
        }
        let query = &text[range.start + 1..range.end];
        let matches = complete::rank(self.source(command), query, MENU_ROWS);
        let sel = match &self.menu {
            Some(m) if !opening && m.matches == matches => m.sel,
            _ => 0,
        };
        self.menu = Some(Menu {
            range,
            command,
            matches,
            sel,
        });
        self.menu_scroll.scroll_to_item(sel);
        cx.notify();
    }

    fn menu_item(&self, ix: usize) -> Option<&Item> {
        let m = self.menu.as_ref()?;
        self.source(m.command).get(*m.matches.get(ix)?)
    }

    pub(super) fn move_menu(&mut self, d: isize, cx: &mut Context<Self>) {
        if let Some(m) = &mut self.menu
            && !m.matches.is_empty()
        {
            m.sel = (m.sel as isize + d).rem_euclid(m.matches.len() as isize) as usize;
            self.menu_scroll.scroll_to_item(m.sel);
            cx.notify();
        }
    }

    /// Picks the highlighted match; false when the menu is closed or has none.
    pub(super) fn pick_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(sel) = (self.menu.as_ref())
            .filter(|m| !m.matches.is_empty())
            .map(|m| m.sel)
        else {
            return false;
        };
        self.pick(sel, window, cx);
        true
    }

    /// Replaces the query with the picked item as a chip, plus a space to keep typing.
    fn pick(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.menu_item(ix).cloned() else {
            return self.close_menu(cx);
        };
        let range = self.menu.take().unwrap().range;
        // Two repos can both have README.md; the chip names the repo too.
        let label = match item.kind {
            Kind::File | Kind::Dir if self.ws.repos.len() > 1 && !item.detail.is_empty() => {
                format!("{} · {}", item.label, item.badge)
            }
            _ => item.label.clone(),
        };
        self.composer
            .update(cx, |c, cx| c.set_selected_range(range, cx));
        self.insert_mention(item.insert, label, window, cx);
    }

    /// Puts `@path` in place of the selection as a chip, plus a space to keep typing.
    pub(super) fn insert_mention(
        &mut self,
        text: String,
        label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let token = InlineToken::new(text.clone(), text.clone()).with_label(label);
        self.composer.update(cx, |c, cx| {
            if c.replace_with_token(token, window, cx).is_err() {
                // ponytail: tokens reject control characters; such a path goes in as plain text
                c.replace(text, window, cx);
            }
            c.insert(" ", window, cx);
        });
        cx.notify();
    }

    /// Opens upward over the transcript, like Slack's: the composer sits at the bottom.
    pub(super) fn render_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.menu.as_ref()?;
        let theme = cx.theme();
        let faint = crate::app::faint(theme);
        let multi = self.ws.repos.len() > 1;
        let rows: Vec<AnyElement> = (0..m.matches.len())
            .filter_map(|ix| {
                let it = self.menu_item(ix)?;
                let icon = match it.kind {
                    Kind::File => IconName::File,
                    Kind::Dir => IconName::Folder,
                    Kind::Command => IconName::Slash,
                };
                let badge = match it.kind {
                    Kind::Command => (!it.badge.is_empty()).then(|| {
                        div()
                            .flex_none()
                            .max_w(px(200.))
                            .truncate()
                            .text_color(faint)
                            .child(it.badge.clone())
                    }),
                    // The repo, so two README.md rows are told apart at a glance; a repo's own
                    // row (`api/`) already names it.
                    _ => (multi && !it.detail.is_empty()).then(|| {
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .text_color(theme.muted_foreground)
                            .child(Icon::new(IconName::FolderGit2).with_size(px(12.)))
                            .child(it.badge.clone())
                    }),
                };
                // The badge names the repo, so the folder starts inside it.
                let detail = match it.kind {
                    Kind::File | Kind::Dir if multi => {
                        let d = it.detail.strip_prefix(it.badge.as_str()).unwrap_or(&it.detail);
                        d.trim_start_matches('/').to_string()
                    }
                    _ => it.detail.clone(),
                };
                Some(
                    h_flex()
                        .id(ix)
                        .h(px(28.))
                        .px_2()
                        .gap_2()
                        .rounded(px(4.))
                        .when(ix == m.sel, |d| d.bg(crate::menu::highlight(theme)))
                        // The mouse moves the one highlight, as in the other menus.
                        .on_mouse_move(cx.listener(move |this, _, _, cx| {
                            if let Some(m) = this.menu.as_mut().filter(|m| m.sel != ix) {
                                m.sel = ix;
                                cx.notify();
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                this.pick(ix, window, cx);
                                window.prevent_default(); // keep focus in the composer
                            }),
                        )
                        .child(
                            Icon::new(icon)
                                .with_size(px(14.))
                                .text_color(theme.muted_foreground),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.foreground)
                                .child(it.label.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(detail),
                        )
                        .children(badge.map(|b| b.text_xs()))
                        .into_any_element(),
                )
            })
            .collect();
        let empty = match (m.command, rows.is_empty()) {
            (_, false) => None,
            (true, _) if self.probing.is_some() && self.commands.is_empty() => {
                Some("Loading skills…")
            }
            (false, _) if self.indexing.is_some() && self.files.is_empty() => {
                Some("Indexing files…")
            }
            (true, _) => Some("No matching skills or commands"),
            (false, _) => Some("No matching files"),
        };
        Some(
            crate::menu::chrome(theme)
                .id("completions")
                .absolute()
                .left_0()
                .right_0()
                .bottom_full()
                .mb_1()
                .occlude()
                .child(
                    div()
                        .id("completion-rows")
                        .max_h(px(8. * 28.))
                        .overflow_y_scroll()
                        .track_scroll(&self.menu_scroll)
                        .children(rows)
                        .when_some(empty, |d, e| {
                            d.child(
                                div()
                                    .h(px(28.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(e),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .px_2()
                        .pt_1()
                        .gap_3()
                        .text_xs()
                        .text_color(faint)
                        .child(hint("up down", "navigate"))
                        .child(hint("enter", "add"))
                        .child(hint("escape", "close"))
                        .when(!m.command && multi, |d| {
                            d.child(div().flex_1())
                                .child("Type repo/ to search one repo")
                        }),
                )
                .into_any_element(),
        )
    }
}

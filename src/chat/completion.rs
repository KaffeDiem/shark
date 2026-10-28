//! The composer's `@` file, `/` command and `#` PR completion menu.
use super::*;
use crate::complete::Kind;
use gpui_kit::component::input::InlineToken;

/// The open `@`/`/`/`#` completion menu.
pub(super) struct Menu {
    /// Bytes of the `@query` in the composer that a pick replaces.
    range: std::ops::Range<usize>,
    /// The trigger character: `@`, `/` or `#`.
    src: char,
    pub(super) matches: Vec<usize>,
    pub(super) sel: usize,
    /// Whether enter picks the highlighted row. A `#` query that only fuzzily matches a PR
    /// (`#endif`) leaves it off, so enter sends the text; moving the highlight turns it on.
    pub(super) armed: bool,
}

const MENU_ROWS: usize = 50;

impl Chat {
    fn source(&self, src: char) -> &[Item] {
        match src {
            '/' => &self.commands,
            '#' => &self.pr_items,
            _ => &self.files,
        }
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
        let Some((range, src)) = found else {
            return self.close_menu(cx);
        };
        let opening = self.menu.as_ref().is_none_or(|m| m.src != src);
        if opening {
            // Sources may have changed since the last menu; refresh behind the cached list.
            match src {
                '/' => self.reprobe(cx),
                '#' => self.refetch_prs(cx),
                _ => self.reindex(cx),
            }
        }
        let query = &text[range.start + 1..range.end];
        let items = self.source(src);
        let matches = match src {
            '#' => complete::rank_prs(items, query, MENU_ROWS),
            _ => complete::rank(items, query, MENU_ROWS),
        };
        let (sel, armed) = match &self.menu {
            Some(m) if !opening && m.matches == matches => (m.sel, m.armed),
            _ => (0, false),
        };
        let armed = src != '#'
            || armed
            || (matches.first()).is_some_and(|&i| complete::names_pr(&items[i], query));
        self.menu = Some(Menu {
            range,
            src,
            matches,
            sel,
            armed,
        });
        self.menu_scroll.scroll_to_item(sel);
        cx.notify();
    }

    fn menu_item(&self, ix: usize) -> Option<&Item> {
        let m = self.menu.as_ref()?;
        self.source(m.src).get(*m.matches.get(ix)?)
    }

    pub(super) fn move_menu(&mut self, d: isize, cx: &mut Context<Self>) {
        if let Some(m) = &mut self.menu
            && !m.matches.is_empty()
        {
            m.sel = (m.sel as isize + d).rem_euclid(m.matches.len() as isize) as usize;
            m.armed = true;
            self.menu_scroll.scroll_to_item(m.sel);
            cx.notify();
        }
    }

    /// Picks the highlighted match; false when the menu is closed, has none, or isn't armed.
    pub(super) fn pick_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(sel) = (self.menu.as_ref())
            .filter(|m| m.armed && !m.matches.is_empty())
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
            Kind::Pr(_) if self.ws.repos.len() > 1 => format!("{}{}", item.badge, item.label),
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
        let faint = crate::ui::faint(theme);
        let multi = self.ws.repos.len() > 1;
        let rows: Vec<AnyElement> = (0..m.matches.len())
            .filter_map(|ix| {
                let it = self.menu_item(ix)?;
                let icon = match it.kind {
                    Kind::Pr(status) => status.icon(theme),
                    kind => Icon::new(match kind {
                        Kind::Dir => IconName::Folder,
                        Kind::Command => IconName::Slash,
                        _ => IconName::File,
                    })
                    .text_color(theme.muted_foreground),
                };
                let file_icon = match it.kind {
                    Kind::File => crate::file_icon::file_icon(&it.label, theme.is_dark()),
                    Kind::Dir => crate::file_icon::folder_icon(&it.label, theme.is_dark()),
                    _ => None,
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
                        let d = it
                            .detail
                            .strip_prefix(it.badge.as_str())
                            .unwrap_or(&it.detail);
                        d.trim_start_matches('/').to_string()
                    }
                    _ => it.detail.clone(),
                };
                Some(
                    h_flex()
                        .id(ix)
                        .when_some(
                            match it.kind {
                                Kind::Pr(status) => Some(format!(
                                    "{} {} · {} pull request",
                                    it.label,
                                    it.detail,
                                    status.label()
                                )),
                                _ => None,
                            },
                            |d, label| d.aria_label(label),
                        )
                        .h(crate::menu::ROW_H)
                        .px_2()
                        .gap_2()
                        .rounded(px(4.))
                        .when(ix == m.sel && m.armed, |d| {
                            d.bg(crate::menu::highlight(theme))
                        })
                        // The mouse moves the one highlight, as in the other menus.
                        .on_mouse_move(cx.listener(move |this, _, _, cx| {
                            if let Some(m) =
                                (this.menu.as_mut()).filter(|m| m.sel != ix || !m.armed)
                            {
                                (m.sel, m.armed) = (ix, true);
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
                        .child(match file_icon {
                            // 16px matches a 14px glyph, as app icons in `menu.rs`.
                            Some(i) => img(i)
                                .size(px(16.))
                                .mx(px(-1.))
                                .flex_none()
                                .into_any_element(),
                            None => icon.with_size(px(14.)).into_any_element(),
                        })
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
        let loading_prs = self.prs_left > 0;
        let empty: Option<SharedString> = match (m.src, rows.is_empty()) {
            (_, false) => None,
            ('/', _) if self.probing.is_some() && self.commands.is_empty() => {
                Some("Loading skills…".into())
            }
            ('/', _) => Some("No matching skills or commands".into()),
            ('#', _) if self.pr_items.is_empty() && loading_prs => {
                Some("Loading pull requests…".into())
            }
            ('#', _) if self.pr_items.is_empty() && !self.prs_errs.is_empty() => {
                Some("Couldn't list pull requests".into())
            }
            ('#', _) if self.pr_items.is_empty() => Some("No pull requests".into()),
            ('#', _) => Some("No matching pull requests".into()),
            _ if self.indexing.is_some() && self.files.is_empty() => Some("Indexing files…".into()),
            _ => Some("No matching files".into()),
        };
        // `#` lists come from the network: say what is still loading and what failed.
        let n = self.ws.repos.len();
        let status = (m.src == '#').then(|| {
            let errs = self.prs_errs.iter().map(|(repo, e)| match n {
                1 => e.clone(),
                _ => format!("{repo}: {e}"),
            });
            let errs = errs.collect::<Vec<_>>().join(" · ");
            h_flex()
                .px_2()
                .pt_1()
                .gap_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .when(loading_prs, |d| {
                    d.child(Spinner::new().with_size(px(12.))).child(match n {
                        1 => "Refreshing pull requests…".to_string(),
                        _ => format!(
                            "Refreshing pull requests… {} of {n} repos",
                            n - self.prs_left
                        ),
                    })
                })
                .when(!errs.is_empty(), |d| {
                    d.child(Icon::new(IconName::TriangleAlert).with_size(px(12.)))
                        .child(div().min_w_0().truncate().child(errs))
                })
        });
        let status = status.filter(|_| loading_prs || !self.prs_errs.is_empty());
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
                        .max_h(crate::menu::ROW_H * 8.)
                        .overflow_y_scroll()
                        .track_scroll(&self.menu_scroll)
                        .children(rows)
                        .when_some(empty, |d, e| {
                            d.child(
                                div()
                                    .h(crate::menu::ROW_H)
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(e),
                            )
                        }),
                )
                .children(status)
                .child(
                    h_flex()
                        .px_2()
                        .pt_1()
                        .gap_3()
                        .text_xs()
                        .text_color(faint)
                        .child(hint("up down", "navigate"))
                        .child(hint("enter", if m.armed { "add" } else { "send" }))
                        .child(hint("escape", "close"))
                        .when(m.src != '/' && multi, |d| {
                            d.child(div().flex_1())
                                .child("Type repo/ to search one repo")
                        }),
                )
                .into_any_element(),
        )
    }
}

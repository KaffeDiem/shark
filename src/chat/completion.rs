//! The composer's `@` file, `/` command and `#` PR completion menu.
use super::*;
use crate::complete::Kind;
use gpui_kit::component::Theme;
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

/// Whether `it` names its repo (the badge, the chip): several repos, and not a command.
fn shows_repo(it: &Item, multi: bool) -> bool {
    multi && !matches!(it.kind, Kind::Command) && !it.detail.is_empty()
}

/// What a menu row shows besides its label.
struct RowParts {
    icon: Icon,
    file_icon: Option<std::sync::Arc<Image>>,
    badge: Option<Div>,
    detail: String,
    aria: Option<String>,
}

fn row_parts(it: &Item, multi: bool, theme: &Theme, faint: Hsla) -> RowParts {
    let plain = |name| Icon::new(name).text_color(theme.muted_foreground);
    // The repo, so two README.md rows are told apart at a glance; a repo's own row (`api/`)
    // already names it.
    let repo_badge = || {
        shows_repo(it, multi).then(|| {
            h_flex()
                .flex_none()
                .gap_1()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::FolderGit2).with_size(px(12.)))
                .child(it.badge.clone())
        })
    };
    match it.kind {
        Kind::Pr(status) => RowParts {
            icon: status.icon(theme),
            file_icon: None,
            badge: repo_badge(),
            detail: it.detail.clone(),
            aria: Some(format!(
                "{} {} · {} pull request",
                it.label,
                it.detail,
                status.label()
            )),
        },
        Kind::Command => RowParts {
            icon: plain(IconName::Slash),
            file_icon: None,
            badge: (!it.badge.is_empty()).then(|| {
                div()
                    .flex_none()
                    .max_w(px(200.))
                    .truncate()
                    .text_color(faint)
                    .child(it.badge.clone())
            }),
            detail: it.detail.clone(),
            aria: None,
        },
        Kind::File | Kind::Dir => {
            let dark = theme.is_dark();
            let (name, file_icon) = match it.kind {
                Kind::Dir => (
                    IconName::Folder,
                    crate::file_icon::folder_icon(&it.label, dark),
                ),
                _ => (IconName::File, crate::file_icon::file_icon(&it.label, dark)),
            };
            // The badge names the repo, so the folder starts inside it.
            let detail = match multi {
                true => (it
                    .detail
                    .strip_prefix(it.badge.as_str())
                    .unwrap_or(&it.detail))
                .trim_start_matches('/')
                .to_string(),
                false => it.detail.clone(),
            };
            RowParts {
                icon: plain(name),
                file_icon,
                badge: repo_badge(),
                detail,
                aria: None,
            }
        }
    }
}

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
        if self.vim_on && !self.vim.typing() {
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
        let multi = self.ws.repos.len() > 1;
        let label = match item.kind {
            Kind::File | Kind::Dir if shows_repo(&item, multi) => {
                format!("{} · {}", item.label, item.badge)
            }
            Kind::Pr(_) if shows_repo(&item, multi) => format!("{}{}", item.badge, item.label),
            _ => item.label.clone(),
        };
        self.composer
            .update(cx, |c, cx| c.set_selected_range(range, cx));
        self.insert_mention(item.insert, label, window, cx);
    }

    /// Puts `@path` in place of the selection as a chip, plus a space to keep typing.
    fn insert_mention(
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
                let RowParts {
                    icon,
                    file_icon,
                    badge,
                    detail,
                    aria,
                } = row_parts(it, multi, theme, faint);
                Some(
                    h_flex()
                        .id(ix)
                        .when_some(aria, |d, label| d.aria_label(label))
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

impl Chat {
    /// Re-lists files in the background; the old list serves until it lands.
    pub(super) fn reindex(&mut self, cx: &mut Context<Self>) {
        if self.indexing.is_some() {
            return;
        }
        let repos = self.ws.repo_paths();
        self.indexing = Some(cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move { complete::files(&repos) })
                .await;
            this.update(cx, |c, cx| {
                c.indexing = None;
                c.files = files;
                c.update_menu(cx);
            })
            .ok();
        }));
    }

    /// Asks claude for its current skills and commands in the background.
    pub(super) fn reprobe(&mut self, cx: &mut Context<Self>) {
        if self.probing.is_some() || cfg!(test) {
            return;
        }
        let (name, dir) = (self.ws.name.clone(), self.ws.dir());
        self.probing = Some(cx.spawn(async move |this, cx| {
            let cmds = cx
                .background_executor()
                .spawn(async move { complete::probe_commands(&name, &dir) })
                .await;
            this.update(cx, |c, cx| {
                c.probing = None;
                if let Some(cmds) = cmds {
                    c.commands = cmds;
                    c.update_menu(cx);
                }
            })
            .ok();
        }));
    }

    /// Re-lists every repo's PRs in the background, at most once a minute: every `# heading` opens
    /// the menu. Each repo's list replaces its cached rows as it lands, so the menu shows progress
    /// per repo, and a repo that fails keeps its cached rows.
    fn refetch_prs(&mut self, cx: &mut Context<Self>) {
        let fresh = (self.prs_at).is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(60));
        if self.fetching_prs.is_some() || fresh || cfg!(test) {
            return;
        }
        let repos: Vec<_> = (self.ws.repos.iter())
            .map(|r| (r.name.clone(), r.src.clone()))
            .collect();
        let n = repos.len();
        (self.prs_left, self.prs_errs) = (n, vec![]);
        let (tx, rx) = std::sync::mpsc::channel();
        let bg = cx.background_executor().clone();
        bg.spawn(async move {
            crate::proc::par_map(&repos, |(name, dir)| {
                let _ = tx.send((name.clone(), complete::fetch_repo_prs(name, dir, false)));
            });
        })
        .detach();
        self.fetching_prs = Some(cx.spawn(async move |this, cx| {
            let mut rx = rx;
            for _ in 0..n {
                let (got, back) = bg.spawn(async move { (rx.recv(), rx) }).await;
                rx = back;
                let Ok((repo, res)) = got else { break };
                this.update(cx, |c, cx| {
                    c.prs_left = c.prs_left.saturating_sub(1);
                    match res {
                        Ok(list) => {
                            c.prs.retain(|p| p.repo != repo);
                            c.prs.extend(list);
                            c.pr_items = complete::pr_items(&c.prs);
                        }
                        Err(e) => c.prs_errs.push((repo, e)),
                    }
                    c.update_menu(cx);
                })
                .ok();
            }
            this.update(cx, |c, cx| {
                c.fetching_prs = None;
                c.prs_left = 0;
                if c.prs_errs.is_empty() {
                    c.prs_at = Some(Instant::now());
                }
                // A repo that left the workspace takes its PRs with it.
                let repos = &c.ws.repos;
                c.prs.retain(|p| repos.iter().any(|r| r.name == p.repo));
                c.pr_items = complete::pr_items(&c.prs);
                c.update_menu(cx);
            })
            .ok();
        }));
    }

    /// Dropped or pasted files: copied into the workspace off the main thread, then mentioned at
    /// the cursor in drop order. Pasted image bytes come as `(bytes, ext)`.
    pub(super) fn attach(
        &mut self,
        paths: Vec<PathBuf>,
        image: Option<(Vec<u8>, &'static str)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dir = self.ws.dir();
        cx.spawn_in(window, async move |this, cx| {
            let done = cx
                .background_executor()
                .spawn(async move {
                    let mut out: Vec<_> = paths.iter().map(|p| complete::attach(&dir, p)).collect();
                    out.extend(image.map(|(b, ext)| {
                        complete::attach_bytes(&dir, &format!("Pasted image.{ext}"), &b)
                    }));
                    out
                })
                .await;
            this.update_in(cx, |c, window, cx| {
                // The user may have selected text while it copied; insert beside it, not over it.
                c.composer.update(cx, |c, cx| {
                    let at = c.cursor();
                    c.set_selected_range(at..at, cx)
                });
                for r in done {
                    match r {
                        Ok(p) => {
                            let label = Path::new(&p).file_name().unwrap_or_default();
                            let label = label.to_string_lossy().into_owned();
                            c.insert_mention(complete::mention(&p), label, window, cx);
                        }
                        Err(e) => window.push_notification(Notification::error(e), cx),
                    }
                }
                c.focus_composer(window, cx);
            })
            .ok();
        })
        .detach();
    }
}

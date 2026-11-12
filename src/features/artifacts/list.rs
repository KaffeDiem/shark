//! The list pane: grouping, ordering, and the rows drawn from them.
use super::preview::{Kind, kind};
use super::{Artifacts, reveal};
use crate::media::human_bytes;
use crate::media::prepare::native;
use crate::sync::compact_age;
use crate::ui::list_cursor_row;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::Input;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::HashSet;
use std::ops::Range;
use std::time::SystemTime;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    /// Path inside `.context`, `/`-separated.
    pub rel: String,
    /// The top-level folder (`plans`), empty for a loose file.
    pub group: String,
    pub kind: Kind,
    pub size: u64,
    pub modified: SystemTime,
}

impl Item {
    pub(crate) fn new(rel: String, size: u64, modified: SystemTime) -> Self {
        Self {
            group: rel.split_once('/').map_or("", |(d, _)| d).to_string(),
            kind: kind(&rel),
            rel,
            size,
            modified,
        }
    }
}

/// The pinned group's key. Dotted names are never scanned, so no folder has it.
pub(crate) const PINNED: &str = ".pinned";

pub(crate) enum Row {
    /// `count` is the group's unfiltered size.
    Group {
        key: String,
        count: usize,
        open: bool,
    },
    /// An index into the items.
    Item(usize),
}

/// A group's title: the folder, capitalized when Shark names it.
pub(crate) fn group_label(group: &str) -> String {
    match group {
        PINNED => "Pinned".into(),
        "" => "Other".into(),
        g => {
            let mut chars = g.chars();
            let first = chars.next().into_iter().flat_map(char::to_uppercase);
            first.chain(chars).collect()
        }
    }
}

/// Plans, Attachments, Scratch, other folders by name, then loose files.
pub(crate) fn arrange(items: &mut [Item]) {
    let rank = |g: &str| match g {
        "plans" => 0,
        "attachments" => 1,
        "scratch" => 2,
        "" => 4,
        _ => 3,
    };
    items.sort_by(|a, b| {
        (rank(&a.group).cmp(&rank(&b.group)))
            .then(a.group.cmp(&b.group))
            .then(b.modified.cmp(&a.modified))
            .then(a.rel.cmp(&b.rel))
    });
}

/// The rows to draw: headers and, unless folded, their items matching `filter` (substring of
/// the path, any case). Groups with no match are left out.
pub(crate) fn rows(
    items: &[Item],
    filter: &str,
    folded: &HashSet<String>,
    pins: &[String],
) -> Vec<Row> {
    let filter = filter.to_lowercase();
    let mut rows: Vec<Row> = vec![];
    let matches = |i: usize| items[i].rel.to_lowercase().contains(&filter);
    let push = |rows: &mut Vec<Row>, key: &str, count: usize, shown: Vec<usize>| {
        if !shown.is_empty() {
            let open = !folded.contains(key);
            rows.push(Row::Group {
                key: key.to_string(),
                count,
                open,
            });
            if open {
                rows.extend(shown.into_iter().map(Row::Item));
            }
        }
    };
    // Pins whose file is missing stay in the state, not in the list.
    let pinned: Vec<usize> = (pins.iter())
        .filter_map(|p| items.iter().position(|i| &i.rel == p))
        .collect();
    push(
        &mut rows,
        PINNED,
        pinned.len(),
        pinned.iter().copied().filter(|&i| matches(i)).collect(),
    );
    let mut start = 0;
    // `items` is arranged, so a group is one run.
    while start < items.len() {
        let group = &items[start].group;
        let end = start
            + items[start..]
                .iter()
                .take_while(|i| &i.group == group)
                .count();
        let unpinned: Vec<usize> = (start..end).filter(|i| !pinned.contains(i)).collect();
        let shown = (unpinned.iter().copied().filter(|&i| matches(i))).collect();
        push(&mut rows, group, unpinned.len(), shown);
        start = end;
    }
    rows
}

/// Larger images stay a file glyph: the list would decode them in full for 28px.
const THUMB_MAX: u64 = 4 * 1024 * 1024;

/// Whether the list draws `name` as a thumbnail: a still image gpui decodes, not too big.
fn has_thumbnail(name: &str, size: u64) -> bool {
    let ext = name
        .rsplit_once('.')
        .map_or("", |(_, e)| e)
        .to_ascii_lowercase();
    size <= THUMB_MAX && !matches!(ext.as_str(), "gif" | "svg") && native(&ext)
}

/// `4 kilobytes`, for a screen reader.
fn spoken_size(n: u64) -> String {
    let unit = |n: f64, one: &str| format!("{n} {one}{}", if n == 1. { "" } else { "s" });
    match n {
        n if n < 1024 => unit(n as f64, "byte"),
        n if n < 1024 * 1024 => unit((n as f64 / 1024.).round(), "kilobyte"),
        n => unit((n as f64 / 1048576. * 10.).round() / 10., "megabyte"),
    }
}

/// `2 minutes ago`, for a screen reader.
fn spoken_age(age: std::time::Duration) -> String {
    let (n, unit) = match age.as_secs() {
        s if s < 60 => return "just now".into(),
        s if s < 3600 => (s / 60, "minute"),
        s if s < 86400 => (s / 3600, "hour"),
        s => (s / 86400, "day"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

impl Artifacts {
    /// The right pane: the `.context` button, the explainer, the filter and the rows.
    pub(super) fn render_list(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, hint) = (theme.muted_foreground, crate::ui::faint(theme));
        let focused = self.focus.contains_focused(window, cx);
        let abs = self.dir.join(".context");
        let (dir, tip) = (self.dir.clone(), abs.display().to_string());
        let rows = if self.rows.is_empty() {
            let query = self.filter.clone();
            v_flex()
                .id("artifacts-no-match")
                .test_support()
                .aria_label(format!("No artifacts match \"{query}\""))
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(format!("No artifacts match \"{query}\""))
                .child(
                    Button::new("artifacts-clear-filter")
                        .label("Clear filter")
                        .small()
                        .on_click(cx.listener(|s, _, w, cx| {
                            s.filter_input.update(cx, |i, cx| i.set_value("", w, cx));
                            s.filter.clear();
                            s.rebuild();
                            cx.notify();
                        })),
                )
                .into_any_element()
        } else {
            uniform_list(
                "artifacts-rows",
                self.rows.len(),
                cx.processor(move |this, range: Range<usize>, _, cx| {
                    range.map(|ix| this.render_row(ix, focused, cx)).collect()
                }),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .into_any_element()
        };
        v_flex()
            .id("artifacts-list")
            .test_support()
            .aria_label("Artifacts")
            .size_full()
            .px(px(8.))
            .border_l_1()
            .border_color(theme.border)
            .text_color(theme.foreground)
            .child(
                v_flex()
                    .flex_none()
                    .gap_1()
                    .px(px(8.))
                    .pt_4()
                    .pb_2()
                    .child(
                        h_flex()
                            .id("artifacts-reveal")
                            .test_support()
                            .aria_label(format!("Reveal .context in Finder: {tip}"))
                            .gap_1p5()
                            .cursor_pointer()
                            .font_family("Menlo")
                            .text_xs()
                            .text_color(theme.sidebar_foreground)
                            .hover(|d| d.text_color(theme.foreground))
                            .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                            .on_click(move |_, window, cx| reveal(&dir, window, cx))
                            .child(Icon::new(IconName::FolderDot).with_size(px(12.)))
                            .child(".context"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("Agents and you keep task files here. Deleted with the task, never committed."),
                    ),
            )
            .child(
                div()
                    .id("artifacts-filter")
                    .test_support()
                    .aria_label("Filter artifacts")
                    .child(
                        Input::new(&self.filter_input)
                            .h(px(32.))
                            .rounded(theme.radius)
                            .cleanable(true)
                            .prefix(Icon::new(IconName::Search).with_size(px(14.)).text_color(muted)),
                    ),
            )
            .child(div().flex_1().min_h_0().flex().flex_col().child(rows))
            .child(
                div()
                    .p(px(12.))
                    .text_xs()
                    .text_color(hint)
                    .child("Enter open · a attach · r refresh · ⌘⌫ delete"),
            )
    }

    /// Row `ix`: a group header or a file. Both are 44px, as `uniform_list` needs.
    fn render_row(&self, ix: usize, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let item = match &self.rows[ix] {
            Row::Item(i) => &self.items[*i],
            Row::Group { key, count, open } => {
                let (key, open) = (key.clone(), *open);
                let label = group_label(&key);
                let id = format!(
                    "artifacts-group-{}",
                    match key.as_str() {
                        "" => "other",
                        PINNED => "pinned",
                        k => k,
                    }
                );
                let aria = format!(
                    "{label}, {count} file{}, {}",
                    crate::ui::plural(*count),
                    if open { "expanded" } else { "collapsed" }
                );
                return h_flex()
                    .id(SharedString::from(id))
                    .test_support()
                    .aria_label(aria)
                    .h(px(44.))
                    .w_full()
                    .flex_none()
                    .items_end()
                    .gap_1p5()
                    .px(px(8.))
                    .pb_1()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .with_size(px(12.)),
                    )
                    .child(label)
                    .child(format!("{count}"))
                    .on_click(cx.listener(move |s, _, _, cx| {
                        if !s.folded.remove(&key) {
                            s.folded.insert(key.clone());
                        }
                        s.rebuild();
                        cx.notify();
                    }))
                    .into_any_element();
            }
        };
        let selected = self.selected.as_deref() == Some(item.rel.as_str());
        let rel = item.rel.clone();
        let (folder, name) = rel.rsplit_once('/').unwrap_or(("", &rel));
        // The path below the group's folder, as a prefix to the name.
        let prefix = match folder.split_once('/') {
            Some((_, below)) => format!("{below}/"),
            None => String::new(),
        };
        let abs = self.abs(&rel);
        let size = human_bytes(item.size);
        let age = SystemTime::now()
            .duration_since(item.modified)
            .unwrap_or_default();
        let detail = format!("{} · {size} · {}", item.kind.label(), compact_age(age));
        let spoken = format!(
            "{name} · {} · {} · modified {}",
            item.kind.label(),
            spoken_size(item.size),
            spoken_age(age)
        );
        let new = self.unseen(item);
        let background = theme.background.blend(if selected && focused {
            theme.list_active
        } else if selected {
            theme.list_hover
        } else {
            theme.transparent
        });
        let hover = theme.background.blend(theme.list_hover);
        let glyph = |theme: &Theme| match crate::file_icon::file_icon(name, theme.is_dark()) {
            Some(i) => img(i).size(px(16.)).flex_none().into_any_element(),
            None => Icon::new(IconName::File)
                .with_size(px(14.))
                .text_color(theme.muted_foreground)
                .into_any_element(),
        };
        let lead = match has_thumbnail(name, item.size) {
            true => {
                let fallback = glyph(theme);
                let fallback = std::cell::Cell::new(Some(fallback));
                img(abs)
                    .image_cache(&self.lru)
                    .size(px(28.))
                    .flex_none()
                    .rounded(theme.radius)
                    .object_fit(ObjectFit::Cover)
                    .with_fallback(move || {
                        fallback
                            .take()
                            .unwrap_or_else(|| div().size(px(16.)).into_any_element())
                    })
                    .into_any_element()
            }
            false => glyph(theme),
        };
        let pinned = self.pins.contains(&rel);
        let (open_rel, menu_rel) = (rel.clone(), rel.clone());
        h_flex()
            .id(SharedString::from(format!("artifact-row-{rel}")))
            .test_support()
            .aria_label(format!(
                "{spoken}{}{}",
                if pinned { " · pinned" } else { "" },
                if new { " · new" } else { "" }
            ))
            .group("artifact-row")
            .h(px(44.))
            .w_full()
            .flex_none()
            .gap_2()
            .px(px(8.))
            .rounded(theme.radius)
            .map(|d| list_cursor_row(d, selected, focused, theme))
            .on_click(cx.listener(move |s, e: &ClickEvent, w, cx| {
                s.focus(w, cx);
                s.select(open_rel.clone(), cx);
                if e.click_count() == 2 {
                    s.open(cx);
                }
            }))
            // Right-click moves the selection too, so it's clear which row the menu is for.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |s, e: &MouseDownEvent, w, cx| {
                    s.select(menu_rel.clone(), cx);
                    s.show_menu(menu_rel.clone(), e.position, w, cx);
                }),
            )
            .child(
                h_flex()
                    .size(px(28.))
                    .flex_none()
                    .justify_center()
                    .child(lead),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .min_w_0()
                            .text_sm()
                            .when(!prefix.is_empty(), |d| {
                                d.child(
                                    div()
                                        .flex_shrink_0()
                                        .text_color(theme.sidebar_foreground)
                                        .child(prefix),
                                )
                            })
                            .child(
                                crate::overflow_fade::label(
                                    name.to_string().into(),
                                    background,
                                    Some(("artifact-row", hover)),
                                )
                                .flex_initial(),
                            )
                            .when(pinned, |d| {
                                d.child(
                                    Icon::new(IconName::Pin)
                                        .with_size(px(12.))
                                        .flex_none()
                                        .ml_1()
                                        .text_color(theme.muted_foreground),
                                )
                            }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(detail),
                    ),
            )
            // The slot stays when empty, so the text never shifts.
            .child(div().size(px(6.)).flex_none().children(new.then(|| {
                (div().size_full().rounded_full().bg(theme.primary))
                    .id(SharedString::from(format!("artifact-new-{rel}")))
                    .test_support()
            })))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn item(rel: &str, age: u64) -> Item {
        Item::new(
            rel.into(),
            1,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1000 - age),
        )
    }

    fn shape(items: &[Item], rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Group { key, count, open } => {
                    let key = if key == PINNED { "pinned" } else { key };
                    format!("[{key} {count} {open}]")
                }
                Row::Item(i) => items[*i].rel.clone(),
            })
            .collect()
    }

    #[::core::prelude::v1::test]
    fn groups_in_order_and_newest_first() {
        let mut items = vec![
            item("loose.txt", 1),
            item("zeta/z.txt", 1),
            item("scratch/old.md", 50),
            item("scratch/new.md", 5),
            item("attachments/b.png", 3),
            item("alpha/a.txt", 9),
            item("plans/a.md", 7),
            item("plans/sub/b.md", 2),
        ];
        arrange(&mut items);
        let all = rows(&items, "", &HashSet::new(), &[]);
        assert_eq!(
            shape(&items, &all),
            [
                "[plans 2 true]",
                "plans/sub/b.md",
                "plans/a.md",
                "[attachments 1 true]",
                "attachments/b.png",
                "[scratch 2 true]",
                "scratch/new.md",
                "scratch/old.md",
                "[alpha 1 true]",
                "alpha/a.txt",
                "[zeta 1 true]",
                "zeta/z.txt",
                "[ 1 true]",
                "loose.txt",
            ]
        );
    }

    #[::core::prelude::v1::test]
    fn group_labels_capitalize_the_first_character_not_byte() {
        assert_eq!(group_label(""), "Other");
        assert_eq!(group_label("plans"), "Plans");
        assert_eq!(group_label("été"), "Été");
        assert_eq!(group_label("日本"), "日本");
    }

    #[::core::prelude::v1::test]
    fn spoken_size_and_age() {
        assert_eq!(spoken_size(1), "1 byte");
        assert_eq!(spoken_size(512), "512 bytes");
        assert_eq!(spoken_size(4096), "4 kilobytes");
        assert_eq!(spoken_size(1536 * 1024), "1.5 megabytes");
        let secs = std::time::Duration::from_secs;
        assert_eq!(spoken_age(secs(5)), "just now");
        assert_eq!(spoken_age(secs(120)), "2 minutes ago");
        assert_eq!(spoken_age(secs(3600)), "1 hour ago");
        assert_eq!(spoken_age(secs(3 * 86400)), "3 days ago");
    }

    #[::core::prelude::v1::test]
    fn thumbnails_only_for_small_decodable_stills() {
        assert!(has_thumbnail("a.PNG", 1000));
        assert!(!has_thumbnail("a.png", THUMB_MAX + 1));
        assert!(!has_thumbnail("a.gif", 1));
        assert!(!has_thumbnail("a.svg", 1));
        assert!(!has_thumbnail("a.pdf", 1));
        assert!(!has_thumbnail("png", 1));
    }

    #[::core::prelude::v1::test]
    fn folding_and_filtering() {
        let mut items = vec![
            item("plans/Auth.md", 1),
            item("plans/b.md", 2),
            item("scratch/auth notes.txt", 3),
        ];
        arrange(&mut items);
        let folded = HashSet::from(["plans".to_string()]);
        assert_eq!(
            shape(&items, &rows(&items, "", &folded, &[])),
            [
                "[plans 2 false]",
                "[scratch 1 true]",
                "scratch/auth notes.txt"
            ]
        );
        // Counts stay the group's total; a group with no match leaves.
        assert_eq!(
            shape(&items, &rows(&items, "AUTH", &HashSet::new(), &[])),
            [
                "[plans 2 true]",
                "plans/Auth.md",
                "[scratch 1 true]",
                "scratch/auth notes.txt"
            ]
        );
        assert_eq!(
            shape(&items, &rows(&items, "b.md", &HashSet::new(), &[])),
            ["[plans 2 true]", "plans/b.md"]
        );
        assert!(rows(&items, "zzz", &HashSet::new(), &[]).is_empty());
        assert!(rows(&[], "", &HashSet::new(), &[]).is_empty());
    }

    #[::core::prelude::v1::test]
    fn pinned_group_comes_first_in_pin_order_and_leaves_its_folder() {
        let mut items = vec![
            item("plans/a.md", 1),
            item("plans/b.md", 2),
            item("scratch/s.md", 3),
            item("loose.txt", 4),
        ];
        arrange(&mut items);
        let pins = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let all = rows(
            &items,
            "",
            &HashSet::new(),
            // Newest pin first; a pin with no file is skipped.
            &pins(&["loose.txt", "gone.md", "plans/b.md"]),
        );
        assert_eq!(
            shape(&items, &all),
            [
                "[pinned 2 true]",
                "loose.txt",
                "plans/b.md",
                "[plans 1 true]",
                "plans/a.md",
                "[scratch 1 true]",
                "scratch/s.md",
            ]
        );
        // The filter applies to pinned rows; a group with no match leaves.
        let some = rows(&items, "b.md", &HashSet::new(), &pins(&["loose.txt", "plans/b.md"]));
        assert_eq!(shape(&items, &some), ["[pinned 2 true]", "plans/b.md"]);
        // Folding works like any group; a group emptied by pins is gone.
        let folded = HashSet::from([PINNED.to_string()]);
        let all = rows(&items, "", &folded, &pins(&["plans/a.md", "plans/b.md"]));
        assert_eq!(
            shape(&items, &all)[..2],
            ["[pinned 2 false]", "[scratch 1 true]"]
        );
        // Only missing pins: no group.
        assert!(rows(&items, "", &HashSet::new(), &pins(&["gone.md"]))
            .iter()
            .all(|r| !matches!(r, Row::Group { key, .. } if key == PINNED)));
    }
}

//! Shared, axis-locked drag previews. The saved order changes only on release.
use super::*;
use gpui_kit::base::motion::{Transition, transition};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Item {
    Tab(String, TabId),
    /// The trailing space of a workspace's tab group: a tab dropped there goes last.
    Append(String, usize),
    /// The dotted target that moves a tab into a new right group.
    Split(String),
    Workspace(String),
    Section(usize),
    Ungrouped,
}

#[derive(Default)]
pub(super) struct Reorder {
    bounds: HashMap<Item, Bounds<Pixels>>,
    offsets: HashMap<Item, Pixels>,
    pub(super) source: Option<Item>,
    pub(super) target: Option<Item>,
    /// The drag has been shown: the titlebar's drop targets are up.
    armed: bool,
}

/// Where a drag previews and may drop: the sidebar's list, or the titlebar's tab area.
#[derive(Clone)]
enum Area {
    Scroll(ScrollHandle),
    Tabs(Rc<std::cell::Cell<Bounds<Pixels>>>),
}

impl Area {
    fn bounds(&self) -> Bounds<Pixels> {
        match self {
            Area::Scroll(s) => s.bounds(),
            Area::Tabs(b) => b.get(),
        }
    }
}

#[derive(Clone)]
pub(super) struct Drag {
    source: Item,
    sections: Vec<String>,
    label: SharedString,
    offset: Point<Pixels>,
    scroll: Area,
    state: Rc<RefCell<Reorder>>,
}

impl Drag {
    fn horizontal(&self) -> bool {
        matches!(self.source, Item::Tab(..))
    }

    fn valid(&self, shark: &Shark, cx: &App) -> bool {
        let current = shark.reorder_items(self.horizontal(), cx);
        current.contains(&self.source)
            && (self.horizontal()
                || self.sections
                    == shark
                        .state
                        .sections
                        .iter()
                        .map(|section| section.name.clone())
                        .collect::<Vec<_>>())
    }

    fn preview_bounds(&self, pointer: Point<Pixels>) -> Bounds<Pixels> {
        let area = self.scroll.bounds();
        let original = self
            .state
            .borrow()
            .bounds
            .get(&self.source)
            .copied()
            .unwrap_or(area);
        let size = size(
            original.size.width.min(area.size.width),
            original.size.height.min(area.size.height),
        );
        let origin = if self.horizontal() {
            point(
                (pointer.x - self.offset.x).clamp(area.left(), area.right() - size.width),
                original
                    .top()
                    .clamp(area.top(), area.bottom() - size.height),
            )
        } else {
            point(
                original
                    .left()
                    .clamp(area.left(), area.right() - size.width),
                (pointer.y - self.offset.y).clamp(area.top(), area.bottom() - size.height),
            )
        };
        Bounds { origin, size }
    }
}

impl Render for Drag {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pointer = window.mouse_position();
        let bounds = self.preview_bounds(pointer);
        let correction = bounds.origin - (pointer - self.offset);
        // Over a drop zone the chip fades, so the target under it stays visible.
        let zone = matches!(
            self.state.borrow().target,
            Some(Item::Append(..) | Item::Split(_))
        );
        // GPUI positions the preview root itself; only a child can offset it back into the pane.
        div().child(
            sidebar::drag_chip(self.label.clone(), cx)
                .when(zone, |d| d.opacity(0.5))
                .id("reorder-preview")
                .test_support()
                .relative()
                .left(correction.x)
                .top(correction.y)
                .w(bounds.size.width)
                .h(bounds.size.height)
                .overflow_hidden(),
        )
    }
}

impl Shark {
    pub(super) fn prune_reorder(&self, cx: &App) {
        let mut current = self.reorder_items(true, cx);
        current.extend(self.reorder_items(false, cx));
        current.push(Item::Ungrouped);
        if let Some(name) = &self.active {
            current.extend((0..2).map(|g| Item::Append(name.clone(), g)));
            current.push(Item::Split(name.clone()));
        }
        let mut state = self.reorder.borrow_mut();
        state.bounds.retain(|key, _| current.contains(key));
        if !cx.has_active_drag() {
            state.source = None;
            state.target = None;
            state.armed = false;
            state.offsets.clear();
        }
    }

    /// Escape during a tab or sidebar drag: drop nothing, put everything back.
    pub(super) fn cancel_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !cx.has_active_drag() || self.reorder.borrow().source.is_none() {
            return false;
        }
        let mut state = self.reorder.borrow_mut();
        state.source = None;
        state.target = None;
        state.armed = false;
        state.offsets.clear();
        drop(state);
        cx.stop_active_drag(window);
        cx.notify();
        true
    }
    pub(super) fn reorder_source<E: StatefulInteractiveElement>(
        &self,
        row: E,
        source: Item,
        label: SharedString,
    ) -> E {
        let scroll = if matches!(source, Item::Tab(..)) {
            Area::Tabs(self.tabs_area.clone())
        } else {
            Area::Scroll(self.sidebar_scroll.clone())
        };
        let state = self.reorder.clone();
        let sections = if matches!(source, Item::Tab(..)) {
            vec![]
        } else {
            self.state
                .sections
                .iter()
                .map(|section| section.name.clone())
                .collect()
        };
        row.on_drag(
            Drag {
                source,
                sections,
                label,
                offset: Point::default(),
                scroll,
                state,
            },
            |drag, offset, _, cx| {
                let mut drag = drag.clone();
                drag.offset = offset;
                let mut state = drag.state.borrow_mut();
                state.source = Some(drag.source.clone());
                state.target = Some(drag.source.clone());
                state.offsets.clear();
                drop(state);
                cx.new(|_| drag)
            },
        )
    }

    pub(super) fn reorder_item(&self, element: AnyElement, item: Item) -> AnyElement {
        Moving {
            element,
            item,
            state: self.reorder.clone(),
        }
        .into_any_element()
    }

    /// Tabs of every group, left to right, or the sidebar's rows.
    fn reorder_items(&self, horizontal: bool, cx: &App) -> Vec<Item> {
        if horizontal {
            self.active_ws()
                .map(|ws| {
                    (0..2)
                        .flat_map(|g| self.visible_tab_ids(ws, g, cx))
                        .map(|id| Item::Tab(ws.name.clone(), id))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            self.side_rows(false)
                .into_iter()
                .map(|row| match row {
                    SideRow::Ws(i) => Item::Workspace(self.state.workspaces[i].name.clone()),
                    SideRow::Section(i) => Item::Section(i),
                })
                .collect()
        }
    }

    fn reorder_move(&mut self, drag: &Drag, pointer: Point<Pixels>, cx: &mut Context<Self>) {
        let horizontal = drag.horizontal();
        let area = drag.scroll.bounds();
        let mut order = self.reorder_items(horizontal, cx);
        // A tab reorders within its own strip; other strips and the zones only take drops.
        let group = |id: &TabId| self.active_ws().map_or(0, |ws| ws.group_of(id));
        if let Item::Tab(_, source) = &drag.source {
            let g = group(source);
            order.retain(|item| matches!(item, Item::Tab(_, id) if group(id) == g));
        }
        let zones = self.drop_zones(drag, cx);
        let mut state = self.reorder.borrow_mut();
        if !drag.valid(self, cx) {
            state.target = None;
            state.offsets.clear();
            drop(state);
            cx.notify();
            return;
        }
        if horizontal && !state.armed {
            // The split target and drop zones appear with the drag.
            state.armed = true;
            cx.notify();
        }
        let valid = |item: &&Item| match (&drag.source, *item) {
            (Item::Tab(ws, _), Item::Tab(other, _)) => ws == other,
            (Item::Tab(..), Item::Append(..) | Item::Split(_)) => zones.contains(item),
            (Item::Workspace(_), Item::Workspace(_) | Item::Section(_) | Item::Ungrouped) => true,
            (Item::Section(_), Item::Section(_)) => true,
            _ => false,
        };
        let coordinate = if horizontal {
            pointer.x.clamp(area.left(), area.right())
        } else {
            pointer.y.clamp(area.top(), area.bottom())
        };
        let target = state
            .bounds
            .keys()
            .filter(|key| {
                order.contains(key)
                    || **key == Item::Ungrouped
                    || zones.contains(key)
                    || (horizontal && matches!(key, Item::Tab(..)))
            })
            .filter(valid)
            .min_by(|a, b| {
                let distance = |item: &Item| {
                    let b = state.bounds[item];
                    let (start, end) = if horizontal {
                        (b.left(), b.right())
                    } else {
                        (b.top(), b.bottom())
                    };
                    if coordinate < start {
                        start - coordinate
                    } else if coordinate > end {
                        coordinate - end
                    } else {
                        px(0.)
                    }
                };
                // Nested zones (the split target inside the trailing space): the smaller wins.
                let width = |item: &Item| state.bounds[item].size.width;
                (distance(a), width(a))
                    .partial_cmp(&(distance(b), width(b)))
                    .unwrap_or(Ordering::Equal)
            })
            .cloned();
        if target == state.target {
            return;
        }
        state.target = target.clone();
        state.offsets.clear();
        let Some(target) = target else { return };
        if horizontal && !order.contains(&target) {
            // Another strip or a zone: it highlights, nothing slides.
            drop(state);
            cx.notify();
            return;
        }
        let before = order.clone();
        match (&drag.source, &target) {
            (Item::Section(from), Item::Section(to)) => {
                let groups = |i: usize| {
                    let start = before
                        .iter()
                        .position(|key| *key == Item::Section(i))
                        .unwrap();
                    let end = before
                        .iter()
                        .enumerate()
                        .skip(start + 1)
                        .find(|(_, key)| {
                            matches!(key, Item::Section(_))
                                || matches!(key,Item::Workspace(n) if self.section_of(n).is_none())
                        })
                        .map_or(before.len(), |(i, _)| i);
                    start..end
                };
                let range = groups(*from);
                let destination = groups(*to);
                let block: Vec<_> = order.drain(range.clone()).collect();
                let insert = if range.start < destination.start {
                    destination.end - block.len()
                } else {
                    destination.start
                };
                order.splice(insert..insert, block);
            }
            (Item::Workspace(source), _) => {
                // Use the same grouping rules as the eventual drop, without saving a preview.
                let mut preview = State {
                    workspaces: self.state.workspaces.clone(),
                    sections: self.state.sections.clone(),
                    ..State::default()
                };
                match &target {
                    Item::Workspace(to) => preview.drop_ws(source, to),
                    Item::Section(i) if self.section_of(source) != Some(*i) => {
                        for section in &mut preview.sections {
                            section.workspaces.retain(|name| name != source);
                        }
                        preview.sections[*i].workspaces.push(source.clone());
                    }
                    Item::Ungrouped => {
                        for section in &mut preview.sections {
                            section.workspaces.retain(|name| name != source);
                        }
                    }
                    _ => {}
                }
                order.clear();
                for (i, section) in preview.sections.iter().enumerate() {
                    order.push(Item::Section(i));
                    if !section.collapsed {
                        order.extend(section.workspaces.iter().cloned().map(Item::Workspace));
                    }
                }
                order.extend(
                    preview
                        .workspaces
                        .iter()
                        .filter(|ws| {
                            !preview
                                .sections
                                .iter()
                                .any(|s| s.workspaces.contains(&ws.name))
                        })
                        .map(|ws| Item::Workspace(ws.name.clone())),
                );
            }
            _ => {
                if let (Some(from), Some(to)) = (
                    order.iter().position(|key| key == &drag.source),
                    order.iter().position(|key| key == &target),
                ) {
                    move_item(&mut order, from, to);
                }
            }
        }
        let Some(first) = before.first().and_then(|key| state.bounds.get(key)) else {
            return;
        };
        let mut next = if horizontal {
            first.left()
        } else {
            first.top()
        };
        for item in order {
            if let Some(bounds) = state.bounds.get(&item).copied() {
                let original = if horizontal {
                    bounds.left()
                } else {
                    bounds.top()
                };
                state.offsets.insert(item.clone(), next - original);
                let pitch = before
                    .iter()
                    .position(|key| key == &item)
                    .and_then(|i| before.get(i + 1))
                    .and_then(|key| state.bounds.get(key))
                    .map(|following| {
                        if horizontal {
                            following.left() - bounds.left()
                        } else {
                            following.top() - bounds.top()
                        }
                    })
                    .unwrap_or(if horizontal {
                        bounds.size.width + px(4.)
                    } else {
                        bounds.size.height + px(if self.rail { 16. } else { 2. })
                    });
                next += pitch;
            }
        }
        drop(state);
        cx.notify();
    }

    fn reorder_drop(&mut self, drag: &Drag, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.reorder.borrow_mut().target.take();
        self.reorder.borrow_mut().source = None;
        self.reorder.borrow_mut().offsets.clear();
        let zones = self.drop_zones(drag, cx);
        if drag.valid(self, cx)
            && drag.scroll.bounds().contains(&window.mouse_position())
            && target.as_ref().is_some_and(|key| {
                *key == Item::Ungrouped
                    || zones.contains(key)
                    || self.reorder_items(drag.horizontal(), cx).contains(key)
            })
        {
            let group = |s: &Shark, id: &TabId| s.active_ws().map_or(0, |ws| ws.group_of(id));
            match (&drag.source, target) {
                (Item::Tab(ws, from), Some(Item::Tab(other, to)))
                    if *ws == other && group(self, from) == group(self, &to) =>
                {
                    self.move_tab(ws, from, &to, cx)
                }
                (Item::Tab(ws, from), Some(Item::Tab(other, to))) if *ws == other => {
                    let g = group(self, &to);
                    self.move_to_group(ws, from, g, Some(&to), window, cx)
                }
                (Item::Tab(ws, from), Some(Item::Append(other, g))) if *ws == other => {
                    self.move_to_group(ws, from, g, None, window, cx)
                }
                (Item::Tab(ws, from), Some(Item::Split(other))) if *ws == other => {
                    self.move_to_group(ws, from, 1, None, window, cx)
                }
                (Item::Workspace(ws), Some(Item::Workspace(to))) => self.drop_ws(ws, &to, cx),
                (Item::Workspace(ws), Some(Item::Section(to))) => {
                    self.move_to_section(ws, Some(to), cx)
                }
                (Item::Workspace(ws), Some(Item::Ungrouped)) => self.move_to_section(ws, None, cx),
                (Item::Section(from), Some(Item::Section(to))) => self.drop_section(*from, to, cx),
                _ => {}
            }
        }
        cx.notify();
    }

    /// The zones a tab drag may drop on: each group's trailing space, and the split target
    /// while there is one group and the tab isn't the only one.
    fn drop_zones(&self, drag: &Drag, cx: &App) -> Vec<Item> {
        let (Item::Tab(name, tab), Some(ws)) = (&drag.source, self.active_ws()) else {
            return vec![];
        };
        if ws.name != *name {
            return vec![];
        }
        let mut zones: Vec<Item> = self
            .groups()
            .map(|g| Item::Append(name.clone(), g))
            .collect();
        if !ws.split() && self.can_move_to(tab, 1, cx) {
            zones.push(Item::Split(name.clone()));
        }
        zones
    }

    pub(super) fn reorder_events(&self, element: Div, cx: &Context<Self>) -> Div {
        element
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<Drag>, _, cx| {
                let drag = event.drag(cx).clone();
                this.reorder_move(&drag, event.event.position, cx);
            }))
            .on_drop(
                cx.listener(|this, drag: &Drag, window, cx| this.reorder_drop(drag, window, cx)),
            )
    }
}

struct Moving {
    element: AnyElement,
    item: Item,
    state: Rc<RefCell<Reorder>>,
}
impl IntoElement for Moving {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Moving {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        Some(format!("reorder-{:?}", self.item).into())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.element.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut state = self.state.borrow_mut();
        // Drop targets use layout bounds, so they do not chase the animated rows.
        state.bounds.insert(self.item.clone(), bounds);
        let target = if cx.has_active_drag() {
            state.offsets.get(&self.item).copied().unwrap_or_default()
        } else {
            px(0.)
        };
        drop(state);
        let duration = if cx.has_active_drag() {
            Duration::from_millis(160)
        } else {
            Duration::ZERO
        };
        let offset = transition(
            "reorder-offset",
            target,
            Transition::new(duration),
            window,
            cx,
        );
        let offset = if matches!(self.item, Item::Tab(..) | Item::Append(..) | Item::Split(_)) {
            point(offset, px(0.))
        } else {
            point(px(0.), offset)
        };
        window.with_element_offset(offset, |window| self.element.prepaint(window, cx));
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if !cx.has_active_drag() || self.state.borrow().source.as_ref() != Some(&self.item) {
            self.element.paint(window, cx);
        }
    }
}

//! Shared, axis-locked drag previews. The saved order changes only on release.
use super::*;
use gpui_kit::base::motion::{Transition, transition};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Item {
    Tab(String, TabId),
    Workspace(String),
    Section(usize),
    Ungrouped,
}

#[derive(Default)]
pub(super) struct Reorder {
    bounds: HashMap<Item, Bounds<Pixels>>,
    offsets: HashMap<Item, Pixels>,
    source: Option<Item>,
    target: Option<Item>,
}

#[derive(Clone)]
pub(super) struct Drag {
    source: Item,
    sections: Vec<String>,
    label: SharedString,
    offset: Point<Pixels>,
    scroll: ScrollHandle,
    state: Rc<RefCell<Reorder>>,
}

/// `b`'s extent along the drag axis: its left and right, or top and bottom.
fn axis(b: &Bounds<Pixels>, horizontal: bool) -> (Pixels, Pixels) {
    match horizontal {
        true => (b.left(), b.right()),
        false => (b.top(), b.bottom()),
    }
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
        // GPUI positions the preview root itself; only a child can offset it back into the pane.
        div().child(
            sidebar::drag_chip(self.label.clone(), cx)
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
        let mut state = self.reorder.borrow_mut();
        state.bounds.retain(|key, _| current.contains(key));
        if !cx.has_active_drag() {
            state.source = None;
            state.target = None;
            state.offsets.clear();
        }
    }
    pub(super) fn reorder_source<E: StatefulInteractiveElement>(
        &self,
        row: E,
        source: Item,
        label: SharedString,
    ) -> E {
        let scroll = if matches!(source, Item::Tab(..)) {
            self.tab_scroll.clone()
        } else {
            self.sidebar_scroll.clone()
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

    fn reorder_items(&self, horizontal: bool, cx: &App) -> Vec<Item> {
        if horizontal {
            self.active_ws()
                .map(|ws| {
                    self.visible_tab_ids(ws, cx)
                        .into_iter()
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
        let mut state = self.reorder.borrow_mut();
        if !drag.valid(self, cx) {
            state.target = None;
            state.offsets.clear();
            drop(state);
            cx.notify();
            return;
        }
        let valid = |item: &&Item| match (&drag.source, *item) {
            (Item::Tab(ws, _), Item::Tab(other, _)) => ws == other,
            (Item::Workspace(_), Item::Workspace(_) | Item::Section(_) | Item::Ungrouped) => true,
            (Item::Section(_), Item::Section(_)) => true,
            _ => false,
        };
        let (low, high) = axis(&area, horizontal);
        let coordinate = if horizontal { pointer.x } else { pointer.y }.clamp(low, high);
        let target = state
            .bounds
            .keys()
            .filter(|key| order.contains(key) || **key == Item::Ungrouped)
            .filter(valid)
            .min_by(|a, b| {
                let distance = |item: &Item| {
                    let (start, end) = axis(&state.bounds[item], horizontal);
                    if coordinate < start {
                        start - coordinate
                    } else if coordinate > end {
                        coordinate - end
                    } else {
                        px(0.)
                    }
                };
                distance(a)
                    .partial_cmp(&distance(b))
                    .unwrap_or(Ordering::Equal)
            })
            .cloned();
        if target == state.target {
            return;
        }
        state.target = target.clone();
        state.offsets.clear();
        let Some(target) = target else { return };
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
                    Item::Section(i) => preview.move_to_section(source, Some(*i)),
                    Item::Ungrouped => preview.move_to_section(source, None),
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
        let mut next = axis(first, horizontal).0;
        for item in order {
            if let Some(bounds) = state.bounds.get(&item).copied() {
                let original = axis(&bounds, horizontal).0;
                state.offsets.insert(item.clone(), next - original);
                let pitch = before
                    .iter()
                    .position(|key| key == &item)
                    .and_then(|i| before.get(i + 1))
                    .and_then(|key| state.bounds.get(key))
                    .map(|following| axis(following, horizontal).0 - original)
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
        if drag.valid(self, cx)
            && drag.scroll.bounds().contains(&window.mouse_position())
            && target.as_ref().is_some_and(|key| {
                *key == Item::Ungrouped || self.reorder_items(drag.horizontal(), cx).contains(key)
            })
        {
            match (&drag.source, target) {
                (Item::Tab(ws, from), Some(Item::Tab(other, to))) if *ws == other => {
                    self.move_tab(ws, from, &to, cx)
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
        let offset = if matches!(self.item, Item::Tab(..)) {
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

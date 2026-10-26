//! gpui-component's `Checkbox` with a visible unchecked box. Upstream borders it with the
//! theme's `input` color, which nearly vanishes on dark themes; this lends it the muted text
//! color (70 %) while the checkbox lays out, which is when it reads the theme.

use gpui_kit::component::{ActiveTheme as _, Sizable, Size, Theme, checkbox, text::Text};
use gpui_kit::*;

pub struct Checkbox(checkbox::Checkbox);

impl Checkbox {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self(checkbox::Checkbox::new(id))
    }

    pub fn checked(self, checked: bool) -> Self {
        Self(self.0.checked(checked))
    }

    pub fn label(self, label: impl Into<Text>) -> Self {
        Self(self.0.label(label))
    }

    pub fn tooltip(self, tooltip: impl Into<SharedString>) -> Self {
        Self(self.0.tooltip(tooltip))
    }

    pub fn on_click(self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        Self(self.0.on_click(handler))
    }
}

impl Sizable for Checkbox {
    fn with_size(self, size: impl Into<Size>) -> Self {
        Self(self.0.with_size(size))
    }
}

impl IntoElement for Checkbox {
    type Element = Visible;

    fn into_element(self) -> Visible {
        Visible(self.0.into_any_element())
    }
}

pub struct Visible(AnyElement);

impl IntoElement for Visible {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Visible {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
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
        let border = cx.theme().muted_foreground.opacity(0.7);
        let input = std::mem::replace(&mut Theme::global_mut(cx).input, border);
        let layout = self.0.request_layout(window, cx);
        Theme::global_mut(cx).input = input;
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0.prepaint(window, cx);
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
        self.0.paint(window, cx);
    }
}

//! Text-overflow treatment shared by tabs and click menus.
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// Keep fitting text unchanged and fade the last 24px of overflowing text into its surface.
pub(crate) fn label(
    text: SharedString,
    background: Hsla,
    hover: Option<(&'static str, Hsla)>,
) -> Div {
    let measure = text.clone();
    div()
        .relative()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .child(text)
        .child(
            div()
                .absolute()
                .size_full()
                .top_0()
                .left_0()
                .text_color(background)
                .when_some(hover, |d, (group, color)| {
                    d.group_hover(group, |d| d.text_color(color))
                })
                .child(
                    canvas(
                        move |bounds, window, _| {
                            let style = window.text_style();
                            let line = window.text_system().shape_line(
                                measure.clone(),
                                style.font_size.to_pixels(window.rem_size()),
                                &[style.to_run(measure.len())],
                                None,
                            );
                            line.width > bounds.size.width
                        },
                        |bounds, overflow, window, _| {
                            if overflow {
                                let color = window.text_style().color;
                                let width = px(24.).min(bounds.size.width);
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(bounds.right() - width, bounds.top()),
                                        size(width, bounds.size.height),
                                    ),
                                    linear_gradient(
                                        90.,
                                        linear_color_stop(color.opacity(0.), 0.),
                                        linear_color_stop(color, 1.),
                                    ),
                                ));
                            }
                        },
                    )
                    .size_full(),
                ),
        )
}

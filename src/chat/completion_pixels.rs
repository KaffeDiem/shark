//! Render the existing completion popup alongside click-menu acceptance fixtures.
use super::*;
use gpui_kit::test::TestWindowExt as _;

struct CompletionPreview(Entity<Chat>);

impl Render for CompletionPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let popup = self.0.update(cx, |chat, cx| {
            assert_eq!(chat.menu.as_ref().unwrap().matches.len(), 3);
            chat.render_menu(cx).expect("seeded completion popup")
        });
        let theme = cx.theme();
        div().size_full().bg(theme.background).child(
            div()
                .absolute()
                .left(px(24.))
                .right(px(24.))
                .bottom(px(24.))
                .h(px(40.))
                .p(px(8.))
                .rounded(theme.radius)
                .bg(theme.secondary)
                .text_color(theme.foreground)
                .child("#")
                .child(popup),
        )
    }
}

pub(crate) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let prs: Vec<complete::Pr> = serde_json::from_value(serde_json::json!([
        { "number": 124, "title": "Polish all click menus", "url": "https://example.invalid/pull/124", "state": "OPEN" },
        { "number": 123, "title": "Keep completion popup styling unchanged", "url": "https://example.invalid/pull/123", "state": "OPEN", "isDraft": true },
        { "number": 121, "title": "Preserve keyboard selection and the existing hint row", "url": "https://example.invalid/pull/121", "state": "MERGED" }
    ])).unwrap();
    for (mode, name) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        cx.update(|cx| {
            crate::theme::apply(
                &crate::state::Appearance {
                    mode,
                    ..Default::default()
                },
                cx,
            )
        });
        let (window, _) = cx
            .update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(0.), px(0.)),
                            size(px(620.), px(260.)),
                        ))),
                        focus: false,
                        show: false,
                        ..Default::default()
                    },
                    cx,
                    |window, cx| {
                        let fixture = format!("completion-pixels-{}-{name}", std::process::id());
                        let chat = cx.new(|cx| {
                            Chat::new(
                                Workspace {
                                    name: fixture.clone(),
                                    ..Default::default()
                                },
                                ChatMeta {
                                    id: fixture,
                                    ..Default::default()
                                },
                                false,
                                window,
                                cx,
                            )
                        });
                        chat.update(cx, |chat, cx| {
                            // The fixture seeds its source; no repository indexing is needed.
                            chat.indexing = None;
                            chat.pr_items = complete::pr_items(&prs);
                            chat.composer.update(cx, |c, cx| {
                                c.set_value("#", window, cx);
                                c.set_selected_range(1..1, cx);
                            });
                            chat.update_menu(cx);
                            chat.menu.as_mut().unwrap().armed = true;
                        });
                        cx.new(|_| CompletionPreview(chat))
                    },
                )
            })
            .unwrap();
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
        })
        .unwrap();
        if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            cx.capture_screenshot(window)
                .expect("real offscreen renderer")
                .save(dir.join(format!("completion-{name}.png")))
                .unwrap();
        }
        cx.update_window(window, |_, w, _| w.remove_window())
            .unwrap();
    }
    println!("completion popup compatibility: 2 rendered cases");
}

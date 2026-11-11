use super::*;
use crate::state::{Agent, AgentIcon};

fn agents() -> State {
    State {
        agents: vec![Agent {
            slug: "designer".into(),
            name: "Designer".into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[gpui_kit::test]
async fn icon_picker_search_applies_and_escape_restores_sidebar(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, agents(), WindowOptions::default());
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.sidebar_focus.focus(w, cx);
        s.pick_agent_icon("designer", w, cx);
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.input("star", cx);
    })
    .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.agent("designer").unwrap().icon, AgentIcon::from("star"));
        assert!(s.icon_picker.is_none());
    });
    assert_eq!(State::load().agents[0].icon, AgentIcon::from("star"));
    on_app(window, shark.clone())(cx, &|s, w, cx| s.pick_agent_icon("designer", w, cx));
    press_in(window)(cx, &["escape"]);
    cx.update_window(window, |_, w, cx| {
        assert!(shark.read(cx).icon_picker.is_none());
        assert!(shark.read(cx).sidebar_focus.is_focused(w));
    })
    .unwrap();
}

#[gpui_kit::test]
async fn paste_icon_shortcut_opens_without_image_or_toast(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, agents(), WindowOptions::default());
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("no image".into())));
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_agent("designer", w, cx));
    press_in(window)(cx, &["cmd-d"]);
    cx.update_window(window, |_, w, cx| {
        assert!(shark.read(cx).icon_picker.is_some());
        assert!(w.notifications(cx).is_empty());
    })
    .unwrap();
    press_in(window)(cx, &["escape"]);
}

#[gpui_kit::test]
async fn replacing_image_commits_before_cleanup_and_rolls_back_on_failure(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, agents(), WindowOptions::default());
    let icon = crate::agent_icon::save_image("designer", &image::RgbaImage::new(96, 96)).unwrap();
    let AgentIcon::Image { file } = &icon else {
        panic!("image icon")
    };
    let path = crate::agent_icon::path(file).unwrap();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        assert!(s.apply_agent_icon("designer", icon.clone(), w, cx));
        s.state.read_only = true;
        assert!(!s.apply_agent_icon("designer", AgentIcon::from("star"), w, cx));
        assert_eq!(s.agent("designer").unwrap().icon, icon);
        assert!(path.exists());
        s.state.read_only = false;
        let emoji = AgentIcon::Emoji {
            value: "🐧".into()
        };
        assert!(s.apply_agent_icon("designer", emoji.clone(), w, cx));
        assert!(!path.exists());
        assert_eq!(State::load().agents[0].icon, emoji);
    });
}

#[gpui_kit::test]
async fn replacing_open_icon_picker_restores_original_focus(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, agents(), WindowOptions::default());
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.sidebar_focus.focus(w, cx);
        s.pick_agent_icon("designer", w, cx);
        s.pick_agent_icon("designer", w, cx);
    });
    press_in(window)(cx, &["escape"]);
    cx.update_window(window, |_, w, cx| {
        assert!(shark.read(cx).sidebar_focus.is_focused(w))
    })
    .unwrap();
}

#[gpui_kit::test]
async fn agent_page_icon_opens_the_picker_on_the_page(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, agents(), WindowOptions::default());
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_agent("designer", w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("agent-page-icon", cx);
    })
    .unwrap();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert!(s.icon_picker.is_some());
        assert_eq!(
            s.home.destination,
            home::Destination::Agent("designer".into())
        );
    });
    press_in(window)(cx, &["escape"]);
    cx.read(|cx| assert!(shark.read(cx).icon_picker.is_none()));
}

#[gpui_kit::test]
async fn paste_image_bytes_and_finder_file_reach_crop_then_persist(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    fs::create_dir_all(&home).unwrap();
    let path = home.join("portrait.png");
    image::RgbaImage::from_pixel(24, 36, image::Rgba([200, 30, 70, 255]))
        .save(&path)
        .unwrap();
    let (window, shark) = boot(cx, agents(), WindowOptions::default());
    for item in [
        ClipboardItem::new_image(&Image::from_bytes(
            ImageFormat::Png,
            fs::read(&path).unwrap(),
        )),
        ClipboardEntry::ExternalPaths(ExternalPaths(vec![path.clone()].into())).into(),
    ] {
        cx.update(|cx| cx.write_to_clipboard(item));
        on_app(window, shark.clone())(cx, &|s, w, cx| s.paste_agent_icon("designer", w, cx));
        cx.run_until_parked();
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            assert!(w.try_find("agent-icon-crop").is_some());
        })
        .unwrap();
        press_in(window)(cx, &["enter"]);
        cx.run_until_parked();
        cx.read(|cx| assert!(shark.read(cx).icon_picker.is_none()));
        assert!(matches!(
            State::load().agents[0].icon,
            AgentIcon::Image { .. }
        ));
    }
}

#[gpui_kit::test]
async fn rejected_crop_cleanup_preserves_referenced_images(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, agents(), WindowOptions::default());
    let old = crate::agent_icon::save_image("designer", &image::RgbaImage::new(96, 96)).unwrap();
    let rejected = crate::agent_icon::save_image(
        "designer",
        &image::RgbaImage::from_pixel(96, 96, image::Rgba([255, 0, 0, 255])),
    )
    .unwrap();
    let path = |icon: &AgentIcon| {
        let AgentIcon::Image { file } = icon else {
            panic!("image")
        };
        crate::agent_icon::path(file).unwrap()
    };
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        assert!(s.apply_agent_icon("designer", old.clone(), w, cx));
        s.state.read_only = true;
        assert!(!s.apply_agent_icon("designer", rejected.clone(), w, cx));
        assert_eq!(s.agent("designer").unwrap().icon, old);
        assert!(path(&old).exists());
        assert!(
            !path(&rejected).exists(),
            "A rejected crop must not leave a private photo behind"
        );
        let shared =
            crate::agent_icon::save_image("other", &image::RgbaImage::new(96, 96)).unwrap();
        s.state.agents.push(Agent {
            slug: "other".into(),
            name: "Other".into(),
            icon: shared.clone(),
            ..Default::default()
        });
        assert!(!s.apply_agent_icon("designer", shared.clone(), w, cx));
        assert!(path(&shared).exists(), "Another agent still owns the image");
        assert!(!s.apply_agent_icon("designer", old.clone(), w, cx));
        assert!(
            path(&old).exists(),
            "Rejecting the same crop must preserve the current image"
        );
    });
}

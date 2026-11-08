use super::*;
use crate::state::{Agent, AgentIcon, AgentLink, AttachStage, Attachment};
use gpui_kit::InputEvent as _;

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
async fn rail_agent_warning_stays_beside_photo_when_tasks_are_collapsed(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, agents(), WindowOptions::default());
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        s.state.agents[0].icon = crate::agent_icon::save_image(
            "designer",
            &image::RgbaImage::from_pixel(32, 32, image::Rgba([20, 80, 180, 255])),
        )
        .unwrap();
        s.state.tasks.push(Task {
            name: "blocked".into(),
            agent: Some(AgentLink::from(&s.state.agents[0])),
            attaching: vec![Attachment {
                repo: "web".into(),
                src: PathBuf::new(),
                branch: "blocked".into(),
                base: String::new(),
                fresh: true,
                stage: AttachStage::SetupFailed,
                error: Some("Setup failed".into()),
            }],
            ..Default::default()
        });
        s.rail = true;
        s.set_agent_collapsed(0, true, cx);
        assert!(matches!(s.attention_of("blocked", cx), Attn::Needs(_)));
        cx.notify();
    });
    for scale in [1., 2.] {
        cx.simulate_window_scale_factor_change(window, scale);
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            let photo = w.find(("agent-icon", 0usize)).bounds();
            let warning = w.find(("agent-rail-warning", 0usize)).bounds();
            assert_eq!(photo.size, size(px(16.), px(16.)));
            assert!(warning.size.width > px(0.) && warning.size.height > px(0.));
            assert!(
                photo.right() <= warning.left() || photo.bottom() <= warning.top(),
                "warning must not cover the face"
            );
        })
        .unwrap();
    }
}

#[gpui_kit::test]
async fn hovered_agent_photo_drop_does_not_import_repository_or_show_error(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    fs::create_dir_all(&home).unwrap();
    let path = home.join("hovered-avatar.png");
    image::RgbaImage::from_pixel(32, 32, image::Rgba([20, 80, 180, 255]))
        .save(&path)
        .unwrap();
    let (window, shark) = boot(cx, agents(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let position = w.find(("agent", 0usize)).bounds().center();
        w.dispatch_event(
            MouseMoveEvent {
                position,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        w.render_frame(cx);
        w.dispatch_event(
            FileDropEvent::Entered {
                position,
                paths: ExternalPaths(vec![path.clone()].into()),
            }
            .to_platform_input(),
            cx,
        );
        w.dispatch_event(FileDropEvent::Pending { position }.to_platform_input(), cx);
        w.dispatch_event(FileDropEvent::Submit { position }.to_platform_input(), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        assert!(
            w.notifications(cx).is_empty(),
            "an avatar drop must not reach repository import"
        );
        let s = shark.read(cx);
        assert!(s.state.repos.is_empty());
        assert!(s.agent_icon_drop.is_none());
        assert!(matches!(
            s.agent("designer").unwrap().icon,
            AgentIcon::Image { .. }
        ));
    })
    .unwrap();
}

#[gpui_kit::test]
async fn repository_directory_drop_over_agent_row_keeps_repository_import(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repo = repos(&home, &["dropped-web"]).remove(0);
    let (window, shark) = boot(cx, agents(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let position = w.find(("agent", 0usize)).bounds().center();
        w.dispatch_event(
            MouseMoveEvent {
                position,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        w.render_frame(cx);
        w.dispatch_event(
            FileDropEvent::Entered {
                position,
                paths: ExternalPaths(vec![repo.path.clone()].into()),
            }
            .to_platform_input(),
            cx,
        );
        w.dispatch_event(FileDropEvent::Pending { position }.to_platform_input(), cx);
        w.dispatch_event(FileDropEvent::Submit { position }.to_platform_input(), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.state.repos.len(),
            1,
            "repository folders must retain sidebar import"
        );
        assert_eq!(s.state.repos[0].path, repo.path.canonicalize().unwrap());
        assert!(
            s.icon_picker.is_none(),
            "a folder must not open the avatar picker"
        );
        assert!(s.agent_icon_drop.is_none());
        assert!(w.notifications(cx).is_empty());
    })
    .unwrap();
}

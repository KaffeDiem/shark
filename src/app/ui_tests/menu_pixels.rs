//! Representative caller contents rendered through the production click-menu renderer.
use super::*;

struct MenuPreview(Entity<menu::Menu>);

impl Render for MenuPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().background)
            .font_features(inter_features())
            .child(menu::layer(self.0.clone()))
    }
}

fn entries(case: &str) -> Vec<Entry> {
    let action = |icon, label| menu::item(icon, label, |_, _| {});
    match case {
        "branch" => vec![
            action(IconName::GitBranch, "branch-from-feature")
                .hint("workspace")
                .checked(true),
            action(IconName::GitBranch, "feature/menu-layout").hint("#124 · open"),
            Entry::Sep,
            action(IconName::GitBranch, "Rebase onto feature/menu-layout").hint("3 commits behind"),
            action(IconName::Plus, "New workspace on top…"),
            action(IconName::Plus, "Insert workspace below…")
                .hint("between feature/menu-layout and branch-from-feature"),
            action(IconName::Copy, "Copy branch name")
                .hint("feature/a-very-long-branch-name-that-must-yield-to-the-action"),
            action(IconName::Target, "Change base…").hint("origin/master"),
            Entry::Sep,
            action(IconName::Github, "Open branch on GitHub"),
            action(IconName::Github, "Open pull request"),
        ],
        "git" => vec![
            action(IconName::ArrowUp, "Push").hint("Next"),
            Entry::Sep,
            action(IconName::ArrowDown, "Fetch now"),
            action(IconName::ArrowDown, "Pull"),
            action(IconName::GitBranch, "Update from base"),
            Entry::Sep,
            action(IconName::Github, "Create or update pull request"),
            action(IconName::Github, "Merge PRs…"),
            action(IconName::GitBranch, "Delete remote branches after merge").checked(true),
        ],
        "models" => crate::chat::model_entries(&ChatMeta::default(), |_, _| {}, |_, _| {}).0,
        _ => vec![
            menu::sub(
                IconName::FolderOpen,
                "Open in Terminal",
                vec![
                    action(IconName::Folder, "Workspace folder")
                        .hint("~/shark/workspaces/branch-from-feature"),
                    Entry::Sep,
                    action(IconName::FolderGit2, "shark"),
                    action(
                        IconName::FolderGit2,
                        "a-repository-with-a-long-descriptive-name",
                    ),
                ],
            ),
            action(IconName::Copy, "Copy path").hint("~/shark/workspaces/branch-from-feature"),
            Entry::Sep,
            action(IconName::Archive, "Archive"),
        ],
    }
}

pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    for (mode, mode_name) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        cx.update(|cx| {
            crate::settings::apply(
                &crate::state::Appearance {
                    mode,
                    ..Default::default()
                },
                cx,
            )
        });
        for case in [
            "branch",
            "git",
            "models",
            "workspace-right",
            "workspace-left",
            "workspace-narrow",
        ] {
            let width = if case == "workspace-narrow" {
                360.
            } else {
                1000.
            };
            let (window, _) = cx
                .update(|cx| {
                    gpui_kit::open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(0.), px(0.)),
                                size(px(width), px(560.)),
                            ))),
                            focus: false,
                            show: false,
                            ..Default::default()
                        },
                        cx,
                        |w, cx| {
                            let leftward = case == "workspace-left";
                            let menu = menu::open_at(
                                entries(case),
                                point(px(if leftward { width - 16. } else { 16. }), px(16.)),
                                if leftward {
                                    Anchor::TopRight
                                } else {
                                    Anchor::TopLeft
                                },
                                w,
                                cx,
                            );
                            menu.update(cx, |m, _| m.select(0));
                            cx.new(|_| MenuPreview(menu))
                        },
                    )
                })
                .unwrap();
            cx.update_window(window, |_, w, cx| {
                w.render_frame(cx);
                if case.starts_with("workspace") {
                    w.press("right", cx);
                    w.render_frame(cx);
                }
                for id in ["menu-parent", "menu-child"] {
                    if let Some(panel) = w.try_find(id) {
                        let bounds = panel.bounds();
                        assert!(
                            bounds.left() >= px(8.) && bounds.right() <= px(width - 8.),
                            "{case} {mode_name} {id} must retain window margins: {bounds:?}"
                        );
                        assert!(
                            bounds.top() >= px(8.) && bounds.bottom() <= px(552.),
                            "{case} {mode_name} {id} must fit the viewport: {bounds:?}"
                        );
                    }
                }
                if case == "workspace-narrow" {
                    assert!(w.try_find("menu-parent").is_none());
                    assert!(w.find("menu-back").visible());
                } else if case.starts_with("workspace") {
                    let parent = w.find("menu-parent").bounds();
                    let child = w.find("menu-child").bounds();
                    assert!(
                        child.right() + px(4.) <= parent.left()
                            || parent.right() + px(4.) <= child.left(),
                        "{case} must keep a traversable gap"
                    );
                }
            })
            .unwrap();
            let screenshot = cx.capture_screenshot(window).expect("real menu renderer");
            if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                let dir = PathBuf::from(dir);
                fs::create_dir_all(&dir).unwrap();
                screenshot
                    .save(dir.join(format!("menu-case-{case}-{mode_name}.png")))
                    .unwrap();
            }
            cx.update_window(window, |_, w, _| w.remove_window())
                .unwrap();
        }
    }
    println!("menu previews: 12 rendered (both themes; shared production model entries)");
    crate::chat::completion_pixels::render_pixels(cx);
}

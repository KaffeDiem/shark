//! The Artifacts preview pane on its own: one file, drawn by type.
use super::UI_LOCK;
use crate::features::artifacts::preview::{Content, Preview};
use crate::media::Lru;
use core::prelude::v1::test;
use gpui_kit::test::TestWindowExt;
use gpui_kit::*;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::SystemTime;

struct Rig {
    cx: TestAppContext,
    window: AnyWindowHandle,
    preview: Entity<Preview>,
    dir: PathBuf,
}

fn rig(cx: &mut TestAppContext, name: &str) -> Rig {
    cx.update(gpui_kit::init);
    let dir = std::env::temp_dir().join(format!("shark-pv-{name}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".context")).unwrap();
    let preview = Rc::new(std::cell::RefCell::new(None));
    let slot = preview.clone();
    let (window, _) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, move |_, cx| {
            let p = cx.new(|cx| Preview::new(Lru::new(cx)));
            *slot.borrow_mut() = Some(p.clone());
            p
        })
        .unwrap()
    });
    let preview = preview.borrow().clone().unwrap();
    Rig {
        cx: cx.clone(),
        window,
        preview,
        dir,
    }
}

impl Rig {
    fn frames(&mut self) {
        for _ in 0..6 {
            self.cx
                .update_window(self.window, |_, w, cx| w.render_frame(cx))
                .unwrap();
            self.cx.run_until_parked();
        }
    }

    fn with(&mut self, f: impl FnOnce(&mut Preview, &mut Window, &mut Context<Preview>)) {
        let p = self.preview.clone();
        self.cx
            .update_window(self.window, |_, w, cx| p.update(cx, |p, cx| f(p, w, cx)))
            .unwrap();
    }

    /// Starts showing `rel` without letting the load finish.
    fn begin(&mut self, rel: &str) {
        let abs = self.dir.join(".context").join(rel);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        let rel = rel.to_string();
        self.with(move |p, w, cx| p.show(Some((rel, abs)), w, cx));
    }

    fn show(&mut self, rel: &str) {
        self.begin(rel);
        self.frames();
    }

    fn generation(&self) -> u64 {
        self.cx.update(|cx| self.preview.read(cx).generation)
    }

    fn find(&mut self, id: &'static str) -> Option<String> {
        self.cx
            .update_window(self.window, |_, w, _| {
                w.try_find(id).map(|e| e.label().unwrap_or("").to_string())
            })
            .unwrap()
    }
}

#[gpui_kit::test]
fn removal_while_loading_keeps_the_result(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut rig = rig(cx, "removed");
    let abs = rig.dir.join(".context/a.txt");
    std::fs::write(&abs, "hello").unwrap();
    rig.with(|p, w, cx| {
        p.show(Some(("a.txt".into(), abs)), w, cx);
        p.mark_removed(cx);
    });
    rig.frames();
    let state = rig
        .cx
        .update(|cx| matches!(rig.preview.read(cx).content, Content::Text(_)));
    assert!(state, "the in-flight load still lands");
}

#[gpui_kit::test]
fn a_failed_load_is_not_retried_until_the_file_changes(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut rig = rig(cx, "failed");
    let abs = rig.dir.join(".context/a.txt");
    std::fs::write(&abs, "x").unwrap();
    std::fs::set_permissions(&abs, std::fs::Permissions::from_mode(0o000)).unwrap();
    rig.show("a.txt");
    let failed = rig
        .cx
        .update(|cx| matches!(rig.preview.read(cx).content, Content::Failed(_)));
    assert!(failed);
    let before = rig.generation();
    rig.with(|p, w, cx| p.reload_if_changed(w, cx));
    let after = rig.generation();
    std::fs::set_permissions(&abs, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(before, after, "an unchanged failing file is left alone");
}

#[gpui_kit::test]
fn a_poll_during_the_first_load_does_not_restart_it(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut rig = rig(cx, "inflight");
    std::fs::write(rig.dir.join(".context/a.txt"), "hello").unwrap();
    rig.begin("a.txt");
    let before = rig.generation();
    rig.with(|p, w, cx| p.reload_if_changed(w, cx));
    assert_eq!(rig.generation(), before, "the load in flight is kept");
    rig.frames();
    assert!(
        rig.cx
            .update(|cx| matches!(rig.preview.read(cx).content, Content::Text(_)))
    );
}

#[gpui_kit::test]
fn reload_keeps_the_scroll_of_text_and_markdown(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut rig = rig(cx, "scroll");
    let body: String = (0..400).map(|i| format!("line {i}\n\n")).collect();
    for rel in ["a.txt", "a.md"] {
        std::fs::write(rig.dir.join(".context").join(rel), &body).unwrap();
        rig.show(rel);
        let scroll = |rig: &Rig| {
            rig.cx.update(|cx| match &rig.preview.read(cx).content {
                Content::Text(i) => i.read(cx).scroll_offset().y.as_f32(),
                Content::Doc(d) => {
                    let o = d.read(cx).list().unwrap().logical_scroll_top();
                    o.item_ix as f32 * 1000. + o.offset_in_item.as_f32()
                }
                _ => panic!("not loaded"),
            })
        };
        rig.cx.update(|cx| {
            let (input, doc) = match &rig.preview.read(cx).content {
                Content::Text(i) => (Some(i.clone()), None),
                Content::Doc(d) => (None, Some(d.clone())),
                _ => panic!("not loaded"),
            };
            if let Some(i) = input {
                i.update(cx, |i, cx| {
                    i.set_scroll_offset(point(px(0.), px(-300.)), cx)
                });
            }
            if let Some(d) = doc {
                d.read(cx).list().unwrap().scroll_to(ListOffset {
                    item_ix: 5,
                    offset_in_item: px(0.),
                });
            }
        });
        rig.frames();
        let at = scroll(&rig);
        assert_ne!(at, 0., "{rel} scrolled");
        std::fs::write(rig.dir.join(".context").join(rel), format!("{body}more\n")).unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(rig.dir.join(".context").join(rel))
            .unwrap()
            .set_modified(later)
            .unwrap();
        rig.with(|p, w, cx| p.reload_if_changed(w, cx));
        rig.frames();
        let after = scroll(&rig);
        assert_ne!(after, 0., "{rel} did not jump to the top");
        assert_eq!(after, at, "{rel} keeps its scroll");
    }
}

#[gpui_kit::test]
fn toolbar_icon_buttons_have_names(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut rig = rig(cx, "names");
    std::fs::write(rig.dir.join(".context/a.txt"), "x").unwrap();
    rig.show("a.txt");
    assert_eq!(rig.find("artifact-open").as_deref(), Some("Open in editor"));
    assert_eq!(rig.find("artifact-more").as_deref(), Some("More actions"));
}

#[gpui_kit::test]
fn a_binary_file_has_no_preview(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut rig = rig(cx, "binary");
    std::fs::write(rig.dir.join(".context/a.bin"), [0u8, 1, 2, 3]).unwrap();
    rig.show("a.bin");
    assert_eq!(
        rig.find("artifact-state").as_deref(),
        Some("No preview for this file type.")
    );
    assert!(rig.find("artifact-open-in").is_some());
    assert!(rig.find("artifact-reveal").is_some());
}

#[gpui_kit::test]
fn a_pdf_has_no_preview(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut rig = rig(cx, "pdf");
    std::fs::write(rig.dir.join(".context/a.pdf"), b"%PDF-1.4 text").unwrap();
    rig.show("a.pdf");
    assert_eq!(
        rig.find("artifact-state").as_deref(),
        Some("No preview for this file type.")
    );
}

#[gpui_kit::test]
fn a_text_file_with_any_name_previews(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut rig = rig(cx, "anytext");
    for rel in ["Makefile", "a.lock", "events.jsonl"] {
        std::fs::write(rig.dir.join(".context").join(rel), "hello\n").unwrap();
        rig.show(rel);
        assert!(
            rig.cx
                .update(|cx| matches!(rig.preview.read(cx).content, Content::Text(_))),
            "{rel} previews as text"
        );
    }
}

#[gpui_kit::test]
fn a_big_text_file_is_too_large(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut rig = rig(cx, "big");
    let f = std::fs::File::create(rig.dir.join(".context/big.txt")).unwrap();
    f.set_len(1024 * 1024 + 1).unwrap();
    rig.show("big.txt");
    let msg = rig.find("artifact-state").unwrap();
    assert!(msg.starts_with("Too large to preview ("), "{msg}");
    assert!(rig.find("artifact-too-large-open").is_some());
    assert!(rig.find("artifact-too-large-open-in").is_some());
}

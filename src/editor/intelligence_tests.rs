use super::*;
use crate::lsp::{
    Session,
    client::Client,
    tools::{Backend, Project},
};
use core::prelude::v1::test;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use serde_json::{Value, json};
use std::{fs, process::Command};

// GPUI's deterministic scheduler rejects wakers invoked by real OS threads. Keep the
// real peer's waits on an ordinary waker; the editor still uses the test scheduler.
fn peer_wait<T>(future: impl std::future::Future<Output = T>) -> T {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: std::sync::Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(std::sync::Arc::new(Unpark(std::thread::current())));
    let mut context = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => return value,
            std::task::Poll::Pending => std::thread::park_timeout(Duration::from_secs(1)),
        }
    }
}

fn fake_client(root: &std::path::Path, log: &std::path::Path) -> Client {
    let mut command = Command::new("/usr/bin/python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/lsp/client_tests/server.py"
        ))
        .arg(log);
    Client::start(command, root, json!({})).unwrap()
}

fn messages(log: &std::path::Path) -> Vec<Value> {
    fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

async fn open_editor(
    tag: &str,
    contents: &str,
    cx: &mut TestAppContext,
) -> (PathBuf, PathBuf, AnyWindowHandle, Entity<FileEditor>) {
    let dir = std::env::temp_dir().join(format!("shark-lsp-ui-{tag}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("a.py");
    fs::write(&file, contents).unwrap();
    cx.update(gpui_kit::init);
    let abs = file.clone();
    let (window, editor) = cx
        .update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                cx.new(|cx| FileEditor::new("r/a.py".into(), abs, true, w, cx))
            })
        })
        .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor.read(cx).loaded
    })
    .await;
    (dir, file, window, editor)
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn language_replacements_preserve_selection_scroll_and_one_vim_undo(cx: &mut TestAppContext) {
    let _turn = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let original = "😀 first\nsecond\nthird\n";
    let (dir, file, window, editor) = open_editor("undo", original, cx).await;
    cx.update_window(window, |_, w, cx| {
        focus(&editor, w, cx);
        editor.update(cx, |e, cx| {
            e.input.update(cx, |input, cx| {
                input.set_selected_range(5..10, cx);
                input.set_scroll_offset(point(px(-4.), px(-16.)), cx);
            });
            let scroll = e.input.read(cx).scroll_offset();
            e.replace_language_text("😀 FIRST\nSECOND\nTHIRD\n".into(), w, cx);
            assert_eq!(e.input.read(cx).selected_range(), 5..10);
            assert_eq!(e.input.read(cx).scroll_offset(), scroll);
            assert!(e.dirty);
            assert_eq!(e.vim.mode, vim::Mode::Normal);
        });
    })
    .unwrap();
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        original,
        "language edits must stay unsaved"
    );
    cx.update_window(window, |_, w, cx| {
        w.press("u", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            editor.read(cx).input.read(cx).value().as_ref(),
            original,
            "one Vim undo must restore the whole refactor"
        )
    });
    cx.update_window(window, |_, w, cx| {
        w.press("ctrl-r", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            editor.read(cx).input.read(cx).value().as_ref(),
            "😀 FIRST\nSECOND\nTHIRD\n"
        )
    });
    let _ = fs::remove_dir_all(dir);
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn fake_server_syncs_unicode_rapid_edits_save_reload_status_and_restart(
    cx: &mut TestAppContext,
) {
    let _turn = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let initial = "name = '🦈'\r\n";
    let (dir, file, window, editor) = open_editor("sync", initial, cx).await;
    let log = dir.join("server.jsonl");
    let client = fake_client(&dir, &log);
    peer_wait(client.initialize()).unwrap();
    let project = Project {
        root: dir.clone(),
        worktree: dir.clone(),
        language_id: "python",
        backend: Backend::Pyrefly,
        toolchain: String::new(),
        rust_toolchain: None,
        interpreter: None,
    };
    let session = cx.update(|cx| cx.new(|_| Session::with_client(project, client.clone())));
    cx.update_window(window, |_, w, cx| {
        editor.update(cx, |e, cx| e.attach_language(session.clone(), w, cx))
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor
            .read(cx)
            .intelligence
            .as_ref()
            .unwrap()
            .snapshot
            .lock()
            .unwrap()
            .text
            .as_ref()
            == initial
    })
    .await;
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    assert_eq!(
        peer_wait(client.request(
            "textDocument/completion",
            json!({"textDocument":{"uri":uri}})
        ))
        .unwrap(),
        initial
    );

    // A readiness/progress repaint must not invent a document version.
    let version = cx.update(|cx| {
        editor
            .read(cx)
            .intelligence
            .as_ref()
            .unwrap()
            .snapshot
            .lock()
            .unwrap()
            .version
    });
    cx.update(|cx| {
        session.update(cx, |s, cx| {
            s.status = "Preparing index".into();
            cx.notify();
        })
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            editor
                .read(cx)
                .intelligence
                .as_ref()
                .unwrap()
                .snapshot
                .lock()
                .unwrap()
                .version,
            version
        )
    });

    let final_text = "name = '🌊é'\r\nprint(name)\r\n";
    cx.update_window(window, |_, w, cx| {
        editor.update(cx, |e, cx| {
            for text in ["name = 'é'\r\n", "name = '🌊'\r\n", final_text] {
                e.replace_language_text(text.into(), w, cx);
            }
            // Save before the debounce has fired; didChange must precede didSave.
            e.save(false, w, cx);
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        let e = editor.read(cx);
        !e.saving
            && !e.dirty
            && e.intelligence
                .as_ref()
                .unwrap()
                .snapshot
                .lock()
                .unwrap()
                .text
                .as_ref()
                == final_text
    })
    .await;
    assert_eq!(
        peer_wait(client.request(
            "textDocument/completion",
            json!({"textDocument":{"uri":uri}})
        ))
        .unwrap(),
        final_text
    );
    cx.wait_for(window, Duration::from_secs(10), |_, _| {
        messages(&log)
            .iter()
            .any(|v| v["method"] == "textDocument/didSave")
    })
    .await;
    let protocol = messages(&log);
    let change = protocol
        .iter()
        .rposition(|v| v["method"] == "textDocument/didChange")
        .unwrap();
    let save = protocol
        .iter()
        .position(|v| v["method"] == "textDocument/didSave")
        .unwrap();
    assert!(change < save);
    assert_eq!(protocol[save]["params"]["text"], final_text);

    let disk_text = "agent = '中文😀'\r\n";
    fs::write(&file, disk_text).unwrap();
    cx.update_window(window, |_, w, cx| {
        editor.update(cx, |e, cx| e.reload(w, cx))
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor
            .read(cx)
            .intelligence
            .as_ref()
            .unwrap()
            .snapshot
            .lock()
            .unwrap()
            .text
            .as_ref()
            == disk_text
    })
    .await;
    assert_eq!(
        peer_wait(client.request(
            "textDocument/completion",
            json!({"textDocument":{"uri":uri}})
        ))
        .unwrap(),
        disk_text
    );

    let restarted_log = dir.join("restarted.jsonl");
    let restarted = fake_client(&dir, &restarted_log);
    peer_wait(restarted.initialize()).unwrap();
    cx.update(|cx| {
        session.update(cx, |s, cx| {
            s.client = Some(restarted.clone());
            s.generation += 1;
            cx.notify();
        })
    });
    cx.wait_for(window, Duration::from_secs(10), |_, _| {
        messages(&restarted_log)
            .iter()
            .any(|v| v["method"] == "textDocument/didOpen")
    })
    .await;
    assert_eq!(
        peer_wait(restarted.request(
            "textDocument/completion",
            json!({"textDocument":{"uri":uri}})
        ))
        .unwrap(),
        disk_text
    );
    peer_wait(client.request("test/echo", Value::Null)).unwrap();
    assert!(
        messages(&log)
            .iter()
            .any(|v| v["method"] == "textDocument/didClose")
    );

    // Cancel a queued debounce and close; no late didChange may follow didClose.
    cx.update_window(window, |_, w, cx| {
        editor.update(cx, |e, cx| {
            e.replace_language_text("closing = '😀'\n".into(), w, cx);
            e.intelligence = None;
        })
    })
    .unwrap();
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    peer_wait(restarted.request("test/echo", Value::Null)).unwrap();
    let protocol = messages(&restarted_log);
    let closed = protocol
        .iter()
        .rposition(|v| v["method"] == "textDocument/didClose")
        .unwrap();
    assert!(
        !protocol[closed + 1..]
            .iter()
            .any(|v| v["method"] == "textDocument/didChange")
    );
    client.shutdown();
    restarted.shutdown();
    let _ = fs::remove_dir_all(dir);
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn completion_acceptance_applies_autoimport_snippet_mirrors_and_undo(
    cx: &mut TestAppContext,
) {
    let _turn = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::lsp::{document::Encoding, providers::prepare_completion};
    use lsp_types::{
        CompletionItem, CompletionTextEdit, InsertTextFormat, Position, Range, TextEdit,
    };
    let initial = "# 😀\nsq";
    let (dir, file, window, editor) = open_editor("completion", initial, cx).await;
    let client = fake_client(&dir, &dir.join("peer.jsonl"));
    peer_wait(client.initialize()).unwrap();
    let project = Project {
        root: dir.clone(),
        worktree: dir.clone(),
        language_id: "python",
        backend: Backend::Pyrefly,
        toolchain: String::new(),
        interpreter: None,
        rust_toolchain: None,
    };
    let session = cx.update(|cx| cx.new(|_| Session::with_client(project, client.clone())));
    cx.update_window(window, |_, w, cx| {
        editor.update(cx, |e, cx| {
            e.set_vim(false, cx);
            e.attach_language(session.clone(), w, cx);
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor
            .read(cx)
            .intelligence
            .as_ref()
            .unwrap()
            .snapshot
            .lock()
            .unwrap()
            .text
            .as_ref()
            == initial
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        editor.update(cx, |e, cx| {
            let snapshot = e
                .intelligence
                .as_ref()
                .unwrap()
                .snapshot
                .lock()
                .unwrap()
                .clone();
            let item = CompletionItem {
                label: "sqrt".into(),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(
                    Range::new(Position::new(1, 0), Position::new(1, 2)),
                    "sqrt(${1:arg}, $1, ${2:next})$0".into(),
                ))),
                additional_text_edits: Some(vec![TextEdit::new(
                    Range::new(Position::new(0, 0), Position::new(0, 0)),
                    "from math import sqrt\n".into(),
                )]),
                ..Default::default()
            };
            let item = prepare_completion(&snapshot, initial.len(), item, Encoding::Utf16).unwrap();
            e.input.update(cx, |input, cx| {
                input.set_selected_range(initial.len()..initial.len(), cx);
                input.present_completion_items(initial.len() - 2, "sq", vec![item.clone()], cx);
                input.insert_completion(&item, initial.len() - 2..initial.len(), w, cx);
            });
        })
    })
    .unwrap();
    cx.run_until_parked();
    let accepted = "from math import sqrt\n# 😀\nsqrt(arg, arg, next)";
    cx.update(|cx| {
        let input = editor.read(cx).input.read(cx);
        assert_eq!(input.value().as_ref(), accepted);
        assert_eq!(input.selected_value().as_ref(), "arg");
    });
    assert_eq!(fs::read_to_string(&file).unwrap(), initial);
    cx.update_window(window, |_, w, cx| w.input("🌊", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            editor.read(cx).input.read(cx).value().as_ref(),
            "from math import sqrt\n# 😀\nsqrt(🌊, 🌊, next)"
        )
    });
    cx.update_window(window, |_, w, cx| w.press("tab", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            editor.read(cx).input.read(cx).selected_value().as_ref(),
            "next"
        )
    });
    cx.update_window(window, |_, w, cx| w.press("shift-tab", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            editor.read(cx).input.read(cx).selected_value().as_ref(),
            "🌊"
        )
    });
    cx.update_window(window, |_, w, cx| w.press("cmd-z", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(editor.read(cx).input.read(cx).value().as_ref(), accepted));
    cx.update_window(window, |_, w, cx| w.press("cmd-z", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            editor.read(cx).input.read(cx).value().as_ref(),
            initial,
            "completion and autoimport must undo together"
        )
    });
    client.terminate();
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn outline_nests_both_symbol_shapes_and_follows_the_cursor() {
    use super::intelligence::{outline, symbol_path};
    use crate::lsp::document::Encoding;
    let text = "struct A {\n    fn b() {}\n}\nfn c() {}\n";
    let range = |l0: u32, c0: u32, l1: u32, c1: u32| json!({"start":{"line":l0,"character":c0},"end":{"line":l1,"character":c1}});
    let nested = json!([
        {"name":"A","range":range(0,0,2,1),"selectionRange":range(0,7,0,8),"children":[
            {"name":"b","range":range(1,4,1,13),"selectionRange":range(1,7,1,8)}
        ]},
        {"name":"c","range":range(3,0,3,9),"selectionRange":range(3,3,3,4)}
    ]);
    // SymbolInformation is flat and in any order; ranges nest it the same way.
    let flat = json!([
        {"name":"c","location":{"uri":"file:///x","range":range(3,0,3,9)}},
        {"name":"b","location":{"uri":"file:///x","range":range(1,4,1,13)}},
        {"name":"A","location":{"uri":"file:///x","range":range(0,0,2,1)}}
    ]);
    let tree = |o: &[super::intelligence::Symbol]| {
        o.iter()
            .map(|s| (s.name.clone(), s.depth))
            .collect::<Vec<_>>()
    };
    let symbols = outline(&nested, text, Encoding::Utf16);
    let expected = vec![("A".into(), 0), ("b".into(), 1), ("c".into(), 0)];
    assert_eq!(tree(&symbols), expected);
    assert_eq!(tree(&outline(&flat, text, Encoding::Utf16)), expected);
    assert_eq!(symbols[1].target, text.find("b()").unwrap());
    assert_eq!(symbol_path(&symbols, text.find("{}").unwrap()), vec![0, 1]);
    assert_eq!(symbol_path(&symbols, text.find("c()").unwrap()), vec![2]);
    assert_eq!(symbol_path(&symbols, text.len()), Vec::<usize>::new());
}

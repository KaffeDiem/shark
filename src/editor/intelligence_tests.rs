use super::*;
use crate::lsp::test_support::{block_on, fake_server, logged};
use crate::lsp::{
    Session,
    client::Client,
    tools::{Backend, Project},
};
use core::prelude::v1::test;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use serde_json::{Value, json};
use std::fs;

fn fake_client(root: &std::path::Path, log: &std::path::Path) -> Client {
    Client::start(fake_server(log), root, json!({})).unwrap()
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn language_replacements_preserve_selection_scroll_and_one_vim_undo(cx: &mut TestAppContext) {
    let _turn = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let original = "😀 first\nsecond\nthird\n";
    let (dir, file, window, editor) = ui_tests::open_editor("a.py", "undo", original, cx).await;
    cx.update_window(window, |_, w, cx| {
        focus(&editor, w, cx);
        editor.update(cx, |e, cx| {
            e.input.update(cx, |input, cx| {
                input.set_selected_range(5..10, cx);
                input.set_scroll_offset(point(px(-4.), px(-16.)), cx);
            });
            let scroll = e.input.read(cx).scroll_offset();
            // An added line above moves the selection with its code.
            e.replace_language_text("# a\n😀 FIRST\nSECOND\nTHIRD\n".into(), w, cx);
            assert_eq!(e.input.read(cx).selected_value().as_ref(), "FIRST");
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
            "# a\n😀 FIRST\nSECOND\nTHIRD\n"
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
    let (dir, file, window, editor) = ui_tests::open_editor("a.py", "sync", initial, cx).await;
    let log = dir.join("server.jsonl");
    let client = fake_client(&dir, &log);
    block_on(client.initialize()).unwrap();
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
        block_on(client.request(
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
        block_on(client.request(
            "textDocument/completion",
            json!({"textDocument":{"uri":uri}})
        ))
        .unwrap(),
        final_text
    );
    cx.wait_for(window, Duration::from_secs(10), |_, _| {
        logged(&log)
            .iter()
            .any(|v| v["method"] == "textDocument/didSave")
    })
    .await;
    let protocol = logged(&log);
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
        block_on(client.request(
            "textDocument/completion",
            json!({"textDocument":{"uri":uri}})
        ))
        .unwrap(),
        disk_text
    );

    let restarted_log = dir.join("restarted.jsonl");
    let restarted = fake_client(&dir, &restarted_log);
    block_on(restarted.initialize()).unwrap();
    cx.update(|cx| {
        editor.update(cx, |e, _| {
            e.lsp_error = Some("Old synchronization failure".into())
        });
        session.update(cx, |s, cx| {
            s.client = Some(restarted.clone());
            s.generation += 1;
            s.status = "Ready".into();
            cx.notify();
        })
    });
    cx.wait_for(window, Duration::from_secs(10), |_, _| {
        logged(&restarted_log)
            .iter()
            .any(|v| v["method"] == "textDocument/didOpen")
    })
    .await;
    cx.update(|cx| {
        let e = editor.read(cx);
        assert!(e.lsp_error.is_none());
        assert_eq!(e.language_status(cx).unwrap().0, "Ready");
    });
    assert_eq!(
        block_on(restarted.request(
            "textDocument/completion",
            json!({"textDocument":{"uri":uri}})
        ))
        .unwrap(),
        disk_text
    );
    block_on(client.request("test/echo", Value::Null)).unwrap();
    assert!(
        logged(&log)
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
    block_on(restarted.request("test/echo", Value::Null)).unwrap();
    let protocol = logged(&restarted_log);
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
async fn diagnostics_from_both_servers_remain_searchable_in_document_order(
    cx: &mut TestAppContext,
) {
    let _turn = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let text = "import os\nname = 1\nvalue = name\n";
    let (dir, file, window, editor) =
        ui_tests::open_editor("a.py", "diagnostics-order", text, cx).await;
    let client = fake_client(&dir, &dir.join("primary.jsonl"));
    let companion = fake_client(&dir, &dir.join("ruff.jsonl"));
    block_on(client.initialize()).unwrap();
    block_on(companion.initialize()).unwrap();
    let project = Project {
        root: dir.clone(),
        worktree: dir.clone(),
        language_id: "python",
        backend: Backend::Pyrefly,
        toolchain: String::new(),
        rust_toolchain: None,
        interpreter: None,
    };
    let session = cx.update(|cx| {
        cx.new(|_| {
            let mut session = Session::with_client(project, client.clone());
            session.companion = Some(companion.clone());
            session
        })
    });
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
            == text
    })
    .await;
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    for (is_companion, lines) in [(false, vec![2, 1]), (true, vec![0])] {
        // The companion re-serializes the URI (`a` as `%61`); it is still this document.
        let uri = if is_companion {
            uri.replace("a.py", "%61.py")
        } else {
            uri.clone()
        };
        cx.update(|cx| {
            session.update(cx, |_, cx| {
                let diagnostics: Vec<_> = lines.iter().map(|line| json!({
                    "range":{"start":{"line":line,"character":0},"end":{"line":line,"character":4}},
                    "message":format!("line {line}")
                })).collect();
                cx.emit(crate::lsp::SessionEvent::Notification {
                    method: "textDocument/publishDiagnostics".into(),
                    params: json!({"uri":uri,"diagnostics":diagnostics}),
                    companion: is_companion,
                });
            });
        });
        cx.run_until_parked();
    }
    cx.update(|cx| {
        let input = editor.read(cx).input.read(cx);
        let set = input.diagnostics().unwrap();
        for (offset, message) in [(1, "line 0"), (11, "line 1"), (20, "line 2")] {
            assert_eq!(set.for_offset(offset).unwrap().message.as_ref(), message);
        }
    });
    client.shutdown();
    companion.shutdown();
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn publish_diagnostics_uris_match_by_decoded_path() {
    use super::intelligence::{file_uri, same_file};
    // Our own URI for a Next.js route file must be representable at all (`[id]`).
    let ours = file_uri(std::path::Path::new("/p/app/(shop)/[id]/page.tsx")).unwrap();
    assert!(same_file(
        "file:///p/app/%28shop%29/%5Bid%5D/page.tsx",
        &ours
    ));
    assert!(same_file(ours.as_str(), &ours));
    assert!(!same_file("file:///p/app/(shop)/[id]/other.tsx", &ours));
    assert!(!same_file("untitled:page.tsx", &ours));
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
    let (dir, file, window, editor) =
        ui_tests::open_editor("a.py", "completion", initial, cx).await;
    let client = fake_client(&dir, &dir.join("peer.jsonl"));
    block_on(client.initialize()).unwrap();
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
            let lines = crate::lsp::document::Lines::new(&snapshot.text);
            let item = prepare_completion(&snapshot, &lines, initial.len(), item, Encoding::Utf16)
                .unwrap();
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

// The menu shows most items unresolved; accepting one resolves it and adds its import.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn accepting_an_unresolved_completion_adds_its_import_as_a_second_undo_step(
    cx: &mut TestAppContext,
) {
    let _turn = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::lsp::{document::Encoding, providers::prepare_completion};
    use lsp_types::{CompletionItem, CompletionTextEdit, Position, Range, TextEdit};
    let initial = "# resolve-completion\nsq";
    let (dir, _, window, editor) =
        ui_tests::open_editor("a.py", "resolve-accept", initial, cx).await;
    let client = fake_client(&dir, &dir.join("peer.jsonl"));
    block_on(client.initialize()).unwrap();
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
        (editor.read(cx).intelligence.as_ref()).is_some_and(|l| {
            l.client.is_some() && l.snapshot.lock().unwrap().text.as_ref() == initial
        })
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        editor.update(cx, |e, cx| {
            let snapshot = (e.intelligence.as_ref().unwrap().snapshot.lock().unwrap()).clone();
            let item = CompletionItem {
                label: "sqrt".into(),
                data: Some(serde_json::json!({"token":"silent"})),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(
                    Range::new(Position::new(1, 0), Position::new(1, 2)),
                    "sqrt".into(),
                ))),
                ..Default::default()
            };
            let original = serde_json::to_value(&item).unwrap();
            let lines = crate::lsp::document::Lines::new(&snapshot.text);
            let mut item =
                prepare_completion(&snapshot, &lines, initial.len(), item, Encoding::Utf16)
                    .unwrap();
            // What the completion provider attaches to an item it showed unresolved.
            item.data.as_mut().unwrap()["sharkResolve"] = original;
            e.input.update(cx, |input, cx| {
                input.set_selected_range(initial.len()..initial.len(), cx);
                input.present_completion_items(initial.len() - 2, "sq", vec![item.clone()], cx);
                input.insert_completion(&item, initial.len() - 2..initial.len(), w, cx);
            });
        })
    })
    .unwrap();
    cx.run_until_parked();
    let log = dir.join("peer.jsonl");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !(logged(&log).iter()).any(|m| {
        m["method"] == "completionItem/resolve" && m["params"]["data"]["token"] == "silent"
    }) {
        assert!(
            std::time::Instant::now() < deadline,
            "the accepted item was not resolved"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // The resolve reply's import, applied as the response handler does.
    cx.update_window(window, |_, w, cx| {
        editor.update(cx, |e, cx| {
            let import = TextEdit::new(
                Range::new(Position::new(0, 0), Position::new(0, 0)),
                "from math import sqrt\n".into(),
            );
            e.apply_completion_imports(
                initial,
                initial.len() - 2,
                &[import],
                Encoding::Utf16,
                w,
                cx,
            )
        })
    })
    .unwrap();
    let imported = "from math import sqrt\n# resolve-completion\nsqrt";
    cx.update(|cx| {
        assert_eq!(editor.read(cx).input.read(cx).value().as_ref(), imported);
        assert!(editor.read(cx).snippets.resolve.is_some());
    });
    cx.update_window(window, |_, w, cx| w.press("cmd-z", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let e = editor.read(cx);
        assert_eq!(
            e.input.read(cx).value().as_ref(),
            "# resolve-completion\nsqrt"
        );
        // Undo cancels the unanswered resolve, so a late reply cannot re-add an import.
        assert!(e.snippets.resolve.is_none());
    });
    client.terminate();
    let _ = fs::remove_dir_all(dir);
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn snippet_completion_inside_an_active_field_starts_its_own_tabstops(
    cx: &mut TestAppContext,
) {
    let _turn = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::lsp::{
        document::{Encoding, Lines},
        providers::prepare_completion,
    };
    use lsp_types::{CompletionItem, CompletionTextEdit, InsertTextFormat, Position, Range};
    let (dir, _, window, editor) = ui_tests::open_editor("a.py", "nested-snippet", "f", cx).await;
    let client = fake_client(&dir, &dir.join("peer.jsonl"));
    block_on(client.initialize()).unwrap();
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
    // Accepts `snippet` over `range` once the server has the buffer's current text.
    let accept = async |range: Range, snippet: &str, cx: &mut TestAppContext| {
        let text = cx.update(|cx| editor.read(cx).input.read(cx).value().to_string());
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            let e = editor.read(cx);
            e.intelligence
                .as_ref()
                .unwrap()
                .snapshot
                .lock()
                .unwrap()
                .text
                .as_ref()
                == text
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
                    label: snippet.into(),
                    insert_text_format: Some(InsertTextFormat::SNIPPET),
                    text_edit: Some(CompletionTextEdit::Edit(lsp_types::TextEdit::new(
                        range,
                        snippet.into(),
                    ))),
                    ..Default::default()
                };
                let lines = Lines::new(&snapshot.text);
                let cursor = e.input.read(cx).cursor();
                let item =
                    prepare_completion(&snapshot, &lines, cursor, item, Encoding::Utf16).unwrap();
                e.input.update(cx, |input, cx| {
                    input.present_completion_items(cursor, "", vec![item.clone()], cx);
                    input.insert_completion(&item, cursor..cursor, w, cx);
                });
            })
        })
        .unwrap();
        cx.run_until_parked();
    };
    let range = |a: u32, b: u32| Range::new(Position::new(0, a), Position::new(0, b));
    accept(range(0, 1), "foo(${1:arg}, ${2:y})$0", cx).await;
    cx.update(|cx| {
        assert_eq!(
            editor.read(cx).input.read(cx).selected_value().as_ref(),
            "arg"
        )
    });
    // A plain completion in a field keeps the outer session.
    accept(range(4, 7), "value", cx).await;
    cx.update_window(window, |_, w, cx| w.press("tab", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let input = editor.read(cx).input.read(cx);
        assert_eq!(input.value().as_ref(), "foo(value, y)");
        assert_eq!(input.selected_value().as_ref(), "y");
    });
    accept(range(11, 12), "bar(${1:x})", cx).await;
    cx.update(|cx| {
        let input = editor.read(cx).input.read(cx);
        assert_eq!(input.value().as_ref(), "foo(value, bar(x))");
        assert_eq!(input.selected_value().as_ref(), "x");
    });
    // Without $0 the last Tab leaves the snippet after its closing parenthesis.
    cx.update_window(window, |_, w, cx| w.press("tab", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let input = editor.read(cx).input.read(cx);
        assert_eq!(input.value().as_ref(), "foo(value, bar(x))");
        assert_eq!(input.selected_range(), 17..17);
    });
    client.terminate();
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn language_edits_keep_the_selection_on_the_same_code() {
    use super::intelligence::map_range;
    // Rename: longer names before the caret, above it and on its line.
    let (old, new) = ("x = 1\nprint(x, x)\n", "value = 1\nprint(value, value)\n");
    let x = old.rfind('x').unwrap();
    let value = new.rfind("value").unwrap();
    assert_eq!(map_range(old, new, x..x + 1), value..value + 5);
    // Format: an added import and re-indented lines.
    let old = "import a\nif a:\n  run()\n";
    let new = "import a\nimport b\nif a:\n    run()\n";
    let run = |s: &str| s.find("run").unwrap();
    assert_eq!(map_range(old, new, run(old)..run(old)), run(new)..run(new));
    assert_eq!(
        map_range(old, new, old.len()..old.len()),
        new.len()..new.len()
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn vim_moves_but_never_edits_in_external_sources(cx: &mut TestAppContext) {
    let _turn = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let text = "first\nsecond\n";
    let (dir, _, window, editor) = ui_tests::open_editor("a.py", "external-vim", text, cx).await;
    cx.update_window(window, |_, w, cx| {
        focus(&editor, w, cx);
        editor.update(cx, |e, cx| e.set_external(true, cx));
        for key in ["j", "x", "d", "d", "i", "o", "u", "l"] {
            w.press(key, cx);
        }
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let e = editor.read(cx);
        assert_eq!(e.input.read(cx).value().as_ref(), text);
        assert_eq!(e.vim.mode, vim::Mode::Normal);
        assert_eq!(e.vim.pending(), "");
        assert_eq!(e.input.read(cx).cursor(), "first\ns".len());
    });
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

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn escape_dismisses_language_menus_before_leaving_editor(cx: &mut TestAppContext) {
    use gpui_kit::base::input::CodeActionItem;
    let _guard = crate::app::ui_tests::UI_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (dir, _, window, editor) =
        ui_tests::open_editor("a.txt", "escape-overlays", "bad\n", cx).await;
    let leaves = std::rc::Rc::new(std::cell::Cell::new(0));
    let events = leaves.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&editor, move |_, event: &EditorEvent, _| {
            if matches!(event, EditorEvent::Leave) {
                events.set(events.get() + 1);
            }
        })
    });
    for vim in [false, true] {
        for quick_fix in [false, true] {
            let before = leaves.get();
            cx.update_window(window, |_, w, cx| {
                editor.update(cx, |e, cx| {
                    e.set_vim(vim, cx);
                    e.input.update(cx, |input, cx| {
                        input.focus(w, cx);
                        if quick_fix {
                            input.present_code_actions(
                                vec![CodeActionItem {
                                    provider_id: "test".into(),
                                    action: lsp_types::CodeAction {
                                        title: "Fix diagnostic".into(),
                                        ..Default::default()
                                    },
                                }],
                                cx,
                            );
                        } else {
                            input.present_completion_items(
                                0,
                                "b",
                                vec![lsp_types::CompletionItem::new_simple(
                                    "better".into(),
                                    "Complete".into(),
                                )],
                                cx,
                            );
                        }
                    });
                });
                w.render_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(window, |_, w, cx| {
                w.render_frame(cx);
                let input = editor.read(cx).input.read(cx);
                assert_eq!(input.code_action_menu_state().open, quick_fix);
                assert_eq!(input.completion_menu_state().open, !quick_fix);
                w.press("escape", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(window, |_, w, cx| {
                let e = editor.read(cx);
                let input = e.input.read(cx);
                assert!(!input.code_action_menu_state().open);
                assert!(!input.completion_menu_state().open);
                assert!(e.is_focused(w, cx));
                assert_eq!(
                    leaves.get(),
                    before,
                    "Escape dismisses the menu without leaving"
                );
                assert_eq!(input.value().as_ref(), "bad\n");
                w.press("escape", cx);
            })
            .unwrap();
            cx.run_until_parked();
            assert_eq!(
                leaves.get(),
                before + 1,
                "a second Escape leaves the idle editor"
            );
        }
    }
    let _ = fs::remove_dir_all(dir);
}

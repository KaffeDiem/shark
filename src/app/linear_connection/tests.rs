use super::*;
use crate::app::ui_tests::{UI_LOCK, boot};
use crate::linear::fake;
use crate::state::State;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

const KEY: &str = "lin_api_SECRET_KEY";
const OTHER: &str = "lin_api_OTHER_KEY";

/// The Keychain and the event log. Tests take turns on `UI_LOCK`, so one is enough.
static STORED: Mutex<Option<Credential>> = Mutex::new(None);
static LOG: Mutex<Vec<&str>> = Mutex::new(vec![]);
static WRITE_FAILS: AtomicBool = AtomicBool::new(false);
static DELETE_ERROR: Mutex<Option<&str>> = Mutex::new(None);
static HTTP_CALLS: AtomicUsize = AtomicUsize::new(0);

fn stored() -> Option<Credential> {
    STORED.lock().unwrap().clone()
}

fn log(event: &'static str) {
    LOG.lock().unwrap().push(event);
}

fn events() -> Vec<&'static str> {
    LOG.lock().unwrap().clone()
}

fn fake_keychain() -> Keychain {
    Keychain {
        read: |_| {
            log("read");
            Task::ready(Ok(STORED.lock().unwrap().clone()))
        },
        write: |_, c| {
            log("write");
            if WRITE_FAILS.load(Ordering::Relaxed) {
                return Task::ready(Err(anyhow::anyhow!("keychain Update failed")));
            }
            *STORED.lock().unwrap() = Some(c.clone());
            Task::ready(Ok(()))
        },
        delete: |_| {
            log("delete");
            if let Some(e) = *DELETE_ERROR.lock().unwrap() {
                return Task::ready(Err(anyhow::anyhow!(e)));
            }
            *STORED.lock().unwrap() = None;
            Task::ready(Ok(()))
        },
    }
}

fn viewer(org: &str) -> String {
    format!(
        r#"{{"data":{{"viewer":{{"id":"u","name":"Ada"}},"organization":{{"id":"{org}-uuid","name":"{org}","urlKey":"{org}"}}}}}}"#
    )
}

/// Linear as `validate` sees it, answering with `ANSWER`.
fn http() -> &'static Http<'static> {
    &|_| {
        log("validate");
        HTTP_CALLS.fetch_add(1, Ordering::Relaxed);
        match *ANSWER.lock().unwrap() {
            Some(org) => fake::resp(200, &viewer(org)),
            None => fake::resp(401, "{}"),
        }
    }
}

/// The organization the next validation reports; None refuses the credential.
static ANSWER: Mutex<Option<&str>> = Mutex::new(Some("acme"));

fn answer(org: Option<&'static str>) {
    *ANSWER.lock().unwrap() = org;
}

/// Not `test_home`: its wipe races the `linear` snapshot tests, and `boot` replaces state.json.
fn setup(cx: &mut TestAppContext, linear: Option<Connection>) -> (AnyWindowHandle, Entity<Shark>) {
    crate::state::TEST_HOME
        .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())));
    *STORED.lock().unwrap() = None;
    *DELETE_ERROR.lock().unwrap() = None;
    LOG.lock().unwrap().clear();
    WRITE_FAILS.store(false, Ordering::Relaxed);
    HTTP_CALLS.store(0, Ordering::Relaxed);
    answer(Some("acme"));
    let state = State {
        linear,
        ..Default::default()
    };
    let (window, shark) = boot(cx, state, WindowOptions::default());
    shark.update(cx, |s, _| {
        s.linear.http = http();
        s.linear.keychain = fake_keychain();
    });
    cx.run_until_parked();
    (window, shark)
}

fn acme() -> Connection {
    Connection {
        org_id: "acme-uuid".into(),
        org: "acme".into(),
        user: "Ada".into(),
        method: Method::ApiKey,
    }
}

fn connection(cx: &TestAppContext, shark: &Entity<Shark>) -> Option<Connection> {
    cx.read(|cx| shark.read(cx).state.linear.clone())
}

fn notice(cx: &TestAppContext, shark: &Entity<Shark>) -> String {
    cx.read(|cx| shark.read(cx).linear.notice.clone().unwrap_or_default())
}

fn use_key(cx: &mut TestAppContext, shark: &Entity<Shark>, key: &'static str) {
    shark.update(cx, |s, cx| s.linear_use_key(key, cx));
    cx.run_until_parked();
}

async fn credential(cx: &mut TestAppContext, shark: &Entity<Shark>) -> Result<Credential, Error> {
    let task = shark.update(cx, |s, cx| s.linear_credential(cx));
    cx.run_until_parked();
    task.await
}

/// Opens Settings on the Linear page; `ctrl-tab` walks there as a keyboard user would.
fn open_page(cx: &mut TestAppContext, window: AnyWindowHandle, shark: &Entity<Shark>) {
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| crate::settings::open(&s.state, w, cx));
        w.render_frame(cx);
        for _ in 0..3 {
            w.press("ctrl-tab", cx);
        }
        w.render_frame(cx);
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn an_api_key_is_validated_then_saved(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, shark) = setup(cx, None);
    use_key(cx, &shark, KEY);
    assert_eq!(events(), ["validate", "write"]);
    assert_eq!(stored(), Some(Credential::key(KEY)));
    assert_eq!(connection(cx, &shark), Some(acme()));
    assert_eq!(State::load().linear, Some(acme()), "saved to disk");
    assert_eq!(credential(cx, &shark).await, Ok(Credential::key(KEY)));
    assert!(!events().contains(&"read"), "the session is already held");
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn an_invalid_key_keeps_the_previous_connection(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, shark) = setup(cx, None);
    use_key(cx, &shark, KEY);
    LOG.lock().unwrap().clear();
    answer(None);
    use_key(cx, &shark, OTHER);
    assert_eq!(events(), ["validate"], "nothing is written");
    assert_eq!(connection(cx, &shark), Some(acme()));
    assert_eq!(credential(cx, &shark).await, Ok(Credential::key(KEY)));
    assert!(notice(cx, &shark).contains("existing connection is unchanged"));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn a_keychain_write_failure_keeps_the_connection_and_says_so(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, shark) = setup(cx, None);
    use_key(cx, &shark, KEY);
    WRITE_FAILS.store(true, Ordering::Relaxed);
    answer(Some("globex"));
    use_key(cx, &shark, OTHER);
    assert_eq!(connection(cx, &shark), Some(acme()));
    assert_eq!(stored(), Some(Credential::key(KEY)));
    let said = notice(cx, &shark);
    assert!(
        said.contains("Keychain") && said.contains("unchanged"),
        "{said}"
    );
    assert_eq!(credential(cx, &shark).await, Ok(Credential::key(KEY)));
    let live = cx.read(|cx| shark.read(cx).linear.session.clone().unwrap());
    assert!(!live.closed.load(Ordering::Relaxed), "the session reopens");
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn switching_organizations_replaces_the_session(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, shark) = setup(cx, None);
    use_key(cx, &shark, KEY);
    answer(Some("globex"));
    use_key(cx, &shark, OTHER);
    let org = cx.read(|cx| shark.read(cx).linear_org_id().map(String::from));
    assert_eq!(org.as_deref(), Some("globex-uuid"));
    assert_eq!(credential(cx, &shark).await, Ok(Credential::key(OTHER)));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn disconnect_deletes_the_credential_and_the_metadata(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, shark) = setup(cx, None);
    use_key(cx, &shark, KEY);
    let sent = HTTP_CALLS.load(Ordering::Relaxed);
    shark.update(cx, |s, cx| s.linear_disconnect(cx));
    cx.run_until_parked();
    assert_eq!(stored(), None);
    assert_eq!(connection(cx, &shark), None);
    assert_eq!(State::load().linear, None);
    assert_eq!(
        HTTP_CALLS.load(Ordering::Relaxed),
        sent,
        "Linear isn't told"
    );
    assert!(credential(cx, &shark).await.is_err());
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn a_failed_delete_stays_connected_and_says_so(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, shark) = setup(cx, None);
    use_key(cx, &shark, KEY);
    *DELETE_ERROR.lock().unwrap() = Some("keychain Delete failed with OSStatus -25293");
    shark.update(cx, |s, cx| s.linear_disconnect(cx));
    cx.run_until_parked();
    assert_eq!(connection(cx, &shark), Some(acme()));
    assert!(notice(cx, &shark).contains("still connected"));
    assert_eq!(credential(cx, &shark).await, Ok(Credential::key(KEY)));
}

/// `SecItemDelete` fails on a missing item; that mustn't leave the user unable to disconnect.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn a_missing_keychain_entry_still_disconnects(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, shark) = setup(cx, Some(acme()));
    *DELETE_ERROR.lock().unwrap() = Some("keychain Delete failed with OSStatus -25300");
    shark.update(cx, |s, cx| s.linear_disconnect(cx));
    cx.run_until_parked();
    assert_eq!(connection(cx, &shark), None);
    assert_eq!(notice(cx, &shark), "");
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn cancelling_a_sign_in_ends_the_wait(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = setup(cx, None);
    shark.update(cx, |s, _| {
        s.linear.begin = || fake::begin("127.0.0.1:0");
        s.linear.ready = || Ok(());
    });
    open_page(cx, window, &shark);
    cx.update_window(window, |_, w, cx| {
        w.click("linear-sign-in", cx);
        w.render_frame(cx);
        assert!(matches!(shark.read(cx).linear.op, Op::Waiting(_)));
    })
    .unwrap();
    assert!(
        cx.opened_url()
            .unwrap()
            .starts_with("https://linear.app/oauth/authorize")
    );
    cx.update_window(window, |_, w, cx| w.click("linear-sign-in", cx))
        .unwrap();
    cx.run_until_parked();
    cx.read(|cx| assert!(shark.read(cx).linear.idle()));
    assert_eq!(notice(cx, &shark), "Sign-in cancelled.");
    assert_eq!(connection(cx, &shark), None);
    assert_eq!(events(), Vec::<&str>::new(), "no request, no Keychain");
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn an_occupied_port_explains_itself_and_opens_no_browser(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    static TAKEN: Mutex<String> = Mutex::new(String::new());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    *TAKEN.lock().unwrap() = listener.local_addr().unwrap().to_string();
    let (_, shark) = setup(cx, None);
    shark.update(cx, |s, _| {
        s.linear.begin = || fake::begin(&TAKEN.lock().unwrap())
    });
    shark.update(cx, |s, cx| s.linear_sign_in(cx));
    let said = notice(cx, &shark);
    assert!(said.contains("48173") && said.contains("API key"), "{said}");
    assert_eq!(cx.opened_url(), None);
    cx.read(|cx| assert!(shark.read(cx).linear.idle()));
}

/// With no Linear app registered, sign-in is off before anyone clicks it, and says why.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn sign_in_is_off_up_front_without_a_client_id(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = setup(cx, None);
    shark.update(cx, |s, _| s.linear.ready = || Err(Error::NotConfigured));
    open_page(cx, window, &shark);
    cx.update_window(window, |_, w, cx| {
        assert_eq!(w.find("linear-sign-in").disabled(), Some(true));
        assert!(w.try_find("linear-sign-in-unavailable").is_some());
        assert_ne!(w.find("linear-use-key").disabled(), Some(true));
        // Disabled, it's no tab stop: the first tab reaches the API key.
        for key in ["tab", "enter"] {
            w.press(key, cx);
        }
        assert!(
            w.try_find("linear-key-connect").is_some(),
            "the form opened"
        );
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn an_empty_client_id_says_so_and_the_api_key_still_works(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, shark) = setup(cx, None);
    shark.update(cx, |s, cx| s.linear_sign_in(cx));
    assert_eq!(notice(cx, &shark), Error::NotConfigured.to_string());
    assert_eq!(cx.opened_url(), None);
    use_key(cx, &shark, KEY);
    assert_eq!(connection(cx, &shark), Some(acme()));
    assert_eq!(notice(cx, &shark), "");
}

#[test]
fn settings_round_trip_and_old_settings_load() {
    let state = State {
        linear: Some(acme()),
        ..Default::default()
    };
    let json = serde_json::to_string(&state).unwrap();
    let back: State = serde_json::from_str(&json).unwrap();
    assert_eq!(back.linear, Some(acme()));

    let old: State = serde_json::from_str(r#"{"repos":[],"workspaces":[]}"#).unwrap();
    assert_eq!(old.linear, None);
    let partial = r#"{"repos":[],"workspaces":[],"linear":{"org_id":"o"}}"#;
    let partial: State = serde_json::from_str(partial).unwrap();
    assert_eq!(partial.linear.unwrap().org, "");
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn no_secret_reaches_the_settings_file(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, shark) = setup(cx, None);
    use_key(cx, &shark, KEY);
    let path = crate::state::support_dir().join("state.json");
    let json = std::fs::read_to_string(&path).unwrap();
    assert!(
        json.contains("acme-uuid"),
        "the file under test has the connection"
    );
    assert!(!json.contains(KEY));
}

/// A read can prompt on macOS, so showing a connection must not read.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn launching_and_opening_settings_never_touch_the_keychain(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = setup(cx, Some(acme()));
    open_page(cx, window, &shark);
    cx.update_window(window, |_, w, _| {
        assert!(
            w.try_find("linear-disconnect").is_some(),
            "shown as connected"
        );
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(events(), Vec::<&str>::new());
}

/// macOS answers a declined prompt with no item. The connection stays and can be retried.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn a_declined_keychain_read_keeps_the_connection_and_offers_a_retry(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = setup(cx, Some(acme()));
    assert!(credential(cx, &shark).await.is_err());
    assert_eq!(connection(cx, &shark), Some(acme()));
    assert_eq!(State::load().linear, Some(acme()));
    assert!(notice(cx, &shark).contains("Keychain access wasn't granted"));

    open_page(cx, window, &shark);
    *STORED.lock().unwrap() = Some(Credential::key(KEY));
    cx.update_window(window, |_, w, cx| w.click("linear-retry", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        notice(cx, &shark),
        "",
        "a successful retry clears the notice"
    );
    cx.read(|cx| assert!(!shark.read(cx).linear.retry));
    assert_eq!(credential(cx, &shark).await, Ok(Credential::key(KEY)));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn an_unauthorized_refresh_asks_to_reconnect_and_stops_sending(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = setup(cx, Some(acme()));
    *STORED.lock().unwrap() = Some(Credential::OAuth {
        access: "a".into(),
        refresh: "r".into(),
        expires_at: 0,
    });
    shark.update(cx, |s, _| {
        s.linear.http = &|_| {
            HTTP_CALLS.fetch_add(1, Ordering::Relaxed);
            fake::resp(400, r#"{"error":"invalid_grant"}"#)
        }
    });
    assert_eq!(credential(cx, &shark).await, Err(Error::Unauthorized));
    assert_eq!(credential(cx, &shark).await, Err(Error::Unauthorized));
    assert_eq!(
        HTTP_CALLS.load(Ordering::Relaxed),
        1,
        "no repeated failures"
    );
    assert_eq!(connection(cx, &shark), Some(acme()));
    assert!(notice(cx, &shark).contains("Reconnect"));

    open_page(cx, window, &shark);
    shark.update(cx, |s, _| s.linear.http = http());
    use_key(cx, &shark, KEY);
    assert_eq!(credential(cx, &shark).await, Ok(Credential::key(KEY)));
}

/// A refresh that lands after the entry changed must not write the old token over it.
#[test]
fn a_closed_session_cannot_save() {
    let saves = Arc::new(AtomicUsize::new(0));
    let counted = saves.clone();
    let expired = Credential::OAuth {
        access: "a".into(),
        refresh: "r".into(),
        expires_at: 0,
    };
    let live = Live {
        session: Session::new(expired),
        save: Box::new(move |_| {
            counted.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }),
        closed: AtomicBool::new(true),
    };
    let rotated = r#"{"access_token":"n","refresh_token":"m","expires_in":86399}"#;
    let refresh: &Http = &|_| fake::resp(200, rotated);
    assert!(matches!(live.fresh(refresh), Err(Error::Save(_))));
    assert_eq!(saves.load(Ordering::Relaxed), 0);
    live.drain();
    assert_eq!(saves.load(Ordering::Relaxed), 0, "draining saves nothing");
    live.closed.store(false, Ordering::Relaxed);
    assert!(
        live.fresh(refresh).is_ok(),
        "reopened, the kept token saves"
    );
    assert_eq!(saves.load(Ordering::Relaxed), 1);
}

/// What makes the swap safe: once `drain` returns, a refresh that was running has saved.
#[test]
fn drain_waits_for_a_running_refresh_and_its_save() {
    let saves = Arc::new(AtomicUsize::new(0));
    let counted = saves.clone();
    let live = Live {
        session: Session::new(Credential::OAuth {
            access: "a".into(),
            refresh: "r".into(),
            expires_at: 0,
        }),
        save: Box::new(move |_| {
            counted.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }),
        closed: AtomicBool::new(false),
    };
    let (started, running) = std::sync::mpsc::channel();
    let refresh: &Http = &|_| {
        started.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        fake::resp(
            200,
            r#"{"access_token":"n","refresh_token":"m","expires_in":86399}"#,
        )
    };
    std::thread::scope(|s| {
        let asked = s.spawn(|| live.fresh(refresh));
        running.recv().unwrap();
        live.drain();
        assert_eq!(saves.load(Ordering::Relaxed), 1);
        assert!(asked.join().unwrap().is_ok());
    });
}

/// The whole API-key route by keyboard, with the key gone from the input afterwards.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn the_api_key_route_works_from_the_keyboard(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = setup(cx, None);
    shark.update(cx, |s, _| s.linear.ready = || Ok(()));
    open_page(cx, window, &shark);
    cx.update_window(window, |_, w, cx| {
        for key in ["tab", "tab", "enter"] {
            w.press(key, cx);
        }
        assert!(w.has_active_dialog(cx), "enter doesn't close Settings");
        assert!(
            w.try_find("linear-key-connect").is_some(),
            "the form opened"
        );
        w.input(KEY, cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(stored(), Some(Credential::key(KEY)));
    assert_eq!(connection(cx, &shark), Some(acme()));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.has_active_dialog(cx), "the result stays on screen");
        assert!(w.try_find("linear-disconnect").is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn the_key_leaves_the_input_and_its_undo_stack_on_submit(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = setup(cx, None);
    cx.update_window(window, |_, w, cx| {
        let page = cx.new(|cx| LinearPage::new(shark.downgrade(), cx.focus_handle(), w, cx));
        let typed = page.update(cx, |p, cx| {
            p.key.update(cx, |k, cx| k.set_value(KEY, w, cx));
            let typed = p.key.clone();
            p.submit(w, cx);
            typed
        });
        let page = page.read(cx);
        assert_ne!(page.key.entity_id(), typed.entity_id(), "a new input");
        assert_eq!(page.key.read(cx).value(), "");
    })
    .unwrap();
    cx.run_until_parked();
}

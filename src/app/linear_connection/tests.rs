use super::*;
use crate::app::ui_tests::{UI_LOCK, boot};
use crate::linear::fake;
use crate::state::State;
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

fn attention(cx: &TestAppContext, shark: &Entity<Shark>) -> String {
    cx.read(|cx| {
        let reason = shark.read(cx).linear_needs_attention();
        reason.unwrap_or_default().to_string()
    })
}

fn the_window(cx: &mut TestAppContext) -> AnyWindowHandle {
    cx.update(|cx| cx.windows()[0])
}

fn use_key(cx: &mut TestAppContext, shark: &Entity<Shark>, key: &'static str) {
    let window = the_window(cx);
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.linear_use_key(key, w, cx).detach());
    })
    .unwrap();
    cx.run_until_parked();
}

fn sign_in(cx: &mut TestAppContext, shark: &Entity<Shark>) {
    let window = the_window(cx);
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.linear_sign_in(w, cx));
    })
    .unwrap();
}

/// Clicks `id` in the dialog on top: `ok` or `cancel`.
fn answer_dialog(cx: &mut TestAppContext, id: &'static str) {
    let window = the_window(cx);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click(id, cx);
        w.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
}

fn dialogs(cx: &mut TestAppContext) -> bool {
    let window = the_window(cx);
    cx.update_window(window, |_, w, cx| w.has_active_dialog(cx))
        .unwrap()
}

fn toasts(cx: &mut TestAppContext) -> usize {
    let window = the_window(cx);
    cx.update_window(window, |_, w, cx| w.notifications(cx).len())
        .unwrap()
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
    answer_dialog(cx, "ok");
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
    assert_eq!(
        stored(),
        Some(Credential::key(KEY)),
        "not before the warning"
    );
    answer_dialog(cx, "ok");
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
    assert_eq!(notice(cx, &shark), "", "cancelling isn't a failure");
    assert_eq!(connection(cx, &shark), None);
    assert_eq!(events(), Vec::<&str>::new(), "no request, no Keychain");
    assert_eq!(toasts(cx), 0);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("linear-notice").is_none());
        assert!(w.try_find("linear-disconnected").is_some());
    })
    .unwrap();
}

/// Signing in with a key half-entered leaves no form open beside the new connection.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn signing_in_closes_the_key_form(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = setup(cx, None);
    shark.update(cx, |s, _| {
        s.linear.begin = || fake::begin("127.0.0.1:0");
        s.linear.ready = || Ok(());
    });
    open_page(cx, window, &shark);
    cx.update_window(window, |_, w, cx| {
        w.click("linear-use-key", cx);
        w.render_frame(cx);
        w.click("linear-sign-in", cx);
        w.render_frame(cx);
        assert!(w.try_find("linear-key-connect").is_none());
        w.click("linear-sign-in", cx);
    })
    .unwrap();
    cx.run_until_parked();
}

/// Declining on Linear's consent page is backing out too.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn declining_in_the_browser_isnt_a_failure(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, shark) = setup(cx, None);
    let said = cx.read(|cx| {
        let denied = Error::Denied("access_denied".into());
        shark.read(cx).linear_failure(denied, Method::OAuth)
    });
    assert_eq!(said, None);
}

/// The whole browser route, finishing after Settings closed: the toast is all the user sees.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn a_sign_in_that_ends_after_settings_closed_still_says_so(cx: &mut TestAppContext) {
    use std::io::Write as _;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    static ADDR: Mutex<String> = Mutex::new(String::new());
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    *ADDR.lock().unwrap() = free.local_addr().unwrap().to_string();
    drop(free);
    let (window, shark) = setup(cx, None);
    shark.update(cx, |s, _| {
        s.linear.begin = || fake::begin(&ADDR.lock().unwrap());
        s.linear.ready = || Ok(());
        s.linear.http = &|req| match fake::body(req).contains("grant_type") {
            true => fake::resp(
                200,
                r#"{"access_token":"n","refresh_token":"m","expires_in":86399}"#,
            ),
            false => http()(req),
        };
    });
    open_page(cx, window, &shark);
    cx.update_window(window, |_, w, cx| {
        w.click("linear-sign-in", cx);
        w.close_all_dialogs(cx);
    })
    .unwrap();
    let url = url::Url::parse(&cx.opened_url().unwrap()).unwrap();
    let state = url.query_pairs().find(|(k, _)| k == "state").unwrap().1;
    let mut browser = std::net::TcpStream::connect(ADDR.lock().unwrap().as_str()).unwrap();
    let callback = format!("GET /linear/callback?code=c&state={state} HTTP/1.1\r\n\r\n");
    browser.write_all(callback.as_bytes()).unwrap();
    cx.run_until_parked();
    assert_eq!(connection(cx, &shark).unwrap().method, Method::OAuth);
    assert_eq!(toasts(cx), 1);
}

/// A sign-in that fails with Settings closed can't show on the page, so it's a toast too.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn a_failed_sign_in_reaches_the_user_with_settings_closed(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = setup(cx, None);
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            let failed = Task::ready(Err(Error::TimedOut));
            s.linear_adopt(failed, Method::OAuth, w, cx).detach();
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(notice(cx, &shark), "Sign-in timed out.");
    assert_eq!(toasts(cx), 1);
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
    sign_in(cx, &shark);
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
        assert!(w.try_find("linear-disconnected").is_none());
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
    sign_in(cx, &shark);
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
    assert!(attention(cx, &shark).contains("Keychain access wasn't granted"));

    open_page(cx, window, &shark);
    *STORED.lock().unwrap() = Some(Credential::key(KEY));
    cx.update_window(window, |_, w, cx| {
        assert!(w.try_find("linear-attention").is_some());
        assert!(w.try_find("linear-connected").is_none());
        w.click("linear-retry", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(attention(cx, &shark), "", "a successful retry clears it");
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
    assert!(attention(cx, &shark).contains("Reconnect"));

    open_page(cx, window, &shark);
    cx.update_window(window, |_, w, _| {
        assert!(
            w.try_find("linear-attention").is_some(),
            "not shown healthy"
        );
        assert!(w.try_find("linear-connected").is_none());
    })
    .unwrap();
    shark.update(cx, |s, _| s.linear.http = http());
    use_key(cx, &shark, KEY);
    assert_eq!(attention(cx, &shark), "");
    assert_eq!(credential(cx, &shark).await, Ok(Credential::key(KEY)));
}

/// A rejected key comes back through Reconnect's key form, and the page turns healthy.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn reconnect_after_a_revoked_key_takes_a_new_one(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = setup(cx, Some(acme()));
    shark.update(cx, |s, cx| s.linear_rejected(cx));
    open_page(cx, window, &shark);
    cx.update_window(window, |_, w, cx| {
        assert_eq!(w.find("linear-sign-in").label(), Some("Reconnect"));
        w.click("linear-sign-in", cx);
        w.render_frame(cx);
        assert!(w.try_find("linear-key-page").is_some(), "the form opened");
        w.input(KEY, cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("linear-connected").is_some());
        assert!(w.try_find("linear-key-page").is_none(), "the form closed");
    })
    .unwrap();
    assert_eq!(stored(), Some(Credential::key(KEY)));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn disconnect_asks_first(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = setup(cx, None);
    use_key(cx, &shark, KEY);
    open_page(cx, window, &shark);
    let ask = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| w.click("linear-disconnect", cx))
            .unwrap();
        cx.run_until_parked();
    };
    ask(cx);
    assert_eq!(connection(cx, &shark), Some(acme()), "only asked so far");
    answer_dialog(cx, "cancel");
    assert_eq!(connection(cx, &shark), Some(acme()));
    assert_eq!(stored(), Some(Credential::key(KEY)));
    ask(cx);
    answer_dialog(cx, "ok");
    assert_eq!(connection(cx, &shark), None);
    assert_eq!(stored(), None);
    assert!(dialogs(cx), "Settings stays open");
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn replacing_a_workspace_warns_and_can_be_declined(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, shark) = setup(cx, None);
    use_key(cx, &shark, KEY);
    answer(Some("globex"));
    LOG.lock().unwrap().clear();
    use_key(cx, &shark, OTHER);
    assert!(dialogs(cx), "asked");
    assert_eq!(events(), ["validate"], "nothing is written yet");
    answer_dialog(cx, "cancel");
    cx.read(|cx| assert!(shark.read(cx).linear.idle()));
    assert_eq!(notice(cx, &shark), "", "declining isn't a failure");
    assert_eq!(connection(cx, &shark), Some(acme()));
    assert_eq!(stored(), Some(Credential::key(KEY)));
    assert_eq!(credential(cx, &shark).await, Ok(Credential::key(KEY)));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn reconnecting_the_same_workspace_doesnt_warn(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, shark) = setup(cx, None);
    use_key(cx, &shark, KEY);
    use_key(cx, &shark, OTHER);
    assert!(!dialogs(cx));
    assert_eq!(stored(), Some(Credential::key(OTHER)));
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
async fn a_failed_key_stays_in_the_open_form_with_the_error_under_it(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = setup(cx, None);
    open_page(cx, window, &shark);
    answer(None);
    cx.update_window(window, |_, w, cx| {
        w.click("linear-use-key", cx);
        w.render_frame(cx);
        w.input(KEY, cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(notice(cx, &shark), BAD_KEY, "not \"Sign in again\"");
    assert_eq!(stored(), None);
    let json = std::fs::read_to_string(crate::state::support_dir().join("state.json")).unwrap();
    assert!(!json.contains(KEY));
    answer(Some("acme"));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let (form, error) = (w.find("linear-key-connect"), w.find("linear-notice"));
        assert!(error.bounds().top() >= form.bounds().bottom(), "under it");
        // No retyping: the value is still there to send again.
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(stored(), Some(Credential::key(KEY)));
    assert_eq!(notice(cx, &shark), "");
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn the_key_leaves_the_input_and_its_undo_stack_only_on_success(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = setup(cx, None);
    answer(None);
    let (page, typed) = cx
        .update_window(window, |_, w, cx| {
            let page = cx.new(|cx| LinearPage::new(shark.downgrade(), cx.focus_handle(), w, cx));
            let typed = page.update(cx, |p, cx| {
                p.key_open = true;
                p.key.update(cx, |k, cx| k.set_value(KEY, w, cx));
                p.submit(w, cx);
                p.key.clone()
            });
            (page, typed)
        })
        .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let page = page.read(cx);
        assert_eq!(page.key.entity_id(), typed.entity_id(), "the same input");
        assert_eq!(page.key.read(cx).value(), KEY);
        assert!(page.key_open);
    });

    answer(Some("acme"));
    cx.update_window(window, |_, w, cx| page.update(cx, |p, cx| p.submit(w, cx)))
        .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let page = page.read(cx);
        assert_ne!(page.key.entity_id(), typed.entity_id(), "a new input");
        assert_eq!(page.key.read(cx).value(), "");
        assert!(!page.key_open);
    });
    assert_eq!(toasts(cx), 1);
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn the_key_form_links_to_linears_api_key_page(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = setup(cx, None);
    open_page(cx, window, &shark);
    cx.update_window(window, |_, w, cx| {
        w.click("linear-use-key", cx);
        w.render_frame(cx);
        w.click("linear-key-page", cx);
    })
    .unwrap();
    assert_eq!(cx.opened_url().as_deref(), Some(KEY_PAGE));
}

/// Settings › Linear in each state, on the real renderer.
pub(in crate::app) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    crate::state::TEST_HOME
        .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())));
    let oauth = Connection {
        org: "Acme Robotics".into(),
        user: "Ada Lovelace".into(),
        method: Method::OAuth,
        ..acme()
    };
    let cases = [
        "disconnected",
        "unavailable",
        "waiting",
        "connected",
        "attention-revoked",
        "attention-keychain",
        "key-error",
        "key-error-connected",
        "disconnect-confirm",
    ];
    for (mode, label) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        State {
            appearance: crate::state::Appearance {
                mode,
                ..Default::default()
            },
            ..Default::default()
        }
        .save()
        .unwrap();
        for case in cases {
            let (window, shark) = cx
                .update(|cx| {
                    gpui_kit::open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(0.), px(0.)),
                                size(px(960.), px(720.)),
                            ))),
                            focus: false,
                            show: false,
                            ..Default::default()
                        },
                        cx,
                        |w, cx| cx.new(|cx| Shark::new(w, cx)),
                    )
                })
                .unwrap();
            cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    let connected = !matches!(case, "disconnected" | "unavailable" | "key-error");
                    s.state.linear = connected.then(|| oauth.clone());
                    if case == "waiting" {
                        s.state.linear = None;
                        s.linear.op = Op::Waiting(Default::default());
                    }
                    if case != "unavailable" {
                        s.linear.ready = || Ok(());
                    }
                    s.linear.reconnect = case == "attention-revoked";
                    s.linear.retry = (case == "attention-keychain").then(|| NOT_READ.to_string());
                    if case.starts_with("key-error") {
                        s.linear.notice = Some(match case {
                            "key-error" => BAD_KEY.to_string(),
                            _ => s.linear_kept(Error::Failed(BAD_KEY.into())),
                        });
                    }
                    crate::settings::open_linear(&s.state, w, cx);
                });
                w.render_frame(cx);
                if case == "key-error" {
                    w.click("linear-use-key", cx);
                    w.render_frame(cx);
                    w.input(KEY, cx);
                }
                if case == "key-error-connected" {
                    shark.update(cx, |s, _| {
                        s.state.linear.as_mut().unwrap().method = Method::ApiKey
                    });
                    w.render_frame(cx);
                    w.click("linear-sign-in", cx);
                    w.render_frame(cx);
                    w.input(KEY, cx);
                }
                if case == "disconnect-confirm" {
                    w.click("linear-disconnect", cx);
                }
                w.render_frame(cx);
            })
            .unwrap();
            let shot = cx.capture_screenshot(window).expect("Linear Metal render");
            if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                let dir = std::path::PathBuf::from(dir);
                std::fs::create_dir_all(&dir).unwrap();
                shot.save(dir.join(format!("linear-{case}-{label}.png")))
                    .unwrap();
            }
            cx.update_window(window, |_, w, _| w.remove_window())
                .unwrap();
        }
    }
    println!("linear connection pixels: {} rendered", cases.len() * 2);
}

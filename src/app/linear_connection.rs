//! The one Linear connection: its Settings page and the credential every Linear feature asks
//! for. Ordinary settings hold only `Connection`. The credential stays in the Keychain, and
//! nothing reads it until a feature asks, because a read can prompt on macOS.
use super::Shark;
use crate::linear::{self, Credential, Error, Flow, Http, Identity, Session};
use crate::settings::stacked;
use crate::ui::{card, confirm, notify_err};
#[cfg(all(test, feature = "test-support"))]
use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, Sizable as _, Theme, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What macOS's `SecItemDelete` returns for a missing item, as `gpui` words its status.
const NOT_FOUND: &str = "OSStatus -25300";
const NOT_READ: &str = "Keychain access wasn't granted, so Shark can't use Linear. Try again and allow access when macOS asks, or reconnect.";
const REJECTED: &str = "Linear rejected the credential. Reconnect to use it.";
const BAD_KEY: &str = "Linear didn't accept this key. Check it and try again.";
/// What leaving a workspace means, for its confirmations.
const LEAVING: &str = "Attached issues stay readable but can't refresh. Nothing changes in Linear.";
/// Settings > Account > Security & Access, per https://linear.app/docs/api-and-webhooks.
const KEY_PAGE: &str = "https://linear.app/settings/account/security";

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Method {
    #[default]
    OAuth,
    ApiKey,
}

/// What `state.json` keeps of the connection. Never the credential.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Connection {
    /// Organization UUID.
    pub org_id: String,
    pub org: String,
    pub user: String,
    pub method: Method,
}

/// The Keychain calls. The test platform's stubs can't fail, so tests swap these.
#[derive(Clone, Copy)]
struct Keychain {
    read: fn(&App) -> Task<anyhow::Result<Option<Credential>>>,
    write: fn(&App, &Credential) -> Task<anyhow::Result<()>>,
    delete: fn(&App) -> Task<anyhow::Result<()>>,
}

type Saver = Box<dyn Fn(&Credential) -> Result<(), String> + Send + Sync>;

/// The session in use. While `closed`, a refresh can't save: the Keychain entry is changing.
struct Live {
    session: Session,
    save: Saver,
    closed: AtomicBool,
}

impl Live {
    fn new(credential: Credential, cx: &App) -> Arc<Self> {
        Arc::new(Self {
            session: Session::new(credential),
            save: Box::new(linear::saver(cx)),
            closed: AtomicBool::new(false),
        })
    }

    /// Blocking.
    fn fresh(&self, http: &Http) -> Result<Credential, Error> {
        let save = |c: &Credential| match self.closed.load(Ordering::Relaxed) {
            true => Err("The Linear connection changed.".to_string()),
            false => (self.save)(c),
        };
        self.session.fresh(http, &save)
    }

    /// Waits out a running refresh and its save. Sends and saves nothing itself. Blocking.
    fn drain(&self) {
        let _ = (self.session).fresh(&|_| Err(Error::Cancelled), &|_| Err(String::new()));
    }
}

#[derive(Clone, Default)]
enum Op {
    #[default]
    Idle,
    /// On the browser; the flag cancels.
    Waiting(Arc<AtomicBool>),
    /// Validating an API key.
    Checking,
    /// The Keychain entry is changing.
    Saving,
}

/// Owned by `Shark`, whose `state.linear` says whether there's a connection.
pub(crate) struct Linear {
    /// None until a feature asks: launching and opening Settings touch nothing.
    session: Option<Arc<Live>>,
    /// Counts connection changes, so a Keychain read from before one is thrown away.
    epoch: u64,
    op: Op,
    /// The last failure, for the Settings page.
    notice: Option<String>,
    /// Linear rejected the credential: no more requests until a new one is connected.
    reconnect: bool,
    /// Why the Keychain didn't give up the credential, which may just be a declined prompt.
    retry: Option<String>,
    pub(crate) http: &'static Http<'static>,
    begin: fn() -> Result<Flow, Error>,
    /// Why `begin` can't work; Settings disables sign-in on it.
    ready: fn() -> Result<(), Error>,
    keychain: Keychain,
}

impl Default for Linear {
    fn default() -> Self {
        Self {
            session: None,
            epoch: 0,
            op: Op::Idle,
            notice: None,
            reconnect: false,
            retry: None,
            http: &linear::curl,
            begin: linear::begin,
            ready: linear::sign_in_ready,
            keychain: Keychain {
                read: |cx| {
                    let read = linear::read_credential(cx);
                    cx.spawn(async move |_| read.await)
                },
                write: linear::write_credential,
                delete: linear::delete_credential,
            },
        }
    }
}

impl Linear {
    fn idle(&self) -> bool {
        matches!(self.op, Op::Idle)
    }

    /// Takes the session out of reach, for `change_keychain`.
    fn close(&mut self) -> Option<Arc<Live>> {
        self.epoch += 1;
        self.op = Op::Saving;
        let old = self.session.take();
        if let Some(old) = &old {
            old.closed.store(true, Ordering::Relaxed);
        }
        old
    }
}

/// Changes the Keychain entry with the session closed. A running refresh finishes first and a
/// later one can't save, so an old token can't land on top of the change. On failure the session
/// reopens as it was, including any rotated token it still has to save.
async fn change_keychain(
    this: &WeakEntity<Shark>,
    cx: &mut AsyncApp,
    change: impl FnOnce(&Linear, &App) -> Task<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let old = this.update(cx, |s, cx| {
        cx.notify();
        s.linear.close()
    })?;
    if let Some(old) = old.clone() {
        (cx.background_executor())
            .spawn(async move { old.drain() })
            .await;
    }
    let changed = this.update(cx, |s, cx| change(&s.linear, cx))?.await;
    if changed.is_err() {
        this.update(cx, |s, _| {
            if let Some(old) = &old {
                old.closed.store(false, Ordering::Relaxed);
            }
            s.linear.session = old;
        })?;
    }
    changed
}

impl Shark {
    /// A credential for a Linear request, and the only way to get one. Every caller shares one
    /// `Session`, so an expired token gets one refresh, saved before anyone receives it. The
    /// first call reads the Keychain, which macOS may prompt for. `Error::Unauthorized` means the
    /// page now shows reconnect, and later calls fail at once without sending anything.
    // ponytail: two first calls at once read the Keychain twice and may prompt twice; they still
    // share one Session. Share the read if a feature ever asks concurrently at startup.
    pub(crate) fn linear_credential(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Task<Result<Credential, Error>> {
        let refused = if self.state.linear.is_none() {
            Some(Error::Failed("Connect Linear in Settings first.".into()))
        } else if self.linear.reconnect {
            Some(Error::Unauthorized)
        } else if matches!(self.linear.op, Op::Saving) {
            Some(Error::Failed(
                "The Linear connection is changing. Try again.".into(),
            ))
        } else {
            None
        };
        if let Some(e) = refused {
            return Task::ready(Err(e));
        }
        let (http, epoch) = (self.linear.http, self.linear.epoch);
        let keychain = self.linear.keychain;
        let held = (self.linear.session.clone()).ok_or_else(|| (keychain.read)(cx));
        cx.spawn(async move |this, cx| {
            let live = match held {
                Ok(live) => live,
                Err(read) => {
                    let stored = read.await;
                    (this.update(cx, |s, cx| s.linear_open(epoch, stored, cx)))
                        .map_err(|e| Error::Failed(e.to_string()))??
                }
            };
            let asked = live.clone();
            let fresh = (cx.background_executor())
                .spawn(async move { asked.fresh(http) })
                .await;
            if matches!(fresh, Err(Error::Unauthorized)) {
                let _ = this.update(cx, |s, cx| {
                    // A replaced session's failure says nothing about the new one.
                    if s.linear
                        .session
                        .as_ref()
                        .is_some_and(|l| Arc::ptr_eq(l, &live))
                    {
                        s.linear_rejected(cx);
                    }
                });
            }
            fresh
        })
    }

    /// The connected organization's UUID. A snapshot from any other reads as disconnected.
    pub(crate) fn linear_org_id(&self) -> Option<&str> {
        self.state.linear.as_ref().map(|c| c.org_id.as_str())
    }

    /// Linear refused the connection's credential: the page shows reconnect. For any request
    /// that gets `Error::Unauthorized`, since a revoked key only shows up when it's used.
    pub(crate) fn linear_rejected(&mut self, cx: &mut Context<Self>) {
        self.linear.reconnect = true;
        cx.notify();
    }

    /// Why the connection can't be used until the user acts, if so. Settings › Linear has the fix.
    pub(crate) fn linear_needs_attention(&self) -> Option<&str> {
        match self.linear.reconnect {
            true => Some(REJECTED),
            false => self.linear.retry.as_deref(),
        }
    }

    /// Runs a Linear read off the UI thread with the connection's credential. A rejected
    /// credential shows reconnect, unless the connection changed meanwhile.
    pub(crate) fn linear_request<T: Send + 'static>(
        &mut self,
        call: impl FnOnce(&Http, &Credential) -> Result<T, Error> + Send + 'static,
        cx: &mut Context<Self>,
    ) -> Task<Result<T, Error>> {
        let (credential, http, epoch) = (
            self.linear_credential(cx),
            self.linear.http,
            self.linear.epoch,
        );
        cx.spawn(async move |this, cx| {
            let credential = credential.await?;
            let result = (cx.background_executor())
                .spawn(async move { call(http, &credential) })
                .await;
            if matches!(result, Err(Error::Unauthorized)) {
                let _ = this.update(cx, |s, cx| {
                    if s.linear.epoch == epoch {
                        s.linear_rejected(cx);
                    }
                });
            }
            result
        })
    }

    /// Keeps a first read. The metadata stays when there's nothing to keep: `None` also means the
    /// user declined the prompt, so it doesn't prove the entry is gone.
    fn linear_open(
        &mut self,
        epoch: u64,
        stored: anyhow::Result<Option<Credential>>,
        cx: &mut Context<Self>,
    ) -> Result<Arc<Live>, Error> {
        if let Some(live) = &self.linear.session {
            return Ok(live.clone());
        }
        if epoch != self.linear.epoch || self.state.linear.is_none() {
            return Err(Error::Failed(
                "The Linear connection changed. Try again.".into(),
            ));
        }
        let failure = match stored {
            Ok(Some(credential)) => {
                let live = Live::new(credential, cx);
                self.linear.session = Some(live.clone());
                if self.linear.retry.take().is_some() {
                    cx.notify();
                }
                return Ok(live);
            }
            Ok(None) => NOT_READ.to_string(),
            Err(e) => format!("{NOT_READ} ({e})"),
        };
        self.linear.retry = Some(failure.clone());
        cx.notify();
        Err(Error::Failed(failure))
    }

    /// Binds the callback port, then opens the browser.
    pub(crate) fn linear_sign_in(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.linear.idle() {
            return;
        }
        self.linear.notice = None;
        match (self.linear.begin)() {
            Err(e) => self.linear.notice = Some(self.linear_kept(e)),
            Ok(flow) => {
                cx.open_url(&flow.url());
                let cancel = Arc::new(AtomicBool::new(false));
                self.linear.op = Op::Waiting(cancel.clone());
                let http = self.linear.http;
                let checked = cx.background_executor().spawn(async move {
                    let checked = flow.finish(http, &cancel).and_then(|credential| {
                        let identity = linear::validate(http, &credential)?;
                        Ok((credential, identity))
                    });
                    // Whatever went wrong after Cancel, the user has already moved on.
                    match cancel.load(Ordering::Relaxed) {
                        true => Err(Error::Cancelled),
                        false => checked,
                    }
                });
                self.linear_adopt(checked, Method::OAuth, window, cx)
                    .detach();
            }
        }
        cx.notify();
    }

    pub(crate) fn linear_cancel(&mut self) {
        if let Op::Waiting(cancel) = &self.linear.op {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Resolves to whether the key is now the connection.
    pub(crate) fn linear_use_key(
        &mut self,
        key: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        if !self.linear.idle() {
            return Task::ready(false);
        }
        self.linear.notice = None;
        self.linear.op = Op::Checking;
        let credential = Credential::key(key);
        let http = self.linear.http;
        let checked = cx.background_executor().spawn(async move {
            let identity = linear::validate(http, &credential)?;
            Ok((credential, identity))
        });
        let adopted = self.linear_adopt(checked, Method::ApiKey, window, cx);
        cx.notify();
        adopted
    }

    /// `e`, saying that a connection there was is still there.
    fn linear_kept(&self, e: Error) -> String {
        match &self.state.linear {
            Some(_) => format!("{e} The existing connection is unchanged."),
            None => e.to_string(),
        }
    }

    /// What a failed attempt says. Backing out is the user's choice, so it says nothing.
    fn linear_failure(&self, e: Error, method: Method) -> Option<String> {
        match (e, method) {
            (Error::Cancelled, _) => None,
            (Error::Denied(e), _) if e == "access_denied" => None,
            (Error::Unauthorized, Method::ApiKey) => {
                Some(self.linear_kept(Error::Failed(BAD_KEY.into())))
            }
            (e, _) => Some(self.linear_kept(e)),
        }
    }

    fn linear_done(&mut self, notice: Option<String>, cx: &mut Context<Self>) {
        self.linear.op = Op::Idle;
        self.linear.notice = notice;
        cx.notify();
    }

    /// Replaces the connection once `checked` has validated a credential, the user has agreed to
    /// leave a different workspace, and the Keychain holds it. Anything short of that leaves the
    /// connection as it was. Resolves to whether it was replaced. `window` outlives Settings, so
    /// the result reaches the user after they've closed it.
    fn linear_adopt(
        &mut self,
        checked: Task<Result<(Credential, Identity), Error>>,
        method: Method,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let window = window.window_handle();
        cx.spawn(async move |this, cx| {
            let (credential, identity) = match checked.await {
                Ok(checked) => checked,
                Err(e) => {
                    let notice = this.update(cx, |s, cx| {
                        let notice = s.linear_failure(e, method);
                        s.linear_done(notice.clone(), cx);
                        notice
                    });
                    if let (Ok(Some(notice)), Method::OAuth) = (notice, method) {
                        let _ = window.update(cx, |_, w, cx| notify_err(w, cx, notice));
                    }
                    return false;
                }
            };
            let leaving = this.update(cx, |s, _| {
                let old = s.state.linear.as_ref();
                old.filter(|c| c.org_id != identity.org_id)
                    .map(|c| c.org.clone())
            });
            if let Ok(Some(old)) = leaving {
                let title = format!("Replace {old} with {}?", identity.org);
                let (agree, agreed) = async_channel::bounded(1);
                let _ = window.update(cx, |_, w, cx| {
                    this.update(cx, |_, cx| {
                        let ok = move |_: &mut Shark, _: &mut Window, _: &mut Context<Shark>| {
                            let _ = agree.try_send(());
                        };
                        confirm(title, LEAVING, "Replace", ok, w, cx)
                    })
                });
                // Dismissing the dialog drops `agree`.
                if agreed.recv().await.is_err() {
                    let _ = this.update(cx, |s, cx| s.linear_done(None, cx));
                    return false;
                }
            }
            let saved =
                change_keychain(&this, cx, |l, cx| (l.keychain.write)(cx, &credential)).await;
            let connected = this.update(cx, |s, cx| {
                if let Err(e) = saved {
                    let e = Error::Failed(format!(
                        "Couldn't save the Linear credential to the Keychain: {e}."
                    ));
                    let notice = s.linear_kept(e);
                    s.linear_done(Some(notice), cx);
                    return None;
                }
                let said = format!("Connected to {} as {}", identity.org, identity.user);
                s.linear.session = Some(Live::new(credential, cx));
                s.linear.reconnect = false;
                s.linear.retry = None;
                s.state.linear = Some(Connection {
                    org_id: identity.org_id,
                    org: identity.org,
                    user: identity.user,
                    method,
                });
                let notice = (s.state.save().err())
                    .map(|e| format!("Connected, but settings couldn't be saved: {e}"));
                s.linear_done(notice, cx);
                Some(said)
            });
            let Ok(Some(said)) = connected else {
                return false;
            };
            let _ = window.update(cx, |_, w, cx| {
                w.push_notification(Notification::success(said), cx)
            });
            true
        })
    }

    /// Deletes the credential and the metadata; Linear itself isn't told. If the delete fails the
    /// connection stays, so no secret outlives its metadata.
    pub(crate) fn linear_disconnect(&mut self, cx: &mut Context<Self>) {
        if !self.linear.idle() {
            return;
        }
        self.linear.notice = None;
        self.linear.op = Op::Saving;
        cx.spawn(async move |this, cx| {
            let deleted = change_keychain(&this, cx, |l, cx| {
                let delete = (l.keychain.delete)(cx);
                // macOS reports a missing item as a failure, but there's no secret left to orphan.
                // ponytail: matches gpui's wording; its error type isn't reachable from here.
                cx.spawn(async move |_| match delete.await {
                    Err(e) if e.to_string().contains(NOT_FOUND) => Ok(()),
                    done => done,
                })
            })
            .await;
            let _ = this.update(cx, |s, cx| {
                if let Err(e) = deleted {
                    let notice = format!(
                        "Couldn't remove the Linear credential from the Keychain: {e}. Shark is still connected."
                    );
                    return s.linear_done(Some(notice), cx);
                }
                s.linear.reconnect = false;
                s.linear.retry = None;
                s.state.linear = None;
                let notice = (s.state.save().err())
                    .map(|e| format!("Disconnected, but settings couldn't be saved: {e}"));
                s.linear_done(notice, cx);
            });
        })
        .detach();
    }
}

#[cfg(all(test, feature = "test-support"))]
impl Shark {
    /// Connected as `connection` over `http`, with the Keychain out of the picture.
    pub(crate) fn linear_connected(
        &mut self,
        key: &str,
        connection: Connection,
        http: &'static Http<'static>,
        cx: &mut Context<Self>,
    ) {
        self.state.linear = Some(connection);
        self.linear.http = http;
        self.linear.session = Some(Live::new(Credential::key(key), cx));
    }
}

/// Settings › Linear.
pub(crate) struct LinearPage {
    shark: WeakEntity<Shark>,
    /// Settings' own focus, for when the focused control goes away.
    focus: FocusHandle,
    key: Entity<InputState>,
    key_open: bool,
    _key_sub: Subscription,
    _shark_sub: Option<Subscription>,
}

impl LinearPage {
    pub(crate) fn new(
        shark: WeakEntity<Shark>,
        focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (key, key_sub) = Self::key_input(window, cx);
        let shark_sub = (shark.upgrade()).map(|s| cx.observe(&s, |_, _, cx| cx.notify()));
        Self {
            shark,
            focus,
            key,
            key_open: false,
            _key_sub: key_sub,
            _shark_sub: shark_sub,
        }
    }

    fn key_input(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<InputState>, Subscription) {
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("lin_api_…")
        });
        let sub = cx.subscribe_in(&key, window, |this, _, ev, w, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.submit(w, cx);
            }
        });
        (key, sub)
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.key.read(cx).value();
        let idle = (self.shark.upgrade()).is_some_and(|s| s.read(cx).linear.idle());
        if key.trim().is_empty() || !idle {
            return;
        }
        let adopted = self
            .shark
            .update(cx, |s, cx| s.linear_use_key(&key, window, cx));
        let Ok(adopted) = adopted else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            if adopted.await {
                this.update_in(cx, |this, w, cx| this.clear_key(w, cx)).ok();
            }
        })
        .detach();
    }

    /// Only once the key is the connection: a failed one stays here to be corrected.
    fn clear_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A new input, not an emptied one: `set_value` leaves the key on the undo stack.
        (self.key, self._key_sub) = Self::key_input(window, cx);
        self.key_open = false;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn open_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.key_open = !self.key_open;
        match self.key_open {
            true => self.key.update(cx, |k, cx| k.focus(window, cx)),
            false => self.focus.focus(window, cx),
        }
        cx.notify();
    }

    /// Repeats the connection's method, or takes a key when sign-in can't start.
    // ponytail: switching method goes through Disconnect. Offer both here if people ask.
    fn reconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shark) = self.shark.upgrade() else {
            return;
        };
        let s = shark.read(cx);
        let oauth = (s.state.linear.as_ref()).is_some_and(|c| c.method == Method::OAuth);
        match oauth && (s.linear.ready)().is_ok() {
            true => self.sign_in(window, cx),
            false => self.open_key(window, cx),
        }
    }

    fn sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.key_open = false;
        (self.shark)
            .update(cx, |s, cx| s.linear_sign_in(window, cx))
            .ok();
    }

    fn disconnect(&mut self, org: &str, window: &mut Window, cx: &mut Context<Self>) {
        let ok = |this: &mut Self, w: &mut Window, cx: &mut Context<Self>| {
            // The focused button goes away with the connection.
            this.focus.focus(w, cx);
            this.shark.update(cx, Shark::linear_disconnect).ok();
        };
        let title = format!("Disconnect {org}?");
        confirm(title, LEAVING, "Disconnect", ok, window, cx);
    }
}

/// `row` for the connection's state: an icon, text known only at runtime, and its actions.
fn status_row(
    id: &'static str,
    theme: &Theme,
    icon: impl IntoElement,
    (title, desc): (SharedString, SharedString),
    title_color: Hsla,
    actions: impl IntoElement,
) -> impl IntoElement {
    h_flex()
        .id(id)
        .min_h(px(52.))
        .px_3()
        .py_2()
        .gap_3()
        .child(div().flex_none().child(icon))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .text_color(title_color)
                        .truncate()
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(desc),
                ),
        )
        .child(h_flex().flex_none().gap_2().child(actions))
        .test_support()
}

impl Render for LinearPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(shark) = self.shark.upgrade() else {
            return div();
        };
        let (connection, op, notice, attention, retry, ready) = {
            let s = shark.read(cx);
            let l = &s.linear;
            (
                s.state.linear.clone(),
                l.op.clone(),
                l.notice.clone(),
                s.linear_needs_attention().map(String::from),
                l.retry.is_some() && !l.reconnect,
                (l.ready)().is_ok(),
            )
        };
        let idle = matches!(op, Op::Idle);
        let waiting = matches!(op, Op::Waiting(_));
        let checking = matches!(op, Op::Checking);
        let on = |f: fn(&mut Shark, &mut Window, &mut Context<Shark>)| {
            let shark = shark.downgrade();
            move |_: &ClickEvent, w: &mut Window, cx: &mut App| {
                shark.update(cx, |s, cx| f(s, w, cx)).ok();
            }
        };
        // At most one primary button: the key form's Connect takes over while it's open.
        let lead = |b: Button, on: bool| match on && !self.key_open {
            true => b.primary(),
            false => b.outline(),
        };
        let theme = cx.theme();
        let glyph = |icon: IconName, color: Hsla| {
            Icon::new(icon)
                .with_size(px(14.))
                .text_color(color)
                .into_any_element()
        };
        let (id, icon, title_color, words): (_, _, _, (SharedString, SharedString)) =
            match (&connection, &attention) {
                _ if waiting => (
                    "linear-waiting",
                    Spinner::new()
                        .color(theme.muted_foreground)
                        .with_size(px(14.))
                        .into_any_element(),
                    theme.foreground,
                    (
                        "Waiting for your browser…".into(),
                        "Finish signing in to Linear there".into(),
                    ),
                ),
                (None, _) => (
                    match ready {
                        true => "linear-disconnected",
                        false => "linear-sign-in-unavailable",
                    },
                    glyph(IconName::CircleDashed, theme.muted_foreground),
                    theme.foreground,
                    (
                        "Not connected".into(),
                        match ready {
                            true => "Read-only access to issues you can see".into(),
                            false => "Read-only access to issues you can see. Sign-in with Linear isn't available yet, so use an API key.".into(),
                        },
                    ),
                ),
                (Some(c), Some(why)) => (
                    "linear-attention",
                    glyph(IconName::TriangleAlert, theme.warning_foreground),
                    theme.warning_foreground,
                    (format!("{} needs attention", c.org).into(), why.clone().into()),
                ),
                (Some(c), None) => (
                    "linear-connected",
                    glyph(IconName::CircleCheck, theme.success_foreground),
                    theme.foreground,
                    (
                        format!("Connected as {}", c.user).into(),
                        format!(
                            "{} workspace · {}",
                            c.org,
                            match c.method {
                                Method::OAuth => "Signed in with Linear",
                                Method::ApiKey => "API key",
                            }
                        )
                        .into(),
                    ),
                ),
            };
        // One id for all, so keyboard focus stays on the control as it changes with the state.
        let main = Button::new("linear-sign-in").small();
        let main = match &connection {
            _ if waiting => main
                .outline()
                .label("Cancel")
                .on_click(on(|s, _, _| s.linear_cancel())),
            None => lead(main, ready)
                .label("Sign in with Linear")
                .disabled(!idle || !ready)
                .on_click(cx.listener(|this, _, w, cx| this.sign_in(w, cx))),
            Some(_) => lead(main, attention.is_some())
                .label("Reconnect")
                .disabled(!idle)
                .on_click(cx.listener(|this, _, w, cx| this.reconnect(w, cx))),
        };
        let actions = h_flex()
            .gap_2()
            .when(retry && idle, |r| {
                r.child(
                    Button::new("linear-retry")
                        .small()
                        .outline()
                        .label("Try again")
                        .on_click(on(|s, _, cx| s.linear_credential(cx).detach())),
                )
            })
            .child(main)
            .when(!waiting && connection.is_none(), |r| {
                r.child(
                    lead(Button::new("linear-use-key").small(), !ready)
                        .label("Use API key")
                        .disabled(!idle)
                        .on_click(cx.listener(|this, _, w, cx| this.open_key(w, cx))),
                )
            })
            .when_some(
                connection
                    .as_ref()
                    .filter(|_| !waiting)
                    .map(|c| c.org.clone()),
                |r, org| {
                    r.child(
                        Button::new("linear-disconnect")
                            .small()
                            .outline()
                            .label("Disconnect")
                            .disabled(!idle)
                            .on_click(
                                cx.listener(move |this, _, w, cx| this.disconnect(&org, w, cx)),
                            ),
                    )
                },
            );
        let failure = notice.map(|notice| {
            div()
                .id("linear-notice")
                .text_xs()
                .text_color(theme.danger_foreground)
                .child(notice)
                .test_support()
        });
        let (under_key, under_card) = match self.key_open {
            true => (failure, None),
            false => (None, failure),
        };
        v_flex()
            .gap_3()
            .key_context("LinearPage")
            .child(
                card(theme)
                    .child(status_row(id, theme, icon, words, title_color, actions))
                    .when(self.key_open, |c| {
                        c.child(stacked(
                            theme,
                            "API key",
                            "Create a personal API key with Read access, then paste it here",
                            v_flex()
                                .gap_2()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(div().flex_1().child(Input::new(&self.key).small()))
                                        .child(
                                            Button::new("linear-key-connect")
                                                .small()
                                                .primary()
                                                .label("Connect")
                                                .loading(checking)
                                                .disabled(!idle)
                                                .on_click(
                                                    cx.listener(|this, _, w, cx| {
                                                        this.submit(w, cx)
                                                    }),
                                                ),
                                        ),
                                )
                                .children(under_key)
                                .child(
                                    h_flex().child(
                                        Button::new("linear-key-page")
                                            .ghost()
                                            .xsmall()
                                            .role(Role::Link)
                                            .icon(IconName::ExternalLink)
                                            .label("Open Linear's API key page")
                                            .on_click(|_, _, cx| cx.open_url(KEY_PAGE)),
                                    ),
                                ),
                        ))
                    }),
            )
            .children(under_card)
    }
}

#[cfg(all(test, feature = "test-support"))]
pub(in crate::app) mod tests;

//! Which workspaces need the user: a turn finished while it wasn't on screen ("unread"), or its
//! next git step is the user's to take ("needs you"). Drives the sidebar row icons, the Dock
//! badge, OS notifications and the jump to the next such workspace.
use crate::app::Shark;
use crate::sync::background;
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// How often each workspace that isn't shown is checked.
const BG_EVERY: Duration = Duration::from_secs(300);
/// One workspace per tick, so the checks are spread out instead of all at once.
const TICK: Duration = Duration::from_secs(20);
/// After gh failed for two workspaces in a row, nothing is checked for this long.
const BG_BACKOFF: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, PartialEq)]
pub enum Attn {
    None,
    /// A turn finished while the workspace wasn't shown.
    Unread,
    /// The label of the step that waits for the user.
    Needs(String),
}

#[derive(Default)]
pub struct Attention {
    unread: HashSet<String>,
    /// Background checks: when, and the step label if it needs the user.
    checked: HashMap<String, (Instant, Option<String>)>,
    paused_until: Option<Instant>,
    /// The workspace whose last check failed: one that isn't on GitHub shouldn't pause the rest.
    failed: Option<String>,
    /// The last "needs you" label seen per workspace, so each change notifies once.
    announced: HashMap<String, Option<String>>,
    pub window_active: bool,
    _poll: Option<Task<()>>,
}

impl Attention {
    pub fn new(cx: &mut Context<Shark>) -> Self {
        Attention {
            window_active: true,
            _poll: Some(poll(cx)),
            ..Default::default()
        }
    }

    /// Opening a workspace reads it; its own GitSync polls from now on.
    pub fn opened(&mut self, name: &str) {
        self.unread.remove(name);
        self.checked.remove(name);
    }

    /// The workspace to check next: not shown, oldest check first, never-checked before all.
    fn due(&self, names: &[String], active: Option<&str>, now: Instant) -> Option<String> {
        if self.paused_until.is_some_and(|t| now < t) {
            return None;
        }
        names
            .iter()
            .filter(|n| Some(n.as_str()) != active)
            .map(|n| (self.checked.get(n).map(|c| c.0), n))
            .filter(|(at, _)| at.is_none_or(|t| now.duration_since(t) >= BG_EVERY))
            .min_by_key(|(at, _)| *at)
            .map(|(_, n)| n.clone())
    }
}

fn poll(cx: &mut Context<Shark>) -> Task<()> {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(TICK).await;
            let Ok(next) = this.update(cx, |s, cx| {
                s.refresh_attention(cx);
                let names: Vec<String> = (s.state.workspaces.iter())
                    .map(|w| w.name.clone())
                    .filter(|n| !s.busy(n, cx))
                    .collect();
                let name = s.attn.due(&names, s.active.as_deref(), Instant::now())?;
                s.state.workspaces.iter().find(|w| w.name == name).cloned()
            }) else {
                break;
            };
            let Some(ws) = next else { continue };
            let name = ws.name.clone();
            let result = cx
                .background_executor()
                .spawn(async move { background::check(&ws) })
                .await;
            let ok = this.update(cx, |s, cx| {
                let now = Instant::now();
                let needs = match result {
                    Ok(needs) => {
                        s.attn.failed = None;
                        needs
                    }
                    Err(_) => {
                        if s.attn.failed.as_ref().is_some_and(|f| *f != name) {
                            s.attn.paused_until = Some(now + BG_BACKOFF);
                        }
                        s.attn.failed = Some(name.clone());
                        s.attn.checked.get(&name).and_then(|c| c.1.clone())
                    }
                };
                if s.active.as_ref() != Some(&name) {
                    s.attn.checked.insert(name, (now, needs));
                }
                s.refresh_attention(cx);
            });
            if ok.is_err() {
                break;
            }
        }
    })
}

impl Shark {
    pub(crate) fn attention_of(&self, name: &str, cx: &App) -> Attn {
        match (self.needs_you(name, cx), self.attn.unread.contains(name)) {
            (Some(l), _) => Attn::Needs(l),
            (None, true) => Attn::Unread,
            (None, false) => Attn::None,
        }
    }

    /// The agent is on it (mid-rebase looks like conflicts), or the workspace is going away.
    fn busy(&self, name: &str, cx: &App) -> bool {
        self.archiving.contains(name) || self.chats.get(name).is_some_and(|c| c.read(cx).working())
    }

    fn needs_you(&self, name: &str, cx: &App) -> Option<String> {
        if self.busy(name, cx) {
            return None;
        }
        let own = || self.syncs.get(name)?.read(cx).needs_you();
        match (
            self.active.as_deref() == Some(name),
            self.attn.checked.get(name),
        ) {
            (false, Some((_, needs))) => needs.clone(),
            _ => own(),
        }
    }

    pub(crate) fn turn_done(&mut self, name: &str, cx: &mut Context<Self>) {
        let shown = self.active.as_deref() == Some(name);
        if !shown {
            self.attn.unread.insert(name.to_string());
            // The agent may have pushed or opened a PR; check again on the next tick.
            if let Some(c) = self.attn.checked.get_mut(name) {
                c.0 = Instant::now().checked_sub(BG_EVERY).unwrap_or(c.0);
            }
        }
        if !shown || !self.attn.window_active {
            notify(name, "The agent finished its turn");
        }
        self.refresh_attention(cx);
    }

    /// Recounts the Dock badge and notifies when a workspace off screen starts needing you.
    pub(crate) fn refresh_attention(&mut self, cx: &mut Context<Self>) {
        let names: Vec<String> = self
            .state
            .workspaces
            .iter()
            .map(|w| w.name.clone())
            .collect();
        self.attn.checked.retain(|n, _| names.contains(n));
        self.attn.unread.retain(|n| names.contains(n));
        let mut count = 0;
        for name in &names {
            let needs = self.needs_you(name, cx);
            count += usize::from(needs.is_some() || self.attn.unread.contains(name));
            // Nothing known yet (never opened, not checked): no baseline to compare against.
            if !self.attn.checked.contains_key(name) && !self.syncs.contains_key(name) {
                continue;
            }
            let on_screen = self.active.as_ref() == Some(name) && self.attn.window_active;
            let before = self.attn.announced.insert(name.clone(), needs.clone());
            if let (Some(prev), Some(l)) = (&before, &needs)
                && prev != &needs
                && !on_screen
            {
                notify(name, l);
            }
        }
        set_badge(count);
        cx.notify();
    }

    /// Opens the next workspace after the open one (in sidebar order) that needs you.
    pub(crate) fn next_attention(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.state.workspaces.len();
        let from = self
            .active
            .as_ref()
            .and_then(|a| self.state.workspaces.iter().position(|w| &w.name == a))
            .map_or(0, |i| i + 1);
        let hit = (0..n).map(|i| (from + i) % n).find(|&i| {
            let name = &self.state.workspaces[i].name;
            self.active.as_ref() != Some(name) && self.attention_of(name, cx) != Attn::None
        });
        match hit {
            Some(ix) => self.open_workspace(ix, window, cx),
            None => window.push_notification("No other workspace needs you", cx),
        }
    }
}

/// Dock badge with the count; empty at zero.
#[cfg(not(test))]
fn set_badge(n: usize) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let label = (n > 0).then(|| NSString::from_str(&n.to_string()));
    NSApplication::sharedApplication(mtm)
        .dockTile()
        .setBadgeLabel(label.as_deref());
}

#[cfg(test)]
fn set_badge(_: usize) {}

/// An OS notification. UNUserNotificationCenter throws in a binary without a bundle (`cargo run`),
/// so this goes through osascript: it shows as Script Editor's, and a failure is only lost.
/// Text travels as arguments, never inside the script.
#[cfg(not(test))]
fn notify(title: &str, body: &str) {
    let (title, body) = (format!("shark · {title}"), body.to_string());
    std::thread::spawn(move || {
        let script = [
            "-e",
            "on run argv",
            "-e",
            "display notification (item 2 of argv) with title (item 1 of argv)",
            "-e",
            "end run",
        ];
        let args: Vec<&str> = script
            .into_iter()
            .chain([title.as_str(), body.as_str()])
            .collect();
        let _ = crate::git::cmd_bounded(std::path::Path::new("/"), "osascript", &args, 10);
    });
}

#[cfg(test)]
fn notify(_: &str, _: &str) {}

#[cfg(test)]
mod tests {
    use super::{Attention, BG_EVERY, Instant, TICK};

    #[test]
    fn checks_are_staggered_and_skip_the_shown_one() {
        let names: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
        let now = Instant::now() + BG_EVERY * 2;
        let mut at = Attention::default();
        assert_eq!(at.due(&names, Some("a"), now).as_deref(), Some("b"));
        at.checked.insert("b".into(), (now, None));
        assert_eq!(at.due(&names, Some("a"), now).as_deref(), Some("c"));
        at.checked.insert("c".into(), (now - BG_EVERY, None));
        assert_eq!(
            at.due(&names, Some("a"), now).as_deref(),
            Some("c"),
            "old enough"
        );
        at.checked.insert("c".into(), (now, None));
        assert_eq!(at.due(&names, Some("a"), now), None, "all fresh");
        at.paused_until = Some(now + TICK);
        assert_eq!(at.due(&names, None, now), None, "backing off");
    }
}

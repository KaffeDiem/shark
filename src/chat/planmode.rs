//! Plan mode: the replies to the agent's plan tools and the plan pane.
use super::*;

/// Replies to tools that need a person, which claude asks us about (`Ev::Permission`).
pub(super) const PLAN_SHOWN: &str = "The user now sees this plan in the app's plan panel, where they \
    approve it or reply with changes. End your turn with one short sentence, without repeating \
    the plan. If they ask for changes, update the plan file and call ExitPlanMode again.";
pub(super) const ENTER_PLAN: &str = "Only the user turns plan mode on in this app (shift-tab). Carry on, or \
    suggest it in your reply.";
pub(super) const NEEDS_APPROVAL: &str = "This needs the user's approval, which this app can't ask for. Do \
    without it, or tell the user what to run.";
pub(super) const APPROVED: &str = "I approve the plan. Plan mode is off: implement it now.";
pub(super) const BUILD_FRESH: &str = "Implement this plan. It was made and approved in another chat, so you \
    have none of that chat's context: read the code it touches before you change it.";

impl Chat {
    /// Plan mode on or off. A running process switches at once, mid-turn too; a new one
    /// switches as it starts.
    pub fn set_plan_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.meta.plan == on {
            return;
        }
        self.meta.plan = on;
        if self.busy && self.codex() {
            self.push(
                Block::Info("codex switches plan mode from the next message".into()),
                cx,
            );
        }
        if let Some(a) = &mut self.agent
            && let Err(e) = a.set_plan_mode(on)
        {
            self.push(Block::Info(format!("switching plan mode failed: {e}")), cx);
        }
        // The pane says what will appear there; with nothing in it, it goes with the mode.
        if on || self.plan.read(cx).versions.is_empty() {
            self.plan_open = on;
        }
        cx.emit(ChatEvent::Meta);
        cx.notify();
    }

    /// Why the plan can't be approved or built now: none yet, already approved, or an older
    /// version is on screen (approving always takes the latest).
    pub fn plan_blocker(&self, cx: &App) -> Option<&'static str> {
        let p = self.plan.read(cx);
        let latest = p.versions.len().checked_sub(1);
        if latest.is_none() {
            Some("no plan to approve yet; the agent submits one in plan mode (shift-tab)")
        } else if p.approved == latest {
            Some("this plan is already approved; ask for a revision to approve a new version")
        } else if latest != Some(p.shown) {
            Some("an older version is shown; press l for the latest, then approve")
        } else {
            None
        }
    }

    /// Says why the plan can't be approved or built, if it can't. Returns whether it refused.
    pub fn refuse_plan(&mut self, cx: &mut Context<Self>) -> bool {
        let why = self.plan_blocker(cx);
        if let Some(why) = why {
            self.push(Block::Info(why.into()), cx);
        }
        why.is_some()
    }

    /// Ends plan mode and tells the agent to build the latest plan. A draft rides along.
    pub fn approve_plan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let why = if let Some(why) = self.plan_blocker(cx) {
            why
        } else if self.busy {
            "the agent is still working; approve when the turn ends (ctrl-c stops it)"
        } else {
            let was = self.meta.plan;
            self.set_plan_mode(false, cx);
            if self.ask(APPROVED, window, cx) {
                self.plan_approved(cx);
                self.focus_composer(window, cx);
            } else {
                self.set_plan_mode(was, cx);
            }
            return;
        };
        self.push(Block::Info(why.into()), cx);
    }

    /// Marks the latest plan approved and closes the pane: there is nothing left to review.
    pub fn plan_approved(&mut self, cx: &mut Context<Self>) {
        self.plan.update(cx, |p, cx| p.approve(cx));
        self.plan_open = false;
        cx.notify();
    }

    /// The first message of a chat that builds another chat's plan: the plan and nothing else,
    /// titled after the plan.
    pub fn build_plan(&mut self, plan: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.meta.title = title_of(&plan::title(plan));
        self.ask(&format!("{BUILD_FRESH}\n\n{plan}"), window, cx)
    }

    /// `TogglePlan`: shows the plan pane and focuses it, or hides it.
    pub fn toggle_plan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.plan_open = !self.plan_open;
        match self.plan_open {
            true => self.plan.read(cx).focus.clone().focus(window, cx),
            false => self.focus_composer(window, cx),
        }
        cx.notify();
    }

    pub fn plan_focused(&self, window: &Window, cx: &App) -> bool {
        self.plan_open && self.plan.read(cx).focus.is_focused(window)
    }

    /// A submitted plan as the pane's newest version, shown.
    pub(super) fn show_plan(&mut self, id: String, text: String, cx: &mut Context<Self>) {
        if !text.trim().is_empty() {
            self.plan.update(cx, |p, cx| p.add(id, text, cx));
            self.plan_open = true;
        }
    }
}

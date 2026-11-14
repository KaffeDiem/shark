//! One look per status, wherever a team, goal or row shows: the sidebar, a team's page, the
//! Goal tab and Home all draw these.
use super::team::{RowStatus, TeamStatus};
use super::*;
use crate::state::{GoalState, Row, RowState, Why};

/// How a status is drawn: its icon (spinning while an agent works), color and words.
pub(super) struct Look {
    pub(super) icon: IconName,
    pub(super) color: Hsla,
    pub(super) spin: bool,
    pub(super) label: SharedString,
}

impl Look {
    fn new(icon: IconName, color: Hsla, label: impl Into<SharedString>) -> Self {
        Look {
            icon,
            color,
            spin: false,
            label: label.into(),
        }
    }

    fn running(theme: &Theme, label: impl Into<SharedString>) -> Self {
        Look {
            spin: true,
            ..Look::new(IconName::LoaderCircle, theme.primary, label)
        }
    }

    fn needs(theme: &Theme, label: impl Into<SharedString>) -> Self {
        Look::new(IconName::CircleAlert, theme.warning_foreground, label)
    }

    /// The icon at `size`, spinning in primary while an agent works.
    pub(super) fn icon(&self, size: f32) -> AnyElement {
        match self.spin {
            true => Spinner::new()
                .icon(self.icon.clone())
                .color(self.color)
                .with_size(px(size))
                .into_any_element(),
            false => Icon::new(self.icon.clone())
                .with_size(px(size))
                .text_color(self.color)
                .into_any_element(),
        }
    }
}

impl Shark {
    /// A row of a goal's plan, as its task's status.
    pub(super) fn row_look(&self, row: &Row, cx: &App) -> Look {
        let theme = cx.theme();
        // A handoff held by the next agent's pause or allowance: at work in name only.
        if row.state == RowState::Started && row.handoff.is_some() && row.chat.is_some() {
            return Look::new(IconName::CircleDashed, faint(theme), "Waiting to hand off");
        }
        match self.row_status(row, cx) {
            RowStatus::Queued => Look::new(IconName::CircleDashed, faint(theme), "Queued"),
            RowStatus::Running => Look::running(theme, "Running"),
            RowStatus::Needs(label) => Look::needs(theme, label),
            RowStatus::Ready => Look::new(
                IconName::CircleCheck,
                theme.success_foreground,
                "Ready for review",
            ),
            RowStatus::Done => Look::new(IconName::GitMerge, theme.muted_foreground, "Done"),
            RowStatus::Stuck(why) => Look::new(IconName::CircleX, theme.danger_foreground, why),
            RowStatus::Skipped => Look::new(IconName::CircleDashed, faint(theme), "Removed"),
        }
    }

    /// A goal: what needs the user in it or its tasks first, else how far it got.
    pub(super) fn goal_look(&self, name: &str, cx: &App) -> Look {
        let theme = cx.theme();
        if let Attn::Needs(label) = self.goal_attention(name, cx) {
            return Look::needs(theme, label);
        }
        match self.goal(name).map(|g| g.state).unwrap_or_default() {
            GoalState::Planning => Look::running(theme, "Planning"),
            GoalState::Review => Look::needs(theme, crate::transcript::PLAN_TO_APPROVE),
            GoalState::Running => Look::running(theme, "Running"),
            GoalState::Paused(Why::You) => {
                Look::new(IconName::CirclePause, theme.muted_foreground, "Paused")
            }
            GoalState::Paused(Why::Time) => Look::new(
                IconName::CirclePause,
                theme.muted_foreground,
                "Paused: agent time used",
            ),
            GoalState::Ready => Look::new(
                IconName::CircleCheck,
                theme.success_foreground,
                "Ready for review",
            ),
            GoalState::Done => Look::new(IconName::GitMerge, theme.muted_foreground, "Done"),
            GoalState::Stopped => Look::new(IconName::CircleX, faint(theme), "Stopped"),
        }
    }
}

/// A team's look from its status (`Shark::team_status`), which callers also read the count from.
pub(super) fn team_look_of(status: TeamStatus, theme: &Theme) -> Look {
    match status {
        TeamStatus::Idle => Look::new(IconName::CircleDashed, faint(theme), "Idle"),
        TeamStatus::Running(n) => Look::running(theme, format!("Running {n}")),
        TeamStatus::Needs(n) => Look::needs(theme, format!("Needs you {n}")),
        TeamStatus::Paused => Look::new(IconName::CirclePause, theme.muted_foreground, "Paused"),
    }
}

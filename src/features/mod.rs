//! Task features; app owns their hosting and cross-feature coordination.
pub(crate) mod artifacts;
pub(crate) mod differ;
pub(crate) mod git;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Feature {
    Diff,
    Git,
    Artifacts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FeatureFocus {
    Main,
    Files,
}

impl Feature {
    /// The task's closed flag for this feature.
    pub(crate) fn closed_mut(self, ws: &mut crate::state::Task) -> &mut bool {
        match self {
            Self::Diff => &mut ws.diff_closed,
            Self::Git => &mut ws.git_closed,
            Self::Artifacts => &mut ws.artifacts_closed,
        }
    }

    /// Whether the task has this feature's tab open.
    pub(crate) fn is_open(self, ws: &crate::state::Task) -> bool {
        !match self {
            Self::Diff => ws.diff_closed,
            Self::Git => ws.git_closed,
            Self::Artifacts => ws.artifacts_closed,
        }
    }
}

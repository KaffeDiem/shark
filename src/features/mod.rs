//! Workspace features; app owns their hosting and cross-feature coordination.
pub(crate) mod differ;
pub(crate) mod git;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Feature {
    Diff,
    Git,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FeatureFocus {
    Main,
    Files,
}

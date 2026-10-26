//! The diff's English strings (upstream's `Diff.*` entries in `locales/ui.yml`). Shark is
//! English-only, so `t!` keeps upstream's call sites and looks the key up here.

use std::borrow::Cow;

macro_rules! t {
    ($key:expr $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::diff_view::strings::lookup($key, &[$((stringify!($name), $value.to_string())),*])
    };
}

pub(super) fn lookup(key: &str, args: &[(&str, String)]) -> Cow<'static, str> {
    let text = match key {
        "Diff.BinaryChanges" => "Binary file changed",
        "Diff.NoTextChanges" => "No text changes in this patch",
        "Diff.NoChanges" => "No changes",
        "Diff.EmptyFile" => "Empty file",
        "Diff.AddedFile" => "Added file",
        "Diff.DeletedFile" => "Deleted file",
        "Diff.RenamedFile" => "Renamed",
        "Diff.CopiedFile" => "Copied",
        "Diff.HiddenLines" => "↑ %{count} unchanged lines",
        "Diff.ExpandAbove" => "Show %{count} lines above",
        "Diff.ExpandBelow" => "Show %{count} lines below",
        "Diff.CollapseFile" => "Collapse %{name}",
        "Diff.ExpandFile" => "Expand %{name}",
        "Diff.AddAnnotation" => "Add annotation to line %{line}",
        "Diff.ConflictedFile" => "Conflicts",
        "Diff.CurrentChange" => "Current change",
        "Diff.IncomingChange" => "Incoming change",
        "Diff.BaseChange" => "Common ancestor",
        "Diff.AcceptCurrent" => "Accept Current",
        "Diff.AcceptIncoming" => "Accept Incoming",
        "Diff.AcceptBoth" => "Accept Both",
        "Diff.AcceptedCurrent" => "Accepted current change",
        "Diff.AcceptedIncoming" => "Accepted incoming change",
        "Diff.AcceptedBoth" => "Accepted both changes",
        "Diff.UndoResolution" => "Undo",
        "Diff.Original" => "Original",
        "Diff.Modified" => "Modified",
        "Diff.ExpandLines" => "Expand %{count} unchanged lines",
        "Diff.SourceLine" => "%{side} line %{line}",
        "Diff.AddedLine" => "%{side} line %{line}, added",
        "Diff.RemovedLine" => "%{side} line %{line}, removed",
        "Diff.UnchangedLines" => "%{count} unchanged lines",
        "Diff.SelectLine" => "Select %{side} line %{line}",
        "Diff.NoNewline" => "No newline at end of file",
        "Diff.Viewer" => "Diff for %{name}",
        "Diff.ViewerFiles" => "Diff for %{count} files",
        _ => unreachable!("unknown diff string {key}"),
    };
    if args.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut text = text.to_string();
    for (name, value) in args {
        text = text.replace(&format!("%{{{name}}}"), value);
    }
    Cow::Owned(text)
}

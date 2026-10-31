//! Small footer projections over cached activity and account snapshots.
use crate::usage::{Snapshot, Status};

pub(super) fn quota_details(name: &str, snapshot: &Snapshot, now: f64) -> Vec<String> {
    let state = status(snapshot);
    let mut lines = Vec::new();
    for row in snapshot
        .allowances
        .iter()
        .filter(|row| row.resets_at.is_none_or(|at| at > now))
    {
        let label = &row.label;
        let remaining = percent(row.remaining.filter(valid_percent));
        let pace = row
            .remaining
            .filter(valid_percent)
            .filter(|_| !snapshot.outdated)
            .map(|left| {
                crate::usage::pace::gauge(
                    100. - left,
                    crate::usage::pace::elapsed(row.resets_at, row.window, now),
                    80.,
                    95.,
                )
                .label()
            });
        lines.push(format!(
            "{name} · {label} · {remaining}{}",
            pace.map(|p| format!(" · {p}")).unwrap_or_default()
        ));
    }
    if let Some(state) = state {
        lines.push(format!("{name} · {state}"));
    } else if lines.is_empty() {
        lines.push(format!("{name} · left unknown"));
    }
    lines
}

/// The percentage and pace must come from the same active quota window.
pub(super) struct Headline {
    pub text: String,
    pub remaining: Option<f64>,
    pub elapsed: Option<f64>,
}

pub(super) fn headline(snapshot: &Snapshot, now: f64) -> Headline {
    let row = snapshot
        .allowances
        .iter()
        .filter(|row| row.resets_at.is_none_or(|at| at > now))
        .filter(|row| row.remaining.is_some_and(|n| valid_percent(&n)))
        .min_by(|a, b| a.remaining.unwrap().total_cmp(&b.remaining.unwrap()));
    // Keep successful percentages during refresh, without adding/removing status rows.
    // Unknown or outdated values stay neutral; full status remains in the tooltip.
    let remaining = row
        .filter(|_| !snapshot.outdated)
        .and_then(|row| row.remaining);
    let elapsed = row
        .filter(|_| remaining.is_some())
        .and_then(|row| crate::usage::pace::elapsed(row.resets_at, row.window, now));
    let text = remaining.map_or_else(
        || {
            if snapshot.status == Status::Loading && !snapshot.outdated {
                "…"
            } else {
                "—"
            }
            .into()
        },
        |n| {
            if n > 0. && n < 1. {
                "<1%".into()
            } else {
                format!("{:.0}%", n.ceil())
            }
        },
    );
    Headline {
        text,
        remaining,
        elapsed,
    }
}

fn valid_percent(value: &f64) -> bool {
    value.is_finite() && (0. ..=100.).contains(value)
}

fn percent(remaining: Option<f64>) -> String {
    remaining.map_or_else(
        || "left unknown".into(),
        |n| {
            if n > 0. && n < 1. {
                "<1% left".into()
            } else {
                format!("{:.0}% left", n.ceil())
            }
        },
    )
}

fn status(snapshot: &Snapshot) -> Option<&'static str> {
    if snapshot.outdated {
        return Some("outdated");
    }
    match snapshot.status {
        Status::Idle => Some("not checked"),
        Status::Loading => Some("checking"),
        Status::Failed(_) => Some("unavailable"),
        Status::Ready => None,
    }
}

#[cfg(test)]
#[path = "footer_usage_tests.rs"]
mod tests;

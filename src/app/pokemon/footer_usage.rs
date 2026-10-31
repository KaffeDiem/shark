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
        lines.push(format!("{name} · {label} · {remaining}"));
    }
    if let Some(state) = state {
        lines.push(format!("{name} · {state}"));
    } else if lines.is_empty() {
        lines.push(format!("{name} · left unknown"));
    }
    lines
}

/// One headline per provider keeps the navigation compact even with many model windows.
pub(super) fn quota_lines(name: &str, snapshot: &Snapshot, now: f64, rail: bool) -> Vec<String> {
    let remaining = snapshot
        .allowances
        .iter()
        .filter(|row| row.resets_at.is_none_or(|at| at > now))
        .filter_map(|row| row.remaining.filter(valid_percent))
        .min_by(f64::total_cmp);
    let state = status(snapshot);
    let value = if remaining.is_none() {
        state.map(str::to_owned).unwrap_or_else(|| percent(None))
    } else {
        percent(remaining)
    };
    let mut lines = if rail {
        vec![name.into(), value]
    } else {
        vec![format!("{name} {value}")]
    };
    if let Some(state) = state.filter(|_| remaining.is_some()) {
        lines.push(if rail {
            state.into()
        } else {
            format!("{name} · {state}")
        });
    }
    lines
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

use super::*;
impl Shark {
    fn headline_utilization(&self) -> Option<f64> {
        let now = crate::usage::now();
        self.usage_providers
            .iter()
            .flat_map(|provider| {
                let mut snapshot = self.usage[provider.index()].clone();
                if snapshot.outdated || snapshot.fetched_at.is_none() {
                    return vec![];
                }
                // A refresh retains its last successful official allowances while loading.
                if snapshot.status == Status::Loading {
                    snapshot.status = Status::Ready;
                }
                // An allowance that reset since the last poll no longer reflects current usage.
                snapshot
                    .allowances
                    .retain(|row| row.resets_at.is_none_or(|at| at > now));
                provider.source().candy_windows(&snapshot)
            })
            .map(|window| window.utilization)
            .filter(|n| n.is_finite() && (0. ..=100.).contains(n))
            .max_by(f64::total_cmp)
    }
    pub(in crate::app) fn observed_headline_warning(&self) -> bool {
        self.headline_utilization().is_some_and(|used| used >= 95.)
    }
}
#[cfg(test)]
mod tests;

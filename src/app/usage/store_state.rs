use super::*;
impl Shark {
    fn headline_utilization(&self, used_today: bool) -> Option<f64> {
        let today = activity::local_day(self.usage_view_now());
        self.usage_providers
            .iter()
            .filter(|provider| {
                !used_today || {
                    let daily = &self.usage_reports[provider.index()].daily;
                    today
                        .checked_sub(daily.first_day)
                        .and_then(|day| usize::try_from(day).ok())
                        .and_then(|index| daily.tokens.get(index))
                        .is_some_and(|n| *n > 0)
                }
            })
            .flat_map(|provider| {
                let mut snapshot = self.usage[provider.index()].clone();
                if snapshot.outdated || snapshot.fetched_at.is_none() {
                    return vec![];
                }
                // A refresh retains its last successful official allowances while loading.
                if snapshot.status == Status::Loading {
                    snapshot.status = Status::Ready;
                }
                provider.source().candy_windows(&snapshot)
            })
            .map(|window| window.utilization)
            .filter(|n| n.is_finite() && (0. ..=100.).contains(n))
            .max_by(f64::total_cmp)
    }
    pub(in crate::app) fn highest_used_headline_limit(&self) -> Option<f64> {
        self.headline_utilization(true)
    }
    pub(in crate::app) fn observed_headline_warning(&self) -> bool {
        self.headline_utilization(false)
            .is_some_and(|used| used >= 95.)
    }
}
#[cfg(test)]
mod tests;

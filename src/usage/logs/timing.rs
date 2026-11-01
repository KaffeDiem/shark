use super::{Bucket, Report};
#[derive(Clone, Debug)]
pub struct ActiveBlock {
    pub tokens_per_minute: f64,
}
impl Report {
    pub fn active_block(&self, now: f64) -> Option<ActiveBlock> {
        let mut recent: Vec<_> = self
            .entries
            .iter()
            .filter(|e| e.total() > 0 && e.date >= now - 5. * 3600. && e.date <= now)
            .collect();
        recent.sort_by(|a, b| a.date.total_cmp(&b.date));
        let start = recent.first()?.date;
        let mut bucket = Bucket::default();
        for e in recent {
            bucket.add(e)
        }
        Some(ActiveBlock {
            tokens_per_minute: bucket.total() as f64 / ((now - start) / 60.).max(1.),
        })
    }
}

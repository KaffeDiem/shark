use super::{Bucket, Report};
use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Utc};
#[derive(Clone, Debug)]
pub struct ActiveBlock {
    pub start: f64,
    pub end: f64,
    pub tokens_per_minute: f64,
    pub bucket: Bucket,
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
            start,
            end: start + 5. * 3600.,
            tokens_per_minute: bucket.total() as f64 / ((now - start) / 60.).max(1.),
            bucket,
        })
    }
}
fn datetime(now: f64) -> DateTime<Local> {
    DateTime::<Utc>::from_timestamp(now as i64, 0)
        .unwrap_or(DateTime::UNIX_EPOCH)
        .with_timezone(&Local)
}
pub fn month_start_in<Tz: TimeZone>(date: DateTime<Tz>) -> f64 {
    let day = date.date_naive().with_day(1).unwrap();
    date.timezone()
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map_or(date.timestamp(), |d| d.timestamp()) as f64
}
pub fn week_start_in<Tz: TimeZone>(date: DateTime<Tz>, first_weekday: u32) -> f64 {
    let days = (date.weekday().num_days_from_monday() + 7 - first_weekday % 7) % 7;
    let day = date.date_naive() - Duration::days(days as i64);
    date.timezone()
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map_or(date.timestamp(), |d| d.timestamp()) as f64
}
pub fn month_start(now: f64) -> f64 {
    month_start_in(datetime(now))
}
pub fn week_start(now: f64) -> f64 {
    week_start_in(datetime(now), 0)
}
pub fn month_key(now: f64) -> String {
    datetime(now).format("%Y-%m").to_string()
}
pub fn enrichment_start(now: f64) -> f64 {
    month_start(now).min(week_start(now)).min(now - 7. * 86400.)
}

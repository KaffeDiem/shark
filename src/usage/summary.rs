//! Calendar summaries of local usage. Dates are calendar days, never fixed 24-hour increments.
use super::logs::{Bucket, Entry, Report};
pub mod combined;
pub mod external;
use chrono::{DateTime, Datelike, Days, Local, Months, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq)]
pub struct Day {
    pub date: NaiveDate,
    pub bucket: Bucket,
}
fn utc(seconds: f64) -> DateTime<Utc> {
    DateTime::from_timestamp(seconds as i64, 0).unwrap_or(DateTime::UNIX_EPOCH)
}
pub fn local_date(seconds: f64) -> NaiveDate {
    utc(seconds).with_timezone(&Local).date_naive()
}
impl Report {
    pub fn month_daily(&self, now: f64) -> Vec<Day> {
        self.month_daily_in(utc(now).with_timezone(&Local))
    }
    pub fn month_daily_in<Tz: TimeZone>(&self, now: DateTime<Tz>) -> Vec<Day> {
        let today = now.date_naive();
        let start = today.with_day(1).unwrap();
        if self.entries.is_empty() {
            return self
                .fallback_series()
                .into_iter()
                .filter(|d| d.date >= start && d.date <= today)
                .collect();
        }
        let mut buckets = BTreeMap::<NaiveDate, Bucket>::new();
        for e in &self.entries {
            let day = utc(e.date).with_timezone(&now.timezone()).date_naive();
            if day >= start && day <= today {
                buckets.entry(day).or_default().add(e)
            }
        }
        dates(start, today.succ_opt().unwrap())
            .map(|date| Day {
                date,
                bucket: buckets.remove(&date).unwrap_or_default(),
            })
            .collect()
    }
    fn fallback_series(&self) -> Vec<Day> {
        self.daily
            .tokens
            .iter()
            .enumerate()
            .filter_map(|(i, n)| {
                let day = self
                    .daily
                    .first_day
                    .checked_add(i as i64)?
                    .checked_add(719163)?;
                let date = NaiveDate::from_num_days_from_ce_opt(i32::try_from(day).ok()?)?;
                Some(Day {
                    date,
                    bucket: Bucket {
                        input: *n,
                        coverage: super::logs::CostCoverage {
                            unknown: *n > 0,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                })
            })
            .collect()
    }
    pub fn last_usage(&self) -> Option<f64> {
        if self.entries.is_empty() {
            return self
                .fallback_series()
                .into_iter()
                .filter(|d| d.bucket.total() > 0)
                .filter_map(|d| {
                    Local
                        .from_local_datetime(&d.date.and_hms_opt(12, 0, 0)?)
                        .earliest()
                        .map(|d| d.timestamp() as f64)
                })
                .max_by(f64::total_cmp);
        }
        self.entries
            .iter()
            .filter(|e| e.total() > 0)
            .map(|e| e.date)
            .max_by(f64::total_cmp)
    }
    pub fn period(&self, start: NaiveDate, end: NaiveDate) -> Bucket {
        let mut out = Bucket::default();
        if self.entries.is_empty() {
            for day in self.fallback_series() {
                if day.date >= start && day.date < end {
                    out.merge(&day.bucket)
                }
            }
            return out;
        }
        for e in &self.entries {
            let day = local_date(e.date);
            if day >= start && day < end {
                out.add(e)
            }
        }
        out
    }
}
pub fn combine_series(series: impl IntoIterator<Item = Vec<Day>>) -> Vec<Day> {
    let mut days = BTreeMap::<NaiveDate, Bucket>::new();
    for series in series {
        for day in series {
            days.entry(day.date).or_default().merge(&day.bucket)
        }
    }
    days.into_iter()
        .map(|(date, bucket)| Day { date, bucket })
        .collect()
}
pub fn dates(start: NaiveDate, end: NaiveDate) -> impl Iterator<Item = NaiveDate> {
    std::iter::successors(Some(start), |d| d.succ_opt()).take_while(move |d| *d < end)
}
/// Numeric history only. Covered zero days are distinct from days never observed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UsageLedger {
    pub tokens_by_day: BTreeMap<String, u64>,
    pub covered_since: Option<String>,
}
impl UsageLedger {
    pub fn tokens(&self, date: NaiveDate) -> u64 {
        self.tokens_by_day
            .get(&date.to_string())
            .copied()
            .unwrap_or(0)
    }
    pub fn covers(&self, date: NaiveDate) -> bool {
        self.covered_since
            .as_deref()
            .is_some_and(|since| date.to_string().as_str() >= since)
    }
    pub fn merge(&mut self, series: &[Day]) {
        for day in series {
            let key = day.date.to_string();
            if day.bucket.total() > 0 {
                let old = self.tokens_by_day.entry(key.clone()).or_default();
                *old = (*old).max(day.bucket.total())
            }
            if self.covered_since.as_ref().is_none_or(|since| key < *since) {
                self.covered_since = Some(key)
            }
        }
    }
    pub fn prune(&mut self, before: NaiveDate) {
        let before = before.to_string();
        self.tokens_by_day.retain(|day, _| *day >= before);
        if self.covered_since.as_ref().is_some_and(|s| *s < before) {
            self.covered_since = Some(before)
        }
    }
    pub fn record(&mut self, series: &[Day], now: NaiveDate) {
        self.merge(series);
        if let Some(start) = NaiveDate::from_ymd_opt(now.year() - 1, 1, 1) {
            self.prune(start)
        }
    }
}
impl<'de> Deserialize<'de> for UsageLedger {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let mut out = Self::default();
        if let Some(rows) = v["tokens_by_day"].as_object() {
            for (k, v) in rows {
                if NaiveDate::parse_from_str(k, "%Y-%m-%d").is_ok_and(|date| date.to_string() == *k)
                {
                    if let Some(n) = v.as_u64().filter(|n| *n > 0) {
                        out.tokens_by_day.insert(k.clone(), n);
                    }
                }
            }
        }
        out.covered_since = v["covered_since"]
            .as_str()
            .filter(|s| {
                NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok_and(|date| date.to_string() == *s)
            })
            .map(str::to_owned);
        Ok(out)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Week,
    Month,
    Year,
}
impl Scope {
    pub const ALL: [Self; 3] = [Self::Week, Self::Month, Self::Year];
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Period {
    pub scope: Scope,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub first_weekday: u32,
}
impl Period {
    /// `first_weekday`: Monday=0 through Sunday=6.
    pub fn containing(scope: Scope, date: NaiveDate, first_weekday: u32) -> Self {
        let start = match scope {
            Scope::Week => {
                date - Days::new(
                    ((date.weekday().num_days_from_monday() + 7 - first_weekday % 7) % 7) as u64,
                )
            }
            Scope::Month => date.with_day(1).unwrap(),
            Scope::Year => NaiveDate::from_ymd_opt(date.year(), 1, 1).unwrap(),
        };
        let end = match scope {
            Scope::Week => start + Days::new(7),
            Scope::Month => start + Months::new(1),
            Scope::Year => start + Months::new(12),
        };
        Self {
            scope,
            start,
            end,
            first_weekday,
        }
    }
    pub fn shifted(self, offset: i32) -> Self {
        let magnitude = offset.unsigned_abs();
        let next = match self.scope {
            Scope::Week => {
                if offset < 0 {
                    self.start.checked_sub_days(Days::new(magnitude as u64 * 7))
                } else {
                    self.start.checked_add_days(Days::new(magnitude as u64 * 7))
                }
            }
            Scope::Month | Scope::Year => {
                let months = Months::new(magnitude.saturating_mul(if self.scope == Scope::Year {
                    12
                } else {
                    1
                }));
                if offset < 0 {
                    self.start.checked_sub_months(months)
                } else {
                    self.start.checked_add_months(months)
                }
            }
        }
        .unwrap_or(self.start);
        Self::containing(self.scope, next, self.first_weekday)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meter {
    pub date: NaiveDate,
    pub tokens: u64,
    pub current: bool,
    pub has_data: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Graduation {
    pub id: String,
    pub date: NaiveDate,
    pub released: bool,
}
#[derive(Clone, Debug)]
pub struct Recap {
    pub period: Period,
    pub buckets: Vec<Meter>,
    pub total: u64,
    pub in_progress: bool,
    pub previous_total: Option<u64>,
    pub best_day: Option<(NaiveDate, u64)>,
    pub active_days: usize,
    pub counted_days: usize,
    pub best_streak: usize,
    pub graduated: Vec<Graduation>,
    pub can_go_back: bool,
}
impl Recap {
    pub fn delta(&self) -> Option<f64> {
        self.previous_total
            .filter(|n| *n > 0)
            .map(|p| (self.total as f64 - p as f64) / p as f64)
    }
    pub fn make(
        ledger: &UsageLedger,
        graduated: &[Graduation],
        period: Period,
        today: NaiveDate,
    ) -> Self {
        let known = |d| ledger.tokens(d) > 0 || ledger.covers(d);
        let elapsed: Vec<_> = dates(period.start, period.end)
            .filter(|d| *d <= today)
            .collect();
        let mut streak = 0;
        let mut best_streak = 0;
        for d in &elapsed {
            streak = if ledger.tokens(*d) > 0 { streak + 1 } else { 0 };
            best_streak = best_streak.max(streak)
        }
        let buckets = if period.scope == Scope::Year {
            (0..12)
                .map(|m| {
                    let start = period.start + Months::new(m);
                    let end = start + Months::new(1);
                    let elapsed: Vec<_> = dates(start, end).filter(|d| *d <= today).collect();
                    Meter {
                        date: start,
                        tokens: elapsed
                            .iter()
                            .map(|d| ledger.tokens(*d))
                            .fold(0, u64::saturating_add),
                        current: start <= today && today < end,
                        has_data: elapsed.iter().any(|d| known(*d)),
                    }
                })
                .collect()
        } else {
            dates(period.start, period.end)
                .map(|date| Meter {
                    date,
                    tokens: ledger.tokens(date),
                    current: date == today,
                    has_data: date <= today && known(date),
                })
                .collect()
        };
        let in_progress = period.start <= today && today < period.end;
        let previous = period.shifted(-1);
        let previous_days: Vec<_> = dates(previous.start, previous.end).collect();
        let compared = if in_progress {
            &previous_days[..elapsed.len().min(previous_days.len())]
        } else {
            &previous_days
        };
        let previous_total = compared.first().filter(|d| ledger.covers(**d)).map(|_| {
            compared
                .iter()
                .map(|d| ledger.tokens(*d))
                .fold(0, u64::saturating_add)
        });
        let mut graduated: Vec<_> = graduated
            .iter()
            .filter(|g| !g.released && g.date >= period.start && g.date < period.end)
            .cloned()
            .collect();
        graduated.sort_by(|a, b| b.date.cmp(&a.date));
        Self {
            period,
            buckets,
            total: elapsed
                .iter()
                .map(|d| ledger.tokens(*d))
                .fold(0, u64::saturating_add),
            in_progress,
            previous_total,
            best_day: elapsed
                .iter()
                .map(|d| (*d, ledger.tokens(*d)))
                .filter(|(_, n)| *n > 0)
                .max_by_key(|(_, n)| *n),
            active_days: elapsed.iter().filter(|d| ledger.tokens(**d) > 0).count(),
            counted_days: elapsed.iter().filter(|d| known(**d)).count(),
            best_streak,
            graduated,
            can_go_back: previous_days.last().is_some_and(|d| ledger.covers(*d)),
        }
    }
}
#[cfg(test)]
mod tests;
#[derive(Clone, Debug)]
pub struct ProviderSeries {
    pub id: String,
    pub name: String,
    pub days: Option<Vec<Day>>,
    pub reports_cost: bool,
}
pub fn combined_provider_series(providers: &[ProviderSeries]) -> Vec<Day> {
    combine_series(providers.iter().filter_map(|p| {
        p.days.as_ref().map(|days| {
            days.iter()
                .cloned()
                .map(|mut d| {
                    if !p.reports_cost {
                        d.bucket.cost = 0.;
                        d.bucket.coverage = Default::default()
                    }
                    d
                })
                .collect()
        })
    }))
}
#[cfg(test)]
mod combined_tests;
#[cfg(test)]
mod external_tests;

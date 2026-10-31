use super::*;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BurnTier {
    #[default]
    Idle,
    Normal,
    Fast,
    Blazing,
}
impl BurnTier {
    pub fn from_rate(rate: f64) -> Self {
        if !rate.is_finite() || rate <= 1000. {
            Self::Idle
        } else if rate < 100000. {
            Self::Normal
        } else if rate < 400000. {
            Self::Fast
        } else {
            Self::Blazing
        }
    }
}
pub struct Source<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub report: &'a Report,
    pub reports_cost: bool,
}
#[derive(Clone, Debug)]
pub struct ProviderSummary {
    pub id: String,
    pub name: String,
    pub today: Bucket,
    pub week: Bucket,
    pub month: Bucket,
    pub rate: f64,
    pub last_usage: Option<f64>,
    pub month_daily: Vec<Day>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub providers: Vec<ProviderSummary>,
    pub today: Bucket,
    pub week: Bucket,
    pub month: Bucket,
    pub month_daily: Vec<Day>,
    pub today_by_provider: BTreeMap<String, u64>,
    pub rate: f64,
    pub burn: BurnTier,
}
impl Summary {
    pub fn from_sources<'a>(sources: impl IntoIterator<Item = Source<'a>>, now: f64) -> Self {
        let today = local_date(now);
        let tomorrow = today.succ_opt().unwrap();
        let week = Period::containing(Scope::Week, today, 0).start;
        let month = today.with_day(1).unwrap();
        let mut out = Self::default();
        for source in sources {
            let r = source.report;
            let mut p = ProviderSummary {
                id: source.id.into(),
                name: source.name.into(),
                today: r.period(today, tomorrow),
                week: r.period(week, tomorrow),
                month: r.period(month, tomorrow),
                rate: r.active_block(now).map_or(0., |b| b.tokens_per_minute),
                last_usage: r.last_usage(),
                month_daily: r.month_daily(now),
                error: r.error.clone(),
            };
            if !source.reports_cost {
                for b in [&mut p.today, &mut p.week, &mut p.month] {
                    b.cost = 0.;
                    b.coverage = Default::default()
                }
                for day in &mut p.month_daily {
                    day.bucket.cost = 0.;
                    day.bucket.coverage = Default::default()
                }
            }
            if p.today.total() == 0
                && p.week.total() == 0
                && p.month.total() == 0
                && p.rate == 0.
                && !p
                    .last_usage
                    .is_some_and(|date| date <= now && date >= now - 7. * 86400.)
            {
                continue;
            }
            if p.today.total() > 0 {
                out.today_by_provider.insert(p.id.clone(), p.today.total());
            }
            out.today.merge(&p.today);
            out.week.merge(&p.week);
            out.month.merge(&p.month);
            out.rate = (out.rate + p.rate).min(f64::MAX);
            out.providers.push(p);
        }
        out.month_daily = combine_series(out.providers.iter().map(|p| p.month_daily.clone()));
        out.burn = BurnTier::from_rate(out.rate);
        out
    }
    pub fn preferring(&self, id: Option<&str>) -> Option<&ProviderSummary> {
        id.and_then(|id| self.providers.iter().find(|p| p.id == id))
            .or_else(|| self.providers.first())
    }
}

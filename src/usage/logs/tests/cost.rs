use super::*;
use crate::pokemon::format::compact_cost;
fn e(model: &str) -> Entry {
    Entry {
        id: model.into(),
        date: epoch(15),
        model: model.into(),
        input: 1000,
        output: 100,
        cache_read: 500,
        ..Entry::default()
    }
}
#[test]
fn plain_dollar_all_languages() {
    {
        for coverage in [
            CostCoverage {
                reported: true,
                ..Default::default()
            },
            CostCoverage {
                estimated: true,
                ..Default::default()
            },
            CostCoverage {
                reported: true,
                unknown: true,
                ..Default::default()
            },
            CostCoverage {
                estimated: true,
                unknown: true,
                ..Default::default()
            },
        ] {
            let b = Bucket {
                cost: 12.5,
                coverage,
                ..Default::default()
            };
            assert_eq!(b.cost_text(false), "$12.50");
            assert_eq!(b.cost_text(true), "$12.5")
        }
    }
}
#[test]
fn explicit_zero_is_known() {
    let mut zero = e("gpt-5.5");
    zero.explicit_cost = Some(0.);
    let b = bucket(&[zero]);
    assert_eq!(b.cost, 0.);
    assert!(b.coverage.reported);
    assert!(!b.coverage.estimated);
    assert_eq!(b.cost_text(false), "$0.00");
    let b = bucket(&[e("gpt-5.5")]);
    close(b.cost, 0.00825);
    assert!(b.coverage.estimated);
    assert_eq!(b.cost_text(false), "$0.01")
}
#[test]
fn partial_coverage_survives_all_reports() {
    let unknown = e("not-priced");
    let b = bucket(&[unknown.clone()]);
    assert_eq!(b.total(), 1600);
    assert_eq!(b.cost_text(false), "Unavailable");
    assert_eq!(b.cost_text(true), "$—");
    let mut reported = e("gpt-5.5");
    reported.explicit_cost = Some(0.);
    let report = Report::from_entries(vec![e("gpt-5.5"), unknown, reported], epoch(15));
    let expected = CostCoverage {
        reported: true,
        estimated: true,
        unknown: true,
    };
    assert_eq!(report.totals.coverage, expected);
    assert_eq!(between(&report, epoch(14), epoch(16)).coverage, expected);
    assert!(
        daily_buckets(&report)
            .values()
            .all(|b| b.coverage == expected)
    );
    assert_eq!(report.totals.total(), 4800);
    assert_eq!(report.totals.cost_text(false), "$0.01");
    assert!(report.totals.cost_explanation().contains("Some usage"));
    let mut merged = Bucket::default();
    for b in daily_buckets(&report).values() {
        merged.merge(b)
    }
    assert_eq!(merged.coverage, expected)
}
#[test]
fn empty_usage_does_not_claim_estimate() {
    let b = bucket(&[
        e("unknown"),
        Entry {
            model: "gpt-5.5".into(),
            ..Default::default()
        },
    ]);
    assert!(b.coverage.unknown);
    assert!(!b.coverage.has_known());
    assert_eq!(b.cost_text(false), "Unavailable")
}
#[test]
fn invalid_source_cost_fallback() {
    for bad in [f64::NAN, f64::INFINITY, -1.] {
        let mut entry = e("gpt-5.5");
        entry.explicit_cost = Some(bad);
        let b = bucket(&[entry]);
        assert!(b.coverage.estimated);
        assert!(b.cost.is_finite())
    }
    let mut unavailable = e("gpt-5.5");
    unavailable.cost_unavailable = Some(true);
    assert!(bucket(&[unavailable.clone()]).coverage.unknown);
    unavailable.explicit_cost = Some(0.5);
    assert!(bucket(&[unavailable.clone()]).coverage.reported);
    unavailable.explicit_cost = Some(0.);
    unavailable.cost_is_estimate = Some(true);
    assert!(bucket(&[unavailable]).coverage.estimated)
}
#[test]
fn claude_cost_cold_warm() {
    let d = Temp::new();
    let bytes = lines(&[assistant("a", "claude-sonnet-5-5", 100, 10, 40, 1000, 15)]);
    let source = d.write("s.jsonl", &bytes);
    let time = fs::metadata(&source).unwrap().modified().unwrap();
    let mut c = Cache::default();
    let check = |es: Vec<Entry>| {
        assert_eq!(es.len(), 1);
        assert_eq!(es[0].explicit_cost, None);
        let r = Report::from_entries(es, epoch(15) + 60.);
        assert_eq!(r.today(), 1150);
        for b in [r.totals.clone(), between(&r, epoch(14), epoch(16))] {
            close(b.cost, 0.0005);
            assert!(b.coverage.estimated)
        }
    };
    check(c.entries(Provider::Claude, &d.roots()));
    d.write("s.jsonl", vec![b' '; bytes.len()]);
    fs::File::options()
        .write(true)
        .open(source)
        .unwrap()
        .set_modified(time)
        .unwrap();
    check(c.entries(Provider::Claude, &d.roots()));
    assert_eq!(c.parsed_files, 1)
}
#[test]
fn codex_cost_cache_preserves_partial_coverage() {
    let d = Temp::new();
    d.write(
        "a.jsonl",
        lines(&[
            ctx("gpt-5.5"),
            event(1., None, [1500, 500, 0, 100, 0, 1600]),
            event(2., None, [0, 0, 0, 0, 0, 70]),
        ]),
    );
    let mut cache = Cache::default();
    for _ in 0..2 {
        let es = cache.entries(Provider::Codex, &d.roots());
        let b = bucket(&es);
        assert_eq!(b.total(), 1670);
        close(b.cost, 0.00825);
        assert_eq!(
            b.coverage,
            CostCoverage {
                estimated: true,
                unknown: true,
                reported: false
            }
        );
        let r = Report::from_entries(es, epoch(15) + 60.);
        assert_eq!(r.totals.coverage, b.coverage)
    }
}
#[test]
fn legacy_price_identities() {
    use super::super::pricing::*;
    close(estimate("claude-opus-4-8", 1000000, 0, 0, 0).unwrap(), 5.);
    close(estimate("claude-opus-4-8", 0, 1000000, 0, 0).unwrap(), 25.);
    close(
        estimate("claude-haiku-4-5-20251001", 1000000, 0, 0, 0).unwrap(),
        1.,
    );
    close(
        estimate("claude-fable-5", 1000000, 1000000, 1000000, 1000000).unwrap(),
        73.5,
    );
    close(
        estimate("claude-fable-5-1", 0, 0, 0, 1000000).unwrap(),
        0.25,
    );
    for m in [
        "claude-opus-4-99",
        "claude-fable-6",
        "claude-opus-6",
        "totally-unknown",
    ] {
        assert!(rate(m).is_none())
    }
}
#[test]
fn period_and_active_window() {
    let now = epoch(15);
    let recent = Entry {
        date: now - 1800.,
        input: 600000,
        ..e("claude-opus-4-8")
    };
    let old = Entry {
        date: now - 36000.,
        input: 999,
        output: 0,
        cache_read: 0,
        ..e("claude-opus-4-8")
    };
    let mut recent = recent;
    recent.output = 0;
    recent.cache_read = 0;
    let r = Report::from_entries(vec![recent, old], now);
    let b = r.active_block(now).unwrap();
    assert!(b.tokens_per_minute > 0.);
    assert_eq!(between(&r, now - 43200., now + 43200.).total(), 600999)
}
#[test]
fn synthetic_zero_not_active() {
    let now = epoch(15);
    let synthetic = Entry {
        date: now - 120.,
        model: "<synthetic>".into(),
        ..Default::default()
    };
    assert!(
        Report::from_entries(vec![synthetic.clone()], now)
            .active_block(now)
            .is_none()
    );
    let actual = Entry {
        date: now - 60.,
        input: 10,
        output: 5,
        ..Default::default()
    };
    let block = Report::from_entries(vec![synthetic, actual], now)
        .active_block(now)
        .unwrap();
    assert!(block.tokens_per_minute > 0.)
}
#[test]
fn claude_absurd_tokens_bounded() {
    let mut v = assistant("a", "claude", 0, 0, 0, 0, 15);
    v["message"]["usage"]["input_tokens"] = json!(1e30);
    v["message"]["usage"]["output_tokens"] = json!(1e30);
    let p = super::super::claude::parse(&lines(&[v]), Path::new("a"), 0);
    assert_eq!(p.entries[0].input, 1000000000000000);
    assert_eq!(p.entries[0].output, 1000000000000000)
}
#[test]
fn codex_absurd_tokens_bounded() {
    let mut v = event(1., Some([10, 0, 0, 5, 0, 15]), [0; 6]);
    v["payload"]["info"]["last_token_usage"]["input_tokens"] = json!(1e30);
    v["payload"]["info"]["last_token_usage"]["output_tokens"] = json!(1e30);
    let p = rollout("a", &[v]);
    assert_eq!(p.events[0].entry.input, 1000000000000000);
    assert_eq!(p.events[0].entry.output, 1000000000000000)
}
#[test]
fn malformed_token_fields_zero() {
    let mut v = assistant("a", "claude", 0, 0, 0, 0, 15);
    v["message"]["usage"] =
        json!({"input_tokens":-5,"output_tokens":null,"cache_read_input_tokens":"nope"});
    let p = super::super::claude::parse(&lines(&[v]), Path::new("a"), 0);
    assert_eq!(p.entries[0].total(), 0)
}
#[test]
fn rounded_cost_boundaries() {
    for (n, w) in [
        (99.96, "$100"),
        (99.94, "$99.9"),
        (9999.6, "$10.0K"),
        (9999.4, "$9999"),
        (100., "$100"),
    ] {
        assert_eq!(compact_cost(n), w)
    }
}
#[test]
fn finite_cost_saturates() {
    let b = bucket(&[
        Entry {
            explicit_cost: Some(f64::MAX),
            ..e("unknown")
        },
        Entry {
            explicit_cost: Some(f64::MAX),
            ..e("unknown")
        },
    ]);
    assert_eq!(b.cost, f64::MAX);
    let mut merged = b.clone();
    merged.merge(&b);
    assert!(merged.cost.is_finite())
}
#[test]
fn all_cost_states() {
    for coverage in [
        CostCoverage {
            estimated: true,
            ..Default::default()
        },
        CostCoverage {
            reported: true,
            ..Default::default()
        },
        CostCoverage {
            unknown: true,
            ..Default::default()
        },
        CostCoverage {
            estimated: true,
            unknown: true,
            ..Default::default()
        },
    ] {
        let b = Bucket {
            cost: 12.5,
            coverage,
            ..Default::default()
        };
        assert!(!b.cost_text(false).is_empty());
        assert!(!b.cost_explanation().is_empty())
    }
}
// The upstream opt-in diagnostic is adapted to immutable fixtures: CI never reads user logs.
#[test]
fn fixture_codex_cost_audit() {
    let d = Temp::new();
    d.write(
        "a.jsonl",
        lines(&[
            ctx("gpt-5.5"),
            event(1., None, [1500, 500, 0, 100, 0, 1600]),
            event(2., None, [0, 0, 0, 0, 0, 70]),
        ]),
    );
    let es = Cache::default().entries(Provider::Codex, &d.roots());
    let b = bucket(&es);
    assert_eq!(b.total(), es.iter().map(Entry::total).sum::<u64>());
    assert!(b.cost > 0.);
    assert!(b.coverage.estimated);
    let unknown = es
        .iter()
        .filter(|e| {
            e.cost_unavailable == Some(true)
                || super::super::pricing::estimate(
                    &e.model,
                    e.input,
                    e.output,
                    e.cache_write,
                    e.cache_read,
                )
                .is_none()
        })
        .map(Entry::total)
        .sum::<u64>();
    assert_eq!(unknown, 70);
    assert_eq!(b.cost_text(false), "$0.01")
}

#[test]
fn grouped_and_cost_edges() {
    use crate::pokemon::format::grouped;
    assert_eq!(grouped(253412890, ','), "253,412,890");
    assert_eq!(grouped(253412890, '.'), "253.412.890");
    assert_eq!(compact_cost(9.54), "$9.5");
    assert_eq!(compact_cost(311.4), "$311");
    assert_eq!(compact_cost(12340.), "$12.3K");
}

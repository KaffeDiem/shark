use super::*;
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
fn claude_totals_cold_warm() {
    let d = Temp::new();
    let bytes = lines(&[assistant("a", "claude-sonnet-5-5", 100, 10, 40, 1000, 15)]);
    let source = d.write("s.jsonl", &bytes);
    let time = fs::metadata(&source).unwrap().modified().unwrap();
    let mut c = Cache::default();
    let check = |es: Vec<Entry>| {
        assert_eq!(es.len(), 1);
        let r = Report::from_entries(es, epoch(15) + 60.);
        assert_eq!(r.today(), 1150);
        for b in [r.totals.clone(), between(&r, epoch(14), epoch(16))] {
            assert_eq!(b.total(), 1150)
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
fn codex_cache_preserves_totals() {
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
        let r = Report::from_entries(es, epoch(15) + 60.);
        assert_eq!(r.totals.total(), 1670)
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
fn grouped_edges() {
    use crate::usage::format::grouped;
    assert_eq!(grouped(253412890, ','), "253,412,890");
    assert_eq!(grouped(253412890, '.'), "253.412.890");
}

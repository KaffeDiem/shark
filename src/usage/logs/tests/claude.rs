use super::super::claude::*;
use super::*;
fn parse_lines(v: &[Value]) -> Vec<Entry> {
    parse(&lines(v), Path::new("session.jsonl"), 0).entries
}
#[test]
fn streamed_keep_largest_and_earliest() {
    let e = parse_lines(&[
        assistant("a", "claude-opus-5", 10, 20, 30, 40, 15),
        assistant("a", "claude-opus-5", 10, 100, 30, 40, 16),
    ]);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].total(), 180);
    assert_eq!(e[0].date, epoch(15))
}
#[test]
fn claude_daily_total() {
    let e = parse_lines(&[assistant("a", "claude-sonnet-4", 1000, 100, 200, 300, 15)]);
    let r = Report::from_entries(e, epoch(15));
    assert_eq!(r.today(), 1600)
}
#[test]
fn subagent_provenance_parent_session() {
    assert_eq!(
        session(Path::new("projects/session-1/subagents/agent-x.jsonl")),
        "session-1"
    );
    let a = Entry {
        id: "a".into(),
        date: 2.,
        input: 10,
        session_ids: ["parent".into()].into(),
        ..Entry::default()
    };
    let mut b = a.clone();
    b.date = 1.;
    b.output = 20;
    b.session_ids = ["child".into()].into();
    let e = dedup_claude(vec![a, b]);
    assert_eq!(e[0].session_ids.len(), 2);
    assert_eq!(e[0].date, 1.);
    assert_eq!(e[0].total(), 30)
}
#[test]
fn records_without_any_id_are_not_merged() {
    let anon = |output| {
        let mut v = assistant("", "claude-sonnet-5", 1, output, 0, 0, 15);
        v["message"].as_object_mut().unwrap().remove("id");
        v.as_object_mut().unwrap().remove("requestId");
        v
    };
    let e = parse_lines(&[anon(2), anon(3)]);
    assert_eq!(e.iter().map(|e| e.output).sum::<u64>(), 5);
}

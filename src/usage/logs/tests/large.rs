use super::*;
use std::{
    io::{BufWriter, Write},
    time::Instant,
};
fn resident_peak() -> u64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // getrusage initializes the structure on success; it does not inspect any user data.
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    assert_eq!(result, 0);
    let peak = unsafe { usage.assume_init() }.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        peak
    } else {
        peak.saturating_mul(1024)
    }
}
// upstream: CodexLargeRolloutPerformanceTests.swift::testCodexLargeRolloutStaysWithinMemoryBudget
#[test]
#[ignore = "Explicit 512 MiB temporary-file memory and throughput benchmark"]
fn codex_512_mib_rollout_stays_within_memory_budget() {
    const TARGET: usize = 512 * 1024 * 1024;
    let temp = Temp::new();
    let path = temp.0.join("large.jsonl");
    let mut writer = BufWriter::new(fs::File::create(&path).unwrap());
    let meta = lines(&[meta("large-rollout", None, false)]);
    writer.write_all(&meta).unwrap();
    let mut written = meta.len();
    let noise = lines(&[
        json!({"type":"event_msg","timestamp":"2026-09-15T12:00:00Z","payload":{"type":"agent_message_delta","delta":"x".repeat(8192)}}),
    ]);
    let (mut count, mut since, mut input, mut read, mut output) = (0u64, 0usize, 0u64, 0u64, 0u64);
    while written < TARGET {
        if count < 64 && since >= 1024 {
            count += 1;
            since = 0;
            let i = 1000 + count;
            let c = 100 + count;
            let o = 50 + count;
            input += i;
            read += c;
            output += o;
            let bytes = lines(&[event(
                (count % 60) as f64,
                Some([input, read, 0, output, 0, input + output]),
                [i, c, 0, o, 0, i + o],
            )]);
            writer.write_all(&bytes).unwrap();
            written += bytes.len();
        } else {
            writer.write_all(&noise).unwrap();
            written += noise.len();
            since += 1;
        }
    }
    writer.flush().unwrap();
    drop(writer);
    assert!(written >= TARGET && written < TARGET + 16 * 1024);
    assert!(count > 10);
    let baseline = resident_peak();
    let started = Instant::now();
    let entries = Cache::default().entries(Provider::Codex, &temp.roots());
    let elapsed = started.elapsed();
    let peak = resident_peak();
    assert_eq!(entries.len(), count as usize);
    assert_eq!(entries.iter().map(|e| e.input).sum::<u64>(), input - read);
    assert_eq!(entries.iter().map(|e| e.cache_read).sum::<u64>(), read);
    assert_eq!(entries.iter().map(|e| e.output).sum::<u64>(), output);
    assert_eq!(
        entries.iter().map(Entry::total).sum::<u64>(),
        input + output
    );
    println!(
        "512 MiB rollout: {:.2}s, peak {} MiB, growth {} MiB",
        elapsed.as_secs_f64(),
        peak / 1024 / 1024,
        peak.saturating_sub(baseline) / 1024 / 1024
    );
    assert!(peak < 700 * 1024 * 1024);
    assert!(peak.saturating_sub(baseline) < 192 * 1024 * 1024);
    assert!(elapsed.as_secs_f64() < 20.);
}
#[test]
fn streamed_rollout_skips_oversized_transcript_records_and_keeps_following_usage() {
    let d = Temp::new();
    let mut data = lines(&[meta("bounded", None, false)]);
    data.extend(std::iter::repeat_n(b'x', 9 * 1024 * 1024));
    data.push(b'\n');
    data.extend(lines(&[event(
        1.,
        Some([100, 20, 0, 30, 0, 130]),
        [100, 20, 0, 30, 0, 130],
    )]));
    let r = super::super::codex::parse_reader(
        std::io::BufReader::with_capacity(8192, std::io::Cursor::new(data)),
        &d.0.join("bounded.jsonl"),
    )
    .unwrap();
    assert_eq!(r.events.len(), 1);
    assert_eq!(r.events[0].entry.total(), 130);
    assert_eq!(r.session.as_deref(), Some("bounded"));
}
#[test]
fn streaming_io_errors_are_reported_instead_of_publishing_partial_usage() {
    struct Broken;
    impl std::io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("injected failure"))
        }
    }
    assert!(
        super::super::codex::parse_reader(std::io::BufReader::new(Broken), Path::new("broken"))
            .is_err()
    );
}

#[test]
fn streamed_claude_records_preserve_incremental_offsets_and_skip_oversized_prompts() {
    let mut data = vec![b'x'; 9 * 1024 * 1024];
    data.push(b'\n');
    let valid = lines(&[assistant("a", "claude-sonnet-4-6", 100, 20, 30, 40, 15)]);
    data.extend(&valid);
    let checkpoint = data.len() as u64;
    let tail = lines(&[assistant("b", "claude-sonnet-4-6", 200, 20, 30, 40, 15)]);
    data.extend(&tail[..tail.len() - 1]);
    let parsed = super::super::claude::parse_reader(
        std::io::BufReader::with_capacity(511, std::io::Cursor::new(data)),
        Path::new("session.jsonl"),
        123,
    )
    .unwrap();
    assert_eq!(parsed.offset, checkpoint + 123);
    assert_eq!(parsed.entries.len(), 2);
    assert_eq!(parsed.entries.iter().map(Entry::total).sum::<u64>(), 480);
}

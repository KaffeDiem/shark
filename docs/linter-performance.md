# Language-intelligence performance measurements — 2026-10-28

Hardware: Apple M4 Max, Mac16,6, 36 GiB memory, macOS 27.0.1 (26A434).
Final samples were collected after the full regression suite and compiler builds
finished. A separate native QA Shark instance remained idle. These are scoped
measurements, not a packaged-app latency or overall performance certification.

## Editor CPU input-to-frame

`perf_language_input_to_frame` drives real editor key handling and the headless
GPUI frame renderer. It inserts into a Python comment in a 10,001-line, 280,014-byte
buffer, discards ten warmups, and times 100 inputs per run. Three fresh test
processes per state produced these ranges of per-run percentiles:

| Debug build, headless CPU frame | p50 | p95 | Largest sample |
|---|---:|---:|---:|
| Language session disabled | 1.151–1.157 ms | 1.175–1.211 ms | 1.339 ms |
| Language session attached | 1.184–1.207 ms | 1.332–1.447 ms | 1.672 ms |

The attached state uses the existing controllable stdio peer. Document sync runs
between inputs; the timed interval is input handling through the next CPU frame.
Completion popup requests are disabled because the deterministic test scheduler
cannot receive their real OS-thread wakers; actual completion RPCs are measured
separately below. Assertions check that every character reached the dirty buffer
and that disk contents did not change.

This does **not** measure GPU presentation, input-device/display latency, a busy
real indexer contending with Shark, completion menu rendering, or the app header's
progress indicator. A fabricated “indexing” status would not establish those
measurements, so no such number is reported. No release-build threshold is inferred
from these debug timings.

Reproduce each state three times:

```sh
SHARK_LSP_PERF_MODE=disabled cargo test --features test-support perf_language_input_to_frame -- --ignored --nocapture
SHARK_LSP_PERF_MODE=attached cargo test --features test-support perf_language_input_to_frame -- --ignored --nocapture
```

The benchmark is ignored in normal regression runs and prints JSON marked
`LANGUAGE_INPUT_FRAME`.

## Actual Python language-server processes

`scripts/lsp-performance.py` runs the exact cached Pyrefly 1.4.0 and Ruff 0.17.3
artifacts selected by the manifest. It creates a new temporary project on each of
three runs: 500 Python modules, each defining 101 functions, plus one source file
importing all 500 modules; total 501 files and 2,820,526 bytes.

Each run starts both servers, initializes them, opens the source, and requests a
cross-file definition. “Cold” means fresh server processes and project identity;
artifacts are already installed and OS caches are not purged. The number includes
starting/initializing both processes and the first useful definition response.
It is not Shark startup, download time, or proof of project-wide indexing completion.

After a two-second quiet interval, the script performs 30 useful completions and
30 cross-file definitions sequentially over stdio. It checks that completion
contains `answer`, definition points to `mod_0000.py`, source bytes stay unchanged,
and both servers shut down successfully.

| Actual server measurement | Run 1 | Run 2 | Run 3 |
|---|---:|---:|---:|
| Process-cold first useful cross-file definition | 176.198 ms | 107.187 ms | 107.393 ms |
| Warm completion p50 / p95 | 0.227 / 0.323 ms | 0.255 / 0.310 ms | 0.234 / 0.287 ms |
| Warm definition p50 / p95 | 0.051 / 0.058 ms | 0.049 / 0.068 ms | 0.049 / 0.065 ms |
| Combined server process-tree RSS after quiet interval | 99.672 MiB | 99.875 MiB | 99.859 MiB |
| Sampled startup/load tree RSS maximum | 92.594 MiB | 89.297 MiB | 88.625 MiB |

RSS sums each server PID and its descendants from `ps`; it excludes Shark, the
benchmark process, shared-memory accounting corrections, and GPU allocations.
The sampler waits 50 ms between `ps` snapshots (actual interval also includes
sampling overhead), so startup/load maxima are lower bounds and can miss brief
peaks. The later quiet sample being larger illustrates why they must not be read
as measured peak indexing memory. No progress token remained active after warm
requests, but a quiet interval alone does not prove all background work completed.

Preliminary runs while other QA activity was still running ranged from 0.711 to
6.581 seconds to first definition. Those samples are excluded from the idle-host
table and demonstrate the sensitivity to process startup delays and contention.

Reproduce after the artifact checker has populated its verified cache:

```sh
python3 scripts/lsp-performance.py
python3 scripts/test-lsp-performance.py
```

The offline tests verify bounded requests even under continuous notifications and
failure propagation/peer cleanup when memory sampling fails.

This script neither installs tools nor changes project configuration outside its
temporary fixture. It prints one JSON record per run and always shuts down or
kills/reaps its own server processes on failure. Other languages and packaged-app
whole-process-tree comparisons remain separate checks.

## Native QA app idle memory snapshot

A separate isolated debug `.app` was observed idle after its two small Python
files reached Ready with real Pyrefly and Ruff. A single `ps` snapshot recorded:

| Process | RSS |
|---|---:|
| Shark Linter QA app | 146,912 KiB |
| Pyrefly child | 25,872 KiB |
| Ruff child | 20,384 KiB |
| **Process-tree total** | **193,168 KiB = 188.64 MiB** |

Evidence: `idle-memory.json` in the native QA run directory identified by
`/tmp/shark-linter-native-location`. The snapshot contains the three PIDs,
parent-child relationships, RSS values, and executable paths. This is a single
idle observation for a different, much smaller workload than the 501-file server
benchmark. It is **not** a disabled/idle/indexing comparison, a peak, a leak test,
or a release-build memory budget. RSS also does not deduplicate shared pages or
include GPU allocation accounting.

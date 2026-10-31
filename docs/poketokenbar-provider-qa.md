# Provider, accounting and native Usage QA

Review date: 2026-10-31. Upstream reference: PokeTokenBar `32ebe4cac2aca1d298d71e7cbc70a507015470f9`. Shipping providers are Claude Code and Codex. Synthetic providers in tests exercise the extension boundary; they do not add other integrations to the app.

## Preserved account fetching

The final `git diff -- src/usage.rs` contains 14 added lines: declarations for `logs`, `providers`, `pace`, and `summary`, plus `Provider::source()`. The existing quota query, CLI process protocols, authentication handling, response parsers, and `Allowance::pace` implementation are unchanged.

The original cache-excluding activity heatmap retains its token definition. Its arithmetic now saturates instead of overflowing on corrupt counts. Separate `Observation` reports carry inclusive input/output/cache-write/cache-read accounting for companion growth and optional cost metadata. Token spending never estimates an official subscription percentage.

`Registry` registration drives provider slots, polling, account-detail tabs, charts and companion observations. Stable IDs own persistence and UI identities. Registration order assigns chart colors; usage rank only orders the stack. The future-provider regression includes a third tool with the same display name as an existing tool, so labels cannot substitute for IDs.

## Confirmed defects corrected

- **Stale completions:** quota requests started before an import, toggle or configuration change could award rewards to the new companion state. Activity results could also replace a report before checking their generation; another provider's refresh then persisted that stale report into the recap ledger. Both completion boundaries now reject obsolete observations while releasing their request slots.
- **Request lifecycle:** rapid navigation could launch hundreds of sprite/detail subprocesses. Thumbnail work is capped at 16 and profile work at 4, with current-page refill and only the latest deferred profile. Starting a fresh egg now clears every invalidated pending set, preventing old callbacks from permanently occupying those slots. Disabled companions do not start asset requests.
- **Cache and subprocess handling:** metadata caches validate the requested resource identity and required data, reject future fetch timestamps, and retain valid stale data when offline. The sprite pipeline validates decoded images rather than accepting a GIF header alone. Pokémon HTTP output is bounded independently of curl's version, and oversize producers are killed and reaped. The API checkpoint includes an infinite `/usr/bin/yes` producer regression.
- **Native drawing:** real Metal inspection found two-digit axis labels wrapping vertically inside narrow day columns. Labels now keep their natural horizontal width and explicit line heights matching the axis rows. Pace markers are clamped inside the track and extend two points above and below its painted four-point height.
- **Invalid quota signals:** out-of-range future-provider percentages no longer get clamped into exhausted-window candy rewards. Companion quota readouts omit unused/yesterday-only providers and monetary/model-specific caps; the critical mood uses fresh official headline windows at the fixed 95% threshold.

Custom-root edits persist their configuration and reset only that provider's growth checkpoint. Added historical logs can populate accounting history but cannot mint historical companion currency. Same-day token high-water marks remain an intentional difference from upstream: missing/restored logs cannot credit the same tokens again.

## Native views exercised

Usage contains a full-calendar month chart, blank future columns, explicit past zero days, regional weekend ticks, stable provider colors, a wrapping legend and an immediate per-day tooltip. One used provider follows the unstacked drawing path. A zero-today report keeps positive monthly history visible; an empty history draws no chart.

The Recap button opens Week/Month/Year periods with bounded navigation, coverage-aware segmented meters, elapsed-period comparisons, streak/day statistics and graduate appearances. Missing/future data is distinct from observed zero. Unown letters and other forms survive the graduate strip, and its active subject is independent of the pinned representative. Disabling Pokémon hides its artwork while keeping usage history available.

Account-detail tabs scroll horizontally without wrapping and filter sections without refetching or changing combined totals. Tests include 14 synthetic providers and an exceptionally long display name. Period cost rows preserve complete known/unavailable values on one line. Providers declaring `reports_cost == false` show no monetary values in summaries, charts or detail cost rows.

## Executed validation

All commands run from the Shark worktree, selecting its pinned Rust toolchain. Live account/log tests were not enabled.

| Check | Result |
| --- | --- |
| API/cache checkpoint | 7 passed |
| Asset lifecycle checkpoint | 9 passed |
| Existing quota protocol/activity checkpoint | 20 passed, 2 live checks ignored |
| Final `cargo test --features test-support app::usage:: -- --test-threads=1` | **71 passed**, 0 failed |
| Native Usage Metal harness | **32 fixtures passed** in dark/light modes |

The final native log is `/tmp/shark-usage-native-final.log`. The Metal log is `/tmp/shark-usage-metal-final.log`; images are under `/tmp/shark-poketokenbar-usage-renders` and remain outside the repository/release bundle.

The Metal harness compares actual cropped chart RGBA bytes for single-active versus single-active-plus-idle providers. They match exactly in both appearances. It also checks painted marker positions and height against the painted track, including no-marker and near/both-end cases. The near-zero fixture uses a reset one second in the future: an already expired reset correctly has no pace marker. Exact zero geometry is separately checked in Rust.

Representative inspected artifacts:

- `usage-stacked-component-dark.png`: horizontal two-digit labels, provider stack and legend.
- `usage-Month-component-dark.png`: coverage meters, month ticks and statistics.
- `usage-quiet-known-component-light.png`: zero today, intact `$31.64`/`$121.30` period costs.
- `usage-pace75-component-dark.png`: actual pace ink beyond the fill, extending two points around the track.

Reproduce the Metal check with:

```sh
SHARK_RENDER_USAGE_ONLY=1 SHARK_RENDER_DIR=/tmp/shark-poketokenbar-usage-renders \
  cargo test --features test-support --test rendering
```

## Source-case adaptation notes

The central [migration inventory](poketokenbar-test-migration.md) owns overall completion and host exclusions. These notes prevent native regressions from being mistaken for identical host architecture:

| Source cases | Rust interpretation |
| --- | --- |
| Remaining 17 `MonthDailyTrendTests` and 12 `DailyTrendStackTests` | Runtime chart metrics, native GPUI interactions/layout, localized date formatting and real Metal crop comparisons. The worst-case caption UI retains Shark's English interface; date-format helpers cover all eight upstream languages. |
| Remaining four `UsageRecapTests` rendering cases | Native card in all three scopes; shared labels/accessible descriptions cover all eight languages. No new interface-language preference is added. |
| Five `ProviderTabLayoutTests` | Actual Claude/Codex registry plus synthetic generic tools for the overflow trigger; other upstream providers are not shipped. |
| Four `ZeroUsageHomeRenderingTests` | Historical accounting lives in Shark's Usage view, rather than duplicating it in Pokémon Home. Known/unavailable native cost rows use the host language; German wording is checked through the shared formatter. |
| `UsageStoreTests::testConcurrentRefreshIsCoalescedNotDropped` | Retains Shark's per-provider one-in-flight sharing, with a fresh retry after generation/configuration invalidation. It does **not** add upstream's unconditional same-generation queued extra fetch. |
| `testLimitDisplayPercentModes`, `testMenuBarLimitRemainingModeInvertsPercents`, three `LimitProgressRenderingTests` | Retains Shark's remaining-only display. Official fractions, fill, own-window pace and painted marker geometry are tested; no used/remaining preference is introduced. |
| `StatusItemMenuTests::testSummaryOmitsCostWhenNoProviderReportsOne` | Applied to the native accounting summary/chart/detail views; Shark's application menu architecture is retained. |
| `PerformanceTests::testLargeDailyReportDecode` | The optional generic summary decoder executes 20 passes over the 1,000-row fixture and checks count, tokens and cost preservation. |
| Two `ScreenshotGenTests` | The real native Usage harness generates and validates the 32 dark/light chart, recap, quiet-day and pace fixtures. |

Upstream service-status polling, OS quota alert notifications, configurable warning thresholds, derived quota forecasts, and attributed menu-title color preferences are host decisions recorded by the central inventory. Candy tests are not presented as substitutes for notification tests.

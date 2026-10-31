# Pokémon token accounting

Shark retains its existing Claude/Codex remaining-allowance queries. Local transcript scanning never supplies an official quota value.

The navigation heatmap retains its existing definition: Claude input, output, and cache creation; Codex uncached input and output. Pokémon growth uses inclusive tokens from the same providers' local logs: disjoint input, output, cache creation, and cache reads. Codex's input already includes cached input, so the parser subtracts cached input before adding the cache-read bucket. Reasoning output is included in output and is not added again.

Claude streamed records are deduplicated globally by message/request identity, keeping the largest usage and earliest timestamp. Source `cost-state` ledgers take precedence over estimates, with each model's final session cost distributed proportionally across its positive-token requests. Appends can update the ledger for the whole session. Incomplete trailing lines are retried; truncation discards the former ledger.

Codex active and archived rollouts share canonical session/state identities. Forked replay is matched against the parent's resolved history; old parent files remain available as dependencies even outside the display window. Genuine subagent turns are retained when a parent is missing. Cumulative resets create a new identity epoch. Total-only usage is counted when recoverable but marked unpriced because the token breakdown is unknown.

Costs are API-equivalent estimates from the exact table in upstream revision `32ebe4cac2aca1d298d71e7cbc70a507015470f9`, unless the tool reported a cost. They are not subscription invoices. Unknown models never borrow another model's price. Coverage distinguishes reported, estimated, and unavailable amounts, including partial totals and explicit zero.

The default parser cache is memory-only. An explicitly selected cache path supports bounded compressed snapshots, parser/index version invalidation, a 60-second write throttle, and 40-day disk-blob retention. Metadata indexes persist independently to avoid repeatedly probing missing parents. Tests use temporary directories and the checked-in sanitized upstream fork/subagent fixtures; they do not read user transcripts or credentials.

## Generic provider API

`usage::providers::UsageProvider` supplies stable `id`/`name`, official `remaining_usage`, the existing heatmap `activity`, and `observations(now) -> Observation { activity, companion: logs::Report }`. The default observation adapts `Daily` into a report; built-ins override it with inclusive parsing. `reports_cost()` defaults to true and lets a future flat-rate source suppress monetary totals without suppressing tokens.

A `Report` carries raw normalized `Entry` records, `Daily` counters, a `Bucket` total, and an optional scan error. Entries contain disjoint unsigned input/output/cache-write/cache-read counts, a UTC timestamp, model ID, optional reported cost/provenance flags, and session provenance. A failed report preserves the last successful report in Shark and does not update growth or the historical ledger. A successful empty scan clears the current provider summary. Official allowance fetching remains independent.

`usage::summary` derives current-day/week/month buckets and a day-aligned current-month series. Unknown/default reports produce no observed days; valid zero observations establish coverage. Daily-only future providers use their represented day range rather than inventing earlier coverage. `UsageLedger` records high-water daily totals and coverage, retaining the current and previous calendar years. `Recap` builds week/month/year meters, elapsed-period comparisons, streaks, and graduation history without treating unrecorded or future days as zeros.

For tools that already emit ccusage-compatible JSON, `summary::external::ExternalReport::parse(bytes)` accepts daily, weekly, monthly, and block records with documented legacy aliases (`date`/`period`, `cachedInputTokens`, `costUSD`). `into_report(source_id, now)` adapts dated rows into the same provider report. Shark's built-ins do not invoke ccusage. Detailed external summaries expose block dates and burn rates; forecast helpers never replace official quota values.

The native usage panel consumes `Summary::from_sources` for combined totals. Summary order follows registration, identity uses provider IDs, and callers can select a preferred source with a stable first-source fallback. Parser/model tests, temporary-file cache tests, fixed-calendar/DST tests, and native GPUI observation tests cover this boundary.

### External report shape

The optional decoder accepts this minimal daily shape:

```json
{"daily":[{"date":"2026-10-31","inputTokens":100,"outputTokens":20,"cacheReadTokens":50,"cacheCreationTokens":0,"totalTokens":170,"totalCost":0.001}]}
```

`weekly` and `monthly` arrays use the same counters with `week`/`month` (or `period`) identifiers. `blocks` additionally accepts `id`, ISO-8601 `startTime`/`endTime`, `isActive`, and `tokensPerMinute` (also accepted inside `burnRate`). Missing arrays are empty. Invalid JSON or invalid bucket types return a decoding error; absent dates cannot produce daily observations.

`into_report` converts only daily rows, at local noon on each date. It keeps weekly/monthly/block values available in the decoded model without adding overlapping aggregates to daily tokens. If `totalTokens` disagrees with the component sum, the authoritative total is preserved as a total-only row with unavailable breakdown pricing. Daily-only provider adapters likewise carry aggregate tokens with unknown cost coverage; the UI does not present those aggregates as a measured input/output breakdown.

### Append-cache boundary

Claude incremental reads verify file identity and bounded hashes of the previous file's first and last 4 KiB before reusing an offset. Rotations and typical truncate/rewrite replacements restart parsing; no transcript text is persisted in these guards. CLI logs are assumed append-only between those guards: arbitrary same-width edits confined to the middle followed by an append require rebuilding the usage cache. Codex files are streamed and reparsed when their stamp changes. Inaccessible Codex parents may contribute retained replay metadata, but their unavailable entries are not added to the current report.

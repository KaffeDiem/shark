# PokeTokenBar port QA

Final validation passed. The source inventory has no pending cases; this does not count excluded host/provider tests as Rust translations.

## Independent Sol review

Three Sol reviewers covered the companion model and persistence, provider/accounting boundaries, and native UI/settings/rendering. They exercised production Rust modules, deterministic fixtures, isolated temporary state, the GPUI interaction harness and the real Metal renderer. Live account tests stay opt-in; no user credentials or transcripts were used for this QA.

Findings fixed include:

- Released Pokémon paths inflated recovered levels; legacy difficulty migration changed progress fractions; same-stage growth failed to refresh learned moves.
- Missing or stale quota reset timestamps could rearm rewards. Invalid quota percentages could become candy signals. A stale scan could enter the combined history after its generation was invalidated.
- Replacing a Claude log with a larger file reused an invalid append offset. An unavailable Codex parent could remain in the metadata index and cause a panic. Both have temporary-file regressions.
- Legacy lowercase nature names could let a Mint choose the same nature. Missing profiles with equal catch timestamps could regenerate the same individual identity. Recovery now preserves valid existing identities and assigns unique deterministic identities to recovered individuals.
- Cancellation during image decoding could publish decoded frames. Sprite fallback identities, concurrent cache publication, decoded-memory limits and retry/cancellation paths have regressions.
- The persistent animation could retain an old power/motion preset or lose its frame cap after asset-cache eviction. Render-time memory-only reconciliation and one retained displayed asset now preserve those settings; cold thumbnail selections decode in background workers.
- Native focus traversal skipped controls, large individual selectors overflowed, modal confirmation could sit behind the panel, and GIF elements without stable IDs reset their animation.
- Strict GIF end-code validation rejected official Pikachu, Bulbasaur and Charmander animations. The validator now accepts the same complete pixel streams as standard decoders while retaining consistency and allocation bounds; a synthetic one-pixel encoding reproduces the case without bundling Pokémon artwork.
- Rendered chart date labels wrapped, and small axis rows inherited excessive line height. Real Metal assertions cover the final positions and painted quota markers.

## Source migration boundary

The [complete inventory](poketokenbar-test-migration.md) accounts for all 1,585 upstream XCTest methods at pinned revision `32ebe4cac2aca1d298d71e7cbc70a507015470f9`. There are 925 translated/adapted cases and no pending cases. Exact Rust locations and adaptation reasons are in the CSV.

The other 660 cases are explicitly excluded from translation: 304 belong to providers outside Claude Code/Codex; 237 belong to upstream quota/authentication behavior replaced by the user's requested existing Shark fetchers; 119 belong to the standalone host, including its menu-bar/floating-pet lifecycle, updater, language settings, status polling and OS notifications. These are not silently counted as passing Rust tests.

Shark's remaining-usage query implementations are preserved. The changes in `src/usage.rs` add module declarations and the provider adapter accessor, without replacing the CLI query bodies. Companion inclusive token accounting is separate from official quota values and Shark's existing heatmap definition.

## Final verification

- `cargo test --features test-support -- --test-threads=1`: **1,482 passed, 0 failed, 25 ignored**, in 281.57 seconds.
- Seven opt-in tests then passed separately: all **424 public sprite URLs downloaded and decoded** through the production validator (10.31 seconds); the 512 MiB Codex benchmark; and five explicit-corpus audits. This brings executed Rust tests to **1,489**, with **18 pre-existing live-account/inference/private-data tests left disabled**.
- The 512 MiB Codex transcript parsed in **1.47 seconds**, using **24 MiB peak RSS** and **7 MiB additional peak memory**. Both built-in readers stream bounded records.
- **80 Metal rendering cases passed:** 48 Pokémon views/appearance cases and 32 Usage chart/recap/pace cases. The checks include actual animated pixels, normal/shiny transitions, immediate representative pinning, long-name layout, exact single/idle-provider chart equality and painted quota-marker geometry in light/dark themes.
- `cargo check` and `git diff --check` passed. The existing dependency future-compatibility warning and non-fatal dead-code warnings remain.

The five corpus audits used checked-in sanitized fork fixtures plus a synthetic Claude log, copied to an explicit temporary corpus. No user account credentials or private transcripts were queried. Public sprite checks fetched identifiers from the PokéAPI sprite repository and stored no artwork in the release bundle.

The initial concurrent broad run was interrupted when host responsiveness deteriorated; it is not counted as a passing run. The final whole-project result above is the completed serialized run on the frozen source.

Detailed evidence: [native Pokémon UI QA](poketokenbar-ui-qa.md), [provider/accounting QA](poketokenbar-provider-qa.md), and [token accounting contracts](pokemon-token-accounting.md). The accounting document records the bounded append-cache assumption for CLI logs.

Temporary artifacts and logs:

- `/tmp/shark-pokemon-renders` — 48 Pokémon images.
- `/tmp/shark-poketokenbar-usage-renders` — Usage screenshots and component crops.
- `/tmp/shark-final-all.log`, `/tmp/shark-final-public-assets.log`, `/tmp/shark-final-large-log.log`, `/tmp/shark-final-parity.log`, `/tmp/shark-final-check.log`.

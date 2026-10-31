# Native Pokémon UI QA

Independent Sol review exercised production GPUI widgets and the actual Metal renderer. All state, snapshots and generated fixtures used an isolated temporary home. The review did not query accounts or alter user data.

## Findings fixed

| Finding | Fix and regression evidence |
| --- | --- |
| The GIF validator rejected official Pikachu, Bulbasaur and Charmander animations. | The optional strict LZW end-code check rejected complete pixel streams. The validator now accepts that encoding while retaining frame consistency, dimension, file-size, frame-count and decoded-memory limits. A synthetic fixture verifies the strict failure and production decoding; all four real runtime fixtures pass the renderer. |
| Footer timing could retain the old power/motion setting or lose its frame cap after cache eviction. | Render-time reconciliation uses memory-only lookups and a cached key. One displayed Asset Arc survives global LRU eviction; returning to a cold thumbnail queues background decoding and paints a static placeholder. Native tests apply quality, Low Power and Reduce Motion changes through actual renders after 300 cache insertions. Disabling cancels animation, retry and power-observation tasks. |
| Tab alternated between the panel root and Refresh. | Native focus trapping reaches every companion tab in both directions, wraps, closes with Escape and restores prior focus. Navigation buttons expose their current state to accessibility. |
| Usage removed companion navigation; Settings and detail reentry retained stale navigation state. | Companion-origin Usage preserves its tabs. Native tests cover Usage → Home → Pokédex, standalone Usage, Settings entry, detail retapping, dismissal and disabled controls. |
| The popover covered fresh-egg confirmation dialogs. | Its deferred drawing priority is below dialogs. A precious-companion test verifies Return cancels and only the explicit confirmation replaces progress. Difficulty drafts are discarded on close and saved only by the displayed Save control. |
| Individuals overflowed the panel and mixed unrelated forms. | Individual controls wrap and are scoped to the displayed form. Form and color controls select the matching individual; all 106 catalog forms across 23 species are selected and pinned through native controls, with persisted form/color identity checked after reload. |
| Long grid names clipped both their beginning and ending. | Labels now fit their cell and use trailing ellipses; full names remain accessible and available as tooltips. Dense Metal fixtures exercise 19 species, long names, four rarities and both pages. |
| Home artwork could not open details and could be confused with a pinned representative. | Home opens the growing individual independently of navigation artwork. Native tests verify this with different active and pinned species. Actual pixels verify immediate footer changes after pinning a selected color. |
| Form metadata notes could describe the original selection instead of the displayed form. | Mounted tests switch alternate/default forms, remove metadata and return Home; notes appear only for loaded alternate-form metadata. |
| LICENSE links lacked native keyboard/accessibility semantics. | Native Link-role controls retain the original URLs. Scrolling reaches the final PokéAPI attribution and complete MIT notice. The separately identified General card width problem was fixed by the parent implementation and verified in native bounds tests and fresh screenshots. |

## Executed checks

The final targeted runs passed:

| Command filter | Result |
| --- | --- |
| `pokemon::api::` | 8 passed |
| `app::pokemon::` | 73 passed |
| `app::ui_tests::pokemon` | 33 passed: 11 migration interactions, 8 independent QA cases, 9 settings cases and 5 integration cases |
| Pokémon Metal rendering harness | Exit 0; 48 fresh PNG artifacts and strict animation/color pixel assertions |

The final label-only layout change was subsequently verified by the Metal harness, including native label bounds. Repository-wide results are recorded separately in [the overall QA report](poketokenbar-qa.md).

Native coverage includes 640×480 and 800×600 viewports, light/dark themes, rail/expanded navigation, scroll access to final controls, a 31-individual picker, all 56 Unown letter/color identities, stale callbacks, backoff cancellation, lazy layout of a 300-entry catch log, import Return-cancellation, backup-before-replacement, preserved device preferences, snapshot history and animation-quality persistence.

## Metal evidence

The 28 page captures cover footer, Home, Pokédex, Shop, Usage, General and LICENSE in both themes and navigation layouts. Both footer and Pokédex must visibly advance validated runtime GIF frames while active. The headless clock is advanced and its ready tasks are explicitly pumped; transient native images also receive real elapsed time.

The additional 20 appearance captures cover light/dark themes and both normal/shiny acquisition orders. Generated red/blue GIFs make exact pixels unambiguous: the grid shows shiny aggregate artwork, detail controls switch normal → shiny → normal without stale pixels, pinning immediately changes footer pixels, and catch-log rows retain each individual's color on reopening. Dense grid captures also exercise long names and next/back pagination.

Usage fixtures distinguish Shark's existing cache-excluding heatmap (150 tokens per provider) from inclusive companion accounting (350 per provider), with explicit estimated cost. These fixture values do not replace live quota queries.

```sh
SHARK_RENDER_POKEMON_ONLY=1 \
SHARK_POKEMON_PREVIEW_SPRITES=/tmp/shark-pokemon-preview-sprites \
SHARK_RENDER_DIR=/tmp/shark-pokemon-renders \
cargo test --features test-support --test rendering
```

Artifacts are under `/tmp/shark-pokemon-renders`. Official preview GIFs and synthetic color fixtures are runtime-only; no Pokémon artwork was bundled. Fresh images inspected directly include Home, General, dense grid, shiny Details, catch log, LICENSE and rail Usage.

## Explicit adaptations and limits

GPUI window-owned overlays replace global NSEvent monitoring. Cursor regressions verify native button roles, hover/removal and absence of stale hitboxes; the headless API does not expose the system NSCursor image. Single-frame GIFs remain static decoded GIFs instead of triggering a redundant PNG download. Shark's English interface, navigation footer and native layout replace the standalone menu-bar/floating-pet and eight-language whole-application presentation. Readability evidence uses native observed labels and actual Metal captures instead of Vision OCR. These distinctions are recorded in the [case inventory](poketokenbar-test-migration.csv).

A source marker establishes traceability, not automatic assertion equivalence or a passing test. The form sweep verifies selection and persistence; it does not download every form. The parent-run opt-in catalog audit separately fetches and production-decodes public assets. No native macOS energy measurement or system cursor screenshot is claimed.

## Collected Pokémon in chat activity

The chat activity renderer now assigns caught individuals (including the hatched active companion) to background agents. Individual IDs survive task/collection reordering, and current species, shiny appearances and forms follow the saved collection. The old animal catalog, color assignment and `critters` module were removed. Shell tasks retain their terminal icon; empty/disabled collections and unavailable portraits use a neutral task icon.

Four pure Rust cases cover roster membership, stable individual assignment, overflow sharing, shells, removal and appearance changes. Three native integration cases cover all seven distinct portraits beyond the six-slot row, disable/re-enable/import propagation to an existing chat, and asynchronous loading without losing task state. The existing lifecycle regression still checks fixed geometry, history replay, task completion and reduced-motion timer shutdown.

Ten Metal captures use production-validated public Pokémon sprites: dark/light × first frame, next hop, hover, reduced motion, and a second reduced-motion frame. Assertions require hover details to appear, hopping pixels to change and reduced-motion pixels to remain equal after the hover card closes. Three Sol agents independently reviewed model coverage, app/cache integration and UI behavior; overflow portrait loading and repeated roster allocation were fixed from that review. Final visual inspection found no clipping or alignment issues in the captured states.

```sh
cargo test --features test-support --bin shark activity_tests -- --test-threads=1
cargo test --features test-support --bin shark background_hops_stop -- --test-threads=1
SHARK_RENDER_ACTIVITY_ONLY=1 \
SHARK_POKEMON_PREVIEW_SPRITES=/tmp/shark-pokemon-preview-sprites \
SHARK_RENDER_DIR=/tmp/shark-chat-pokemon-renders \
cargo test --features test-support --test rendering
```

Final follow-up regression run: `cargo test --features test-support -- --test-threads=1` — **1,487 passed, 0 failed, 25 existing opt-in tests ignored** (280.12 seconds). New and shared-renderer files pass rustfmt checks; `git diff --check` is clean.

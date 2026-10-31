# Pokémon in Shark

The navigation footer raises a Pokémon using Claude Code and Codex token activity. Click it to open Home, Pokédex, Bag, Shop, or the existing Usage view. Egg incubation, evolution, rarity, shiny appearances, natures, individual profiles, forms, and graduation are implemented in Rust.

Settings → General also controls animation quality and lets you choose the navigation Pokémon. Navigation animation pauses when Shark is inactive or Reduce Motion is enabled, and adapts to Low Power Mode.

Settings → General → Pokémon companion hides the companion and pauses growth. The collection survives this change. Re-enabling starts from a new usage checkpoint, so tokens consumed while disabled do not retroactively grow a Pokémon. Settings → LICENSE contains Shark's license, PokeTokenBar's complete MIT notice, and Pokémon attribution.

## Usage providers

`src/usage/providers.rs` defines `UsageProvider` and a registry with stable string IDs. To integrate another coding tool:

1. Implement `id`, `name`, `remaining_usage`, and `activity`. Keep blocking I/O in these worker methods and own the incremental cache inside the adapter.
2. Register the adapter with a unique stable ID. Return `Failure::Unsupported` if the tool cannot report official quotas.
The app derives polling, daily activity, Usage rows, and companion observations from the registry automatically. Registration order controls display order; stable IDs own persisted checkpoints, so adding a tool requires no built-in enum variant or UI changes. `observations` defaults to the provider's daily activity; override it to supply separate inclusive companion accounting and optional cost metadata. Override `candy_windows` if the tool's official headline quotas use different windows or labels.

The built-in adapters call Shark's existing account query and local-log readers. No PokeTokenBar OAuth, Keychain, or session-key fetching replaces those protocols. Queries run in the background and cannot start a model turn. Quota percentages are supplied by the CLI, not estimated from token totals. Token growth and quota rewards have independent persisted checkpoints.

The existing Usage heatmap keeps Shark's original token definition. Companion accounting includes input, output, cache writes, and cache reads; Usage shows this breakdown separately. Cost metadata distinguishes reported values, estimates from the pinned upstream pricing table, partial coverage, and unavailable prices. These costs never determine subscription allowances.

Settings → Token sources accepts additional local scan folders for each supported provider. Changing folders invalidates pending observations and establishes a new checkpoint for that provider, so adding old logs cannot grant historical currency. This preference does not replace the CLI's account query.

The first observation establishes a baseline. Repeated observations and restarts cannot credit the same usage twice. Same-day lower totals preserve the high-water mark, so temporarily missing logs cannot award tokens again when restored. Switching Pokémon off and on invalidates pending observations and establishes a fresh baseline. Candy rewards apply to the headline five-hour and weekly windows; model-specific and monetary caps do not earn duplicate rewards.

## Data and recovery

Companion state lives in Shark's existing atomic `state.json` save. Pokémon data comes from PokéAPI and sprites from the PokeAPI sprite repository, fetched on background workers and cached in Shark's application-support directory. The binary contains no Pokémon artwork. Requests contain public species or resource identifiers only, never usage logs, prompts, credentials, or workspace paths. Sprite and metadata caches are validated and bounded. Both Claude and Codex transcript readers stream records instead of allocating an entire transcript.

Settings → General offers manual snapshots, snapshot history, and import/export. Automatic snapshots are spaced at least 12 hours apart and retain the ten newest valid saves. Imports require a backup before replacing progress. Return cancels the confirmation. Imports preserve this Mac's appearance, Pokémon visibility, and difficulty settings; usage checkpoints and quota reward history are rebased to avoid replaying earned rewards.

Shark exports its own `shark.pokemon.save` envelope, schema 1. This is a Rust save format, not a claim of compatibility with Swift PokeTokenBar save files. Decoding rejects foreign/future schemas and files over 8 MiB. Numeric values and Pokémon paths are sanitized on import and ordinary state load. If Shark's main state file is corrupt, its existing preservation mechanism keeps the unreadable file and the Pokémon collection is recovered from the newest valid snapshot when available.

## Host integration

The embedded companion uses Shark’s English interface, application lifecycle and official CLI quota queries. It does not install PokeTokenBar’s standalone macOS menu-bar item, floating desktop pet, self-updater, authentication store, status-page poller or OS quota notifications. The migration inventory identifies those retained host boundaries explicitly.

## Upstream and validation

Adapted from [PokeTokenBar](https://github.com/chattymin/PokeTokenBar/tree/32ebe4cac2aca1d298d71e7cbc70a507015470f9), revision `32ebe4cac2aca1d298d71e7cbc70a507015470f9`, under the [MIT license](../licenses/PokeTokenBar-MIT.txt). Shark remains GPL-3.0.

The [test migration inventory](poketokenbar-test-migration.md) tracks all upstream test methods, including unfinished translations and the user's provider-scope exclusions. It is the authoritative record of migration completeness. The implementation uses Rust tests; no Swift runtime or test runner is required.

Run checks from this repository directory so `rust-toolchain.toml` selects the correct compiler:

```sh
cargo check
cargo test --features test-support -- --test-threads=1
SHARK_RENDER_DIR=/tmp/shark-pokemon-renders cargo test --features test-support --test rendering
```

Background agents in chat borrow Pokémon from your collection, including your currently hatched companion. Each agent keeps its individual Pokémon as other tasks finish or reorder; shiny and form appearances are preserved. The row shows up to six portraits, with all tasks and their Pokémon in the hover details. Shell tasks retain their terminal icon. Disabling Pokémon, an unhatched first egg, or an unavailable image leaves a neutral task indicator. Reduced motion keeps the portraits still.

# PokeTokenBar test migration

Upstream: https://github.com/chattymin/PokeTokenBar/tree/32ebe4cac2aca1d298d71e7cbc70a507015470f9

The [case inventory](poketokenbar-test-migration.csv) records **every one of the 1,585 upstream XCTest methods**, with its original source location and a Rust test location where translated. Every source case has an explicit disposition. Host/provider exclusions are not claimed as translated tests; the executed QA results are recorded separately.

| Status | Cases |
| --- | ---: |
| translated | 858 |
| adapted | 67 |
| out-of-scope | 304 |
| retained-shark-fetcher | 237 |
| retained-shark-host | 119 |
| pending | 0 |

`adapted` identifies an intentional Rust contract difference, with the exact rationale in the case inventory. `translated` means the case has a Rust regression carrying an upstream marker. It does not itself establish that the test passes or that every assertion has equivalent coverage. Run the Rust suite and review each translation. `pending` cases are unfinished work, not skipped or passing tests.

The user narrowed coding-tool support to Claude Code and Codex. Tests exclusive to other providers are `out-of-scope`. The user also explicitly retained Shark's existing remaining-usage fetching because it works more reliably. Tests specific to the upstream quota/authentication architecture are `retained-shark-fetcher`; the corresponding Shark implementation and existing Rust regressions remain authoritative. `retained-shark-host` records standalone updater, process lifecycle, menu-bar and language-interface tests whose host is Shark. These are explicitly not claimed as Rust translations. Shared parsing, companion, persistence and embedded UI tests remain pending until individually translated; they are not silently excluded.

Regenerate with:

```sh
python3 scripts/poketokenbar-test-inventory.py /path/to/PokeTokenBar
```

Rust tests use fixtures and deterministic seeds and do not access the user's accounts. Production uses Shark's provider adapters and runtime PokéAPI resources.

# Git artifact

Git follows the Diff artifact lifecycle: it starts closed, opens once per workspace,
participates in the saved tab order, and supports closing, reordering, shortcuts,
pane navigation, the command palette, and the View menu. `cmd-shift-g` opens it.
Arrow keys, Page Up/Down and Home/End scroll the conversation; Vim mode also supports
j/k, Ctrl-D/U, gg and G outside text inputs. Saving from Home never posts a hidden
Git draft.
The existing workspace Git controls still own push, PR creation, readiness, merge,
and archive.

## Review workflows

- Pick a repository, read its PR description, conversation, reviews and inline
  threads. The explicit PR URL of an isolated review checkout takes precedence
  over branch discovery, including fork PRs.
- Edit the title and Markdown description. Saving re-reads the original title and
  body; an intervening remote edit leaves the local draft intact.
- Post a conversation comment or a reply to a particular review thread, and
  resolve or reopen a thread. Controls reflect GitHub permissions, which are
  checked again immediately before a mutation. Thread paths open the matching
  current Diff file; GitHub links retain the original context for missing or
  outdated code.
- Refresh after a remote change or a failed request. Failed reads retain the last
  successful data. Network mutations are never retried automatically: a lost
  response can mean the write succeeded.

See [Right-pane features](right-pane-features.md) for shared host routing and the
feature-author pattern.

## Data and lifecycle

`src/pr/git_feature.rs` owns the GitHub boundary. It uses the existing authenticated
`gh` CLI and bounded subprocess runner, passes text as literal arguments, validates
PR identity, and follows pagination for conversations, reviews, threads and nested
replies. GraphQL error envelopes are failures even when HTTP succeeds.

`src/features/git/mod.rs` owns a cached view per workspace. Drafts are keyed by the
exact PR URL and thread ID, stored under the existing application support directory
in `git-drafts/<workspace>.json`, and written with the existing private atomic file
writer. Description drafts retain their original remote baseline across restarts.
Read errors protect the original draft file. Storage failures participate in the
app's unsaved-data banner and quit/archive guards. One background save worker per
workspace serializes snapshots, including discards; typing never waits on its disk
I/O. Changes made during a write become its next snapshot. Retry uses the same
worker, and quit/archive wait for pending local saves as well as remote writes.

Reads and GitHub writes run outside the UI thread. Conversation rows use the house
variable-height virtual list; Markdown is created when a row is rendered and
capped at 128 cached entries. Long bodies use a UTF-8-safe 4 KB formatted preview;
Read full text opens the complete Markdown source in the house read-only editor,
with wrapping, search and copy. This avoids an expensive rich-text layout of very
large single paragraphs. The complete source remains in descriptions, comments
and editable drafts. Rapid repository switches coalesce into one pending read. Navigation cannot redirect an in-flight write to a different repository. Closing a tab keeps
its drafts. Refresh keeps the list position within the same PR. Quitting or
archiving waits for a pending remote write.

Local drafts opens saved descriptions, comments and replies even after an offline
restart, permission loss, or removal of the original review thread. Drafts can be
read, copied, edited or discarded without GitHub access. Posting requires the
matching PR and an available, permitted destination; the API rechecks identity
and permissions before writing. GitHub URL capitalization differences are accepted
without changing existing draft keys.

## API limitations

GitHub's `updatePullRequest` mutation has no compare-and-swap argument. A fresh read
prevents overwriting an already stale description. After refreshing, the explicit
“Replace with my draft…” action confirms overwriting the displayed remote version
while preserving the local draft. A concurrent edit between that read and the mutation remains possible. After an unconfirmed post, refresh
and inspect GitHub before manually retrying to avoid duplicates.

The API contract comes from GitHub's official [pull request GraphQL reference](https://docs.github.com/en/graphql/reference/pulls)
and the [GitHub CLI API manual](https://cli.github.com/manual/gh_api).

## Validation

Run the focused workflow tests with:

```sh
cargo test --features test-support git_feature
cargo test --features test-support
cargo test --features test-support --test rendering
cargo fmt --check
```

The tests use a local `SHARK_GH` executable to exercise the real subprocess boundary
without publishing test comments. The fixture covers description edits, conflicts,
comments, replies, resolution, permissions, pagination, identity drift, failure
retention, tab persistence, draft isolation, and large conversations. Live API
validation uses read-only queries; no live PR is modified by these tests.

Local measurements (development build, this machine; not universal latency guarantees):

| Scenario | Observed result |
| --- | --- |
| Headless frame, 10 review threads | 1.52 ms mean across 10 frames |
| Headless frame, 100 review threads | 1.49 ms mean across 10 frames |
| Headless frame, 1,000 review threads | 1.49 ms mean across 10 frames |
| 2,000-thread mock CLI payload | 133 ms; one subprocess, no per-thread requests |
| Actual compiled read-only GitHub load + discovery, PR #60 | 1.23 seconds |
| 65 KB inline-heavy Markdown preview | 3–4 ms settled redraw; 11–18 ms parsed and rendered |
| Complete 65 KB source in read-only editor | 13–19 ms open; 1.4–1.6 ms redraw; exact clipboard copy verified |
| Native Metal rendering | 800/1440 px, dark/light, with and without composer |

Run the opt-in live read test explicitly (it never mutates the PR):

```sh
SHARK_GIT_FEATURE_LIVE_PR=https://github.com/KaffeDiem/shark/pull/60 \
  cargo test --features test-support pr::git_feature::tests::live_read_only_pull_request -- --ignored --nocapture
```

The full regression run passed 380 tests with 15 opt-in tests ignored. After the
long-text performance fix, the final focused Git run passed 19 tests with the
opt-in live test ignored (the live read had already passed separately). The native
Metal harness passed in both themes and at both widths. `cargo build` and formatting
checks passed. Strict Clippy exposed four existing lint categories in unrelated
code (`type_complexity`, `needless_borrow`, `non_octal_unix_permissions`, and
`unnecessary_unwrap`); checking with only those categories allowed passes. The
existing `block` dependency also emits a future-Rust compatibility warning.

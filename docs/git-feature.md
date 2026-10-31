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

## Commits

Conversation / Commits switches the pane without disturbing the other view: the open
commit, file and conversation drafts stay as they were, and leaving a view stops its
videos. `cmd-s` never posts a draft while Commits or the full reader is shown.

- The list shows the branch's own commits, `base..HEAD`, newest first in topological
  order, including commits merged in from side branches and local commits not yet
  pushed. The base is the host's effective comparison base, the same one Diff uses:
  the host passes `Shark::effective_workspace(ws, cx)` to `GitFeature::set_workspace`.
  Both ends are resolved to SHAs on each refresh. The list is one `git log`, bounded by
  `MAX_COMMITS` in `src/git/history.rs`; a list that hits the bound is labelled `N+`.
  There is no paging. A missing base is an explicit error; it never falls back to all
  history. A moved base resets only the history.
- Opening a commit shows its full message, author, date and changed files. Ordinary
  commits are compared with their parent, merges with their first parent (labelled),
  root commits with the empty tree. Renames show as a delete and an add, as in Diff.
  Empty commits, mode-only changes, symlinks and submodules are explicit entries.
  Newer/Older step through the list. Arrow keys move, Enter opens, Escape goes back.
- A file opens as Diff, Before or After. Added and deleted files expose only the side
  they have. Text uses the read-only editor; Markdown offers Preview / Source; images
  and videos use the shared media renderer; binaries, large files, symlink targets
  and submodules are described instead of drawn. Symlinks are never followed.

Everything is read from Git objects (`src/git/history.rs`): `git log`, `git show`,
`git diff-tree --raw -z`, `git diff` between two commits, and `git cat-file blob`
through a bounded raw-bytes runner (`git::git_bytes`), not the lossy text helper. HEAD,
the index, working files and Diff's unsaved edits are never touched. Loads carry a
generation and the selection they were started for; late results for another repository,
base, commit or file are dropped, and a failed refresh keeps the last loaded list.
Selections live in memory only.

## Rich content

PR descriptions, comments, reviews, replies and historical Markdown render through one
document component (`src/features/git/document.rs`). It splits Markdown into blocks:
Markdown text in the house style, closed `mermaid` fences, and lines holding only
images (`![…](…)`, `<img>`, a bare or Markdown link to an image, video or GitHub
attachment). Unclosed fences and `data:` images stay Markdown. Text blocks split at
paragraph breaks; tables, lists, quotes and indented code stay whole, and link reference
definitions are repeated in every block. An image inside a sentence is left to the
Markdown renderer, which does not load remote or repository-relative images.

- Mermaid uses `merman` (version and features pinned in `Cargo.toml`) with its
  resvg-safe pipeline, rasterized by resvg with system fonts. Diagrams follow the
  theme, render off the UI thread under a deadline and a source size bound
  (`src/features/git/mermaid.rs`), and are cached as PNGs in the media cache keyed by
  source and theme. The image size is read from the PNG header. Failures show the error
  and the source.
- Relative images resolve against the document's captured revision: the commit being
  viewed (Before uses the parent), or the PR's `headRefOid` for conversations. Paths
  cannot leave the repository; a revision missing locally shows why, with a link to
  GitHub. The working tree is never read.
- Remote images and videos download in the background with the system `curl`: HTTPS
  only (redirects too), no `.curlrc`, netrc, cookies or GitHub credentials, bounded in
  time and size (`src/media/fetch.rs`), written atomically into the media cache. The
  size is checked again on the file after the download, because older curl enforces its
  limit only when the response states a length. GitHub attachment URLs have no
  extension; the response's content type decides what they are. Failed, private or
  unsupported resources keep their link and an Open in browser action. Videos never
  autoplay, and one plays at a time.
- Conversation rows keep a bounded compact preview. Every message has a Reader action
  (Read full text for long ones) that opens the complete message as a virtualized list
  of blocks, with Source switching to the read-only editor. The open message is one
  `Reader` (its document plus the optional source editor) in `src/features/git/mod.rs`.

Media rendering (`src/media.rs`) is shared with Diff. Each side names its source:
`Working` (re-hashed against the diff's blob), `Blob` (exact Git object) or
`Downloaded` (a unit variant; its key is the URL hash). Historical sides are always
blobs. Working copies and blobs are cached under separate names: with Git LFS or other
clean filters a working file can hash to a blob while holding different bytes.
Downloads stay private until classified, then are published with an atomic rename as
`{key}.{ext}` in the media cache. That name is the whole record: a later fetch finds the
file by scanning the cache directory for it, skipping derived files such as
`{key}.preview.png`. There are no `.type` or `.size` sidecar files.

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
kept in a bounded cache. Long bodies use a UTF-8-safe formatted preview;
Read full text opens the complete message in a virtualized rich reader; its Source
button shows the Markdown in the house read-only editor, with wrapping, search and
copy. The complete source remains in descriptions, comments
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
cargo test --features test-support ui_tests::git_tests
cargo test --features test-support media
cargo test --features test-support
cargo test --features test-support --test rendering
cargo fmt --check
```

The tests use a local `SHARK_GH` executable to exercise the real subprocess boundary
without publishing test comments. Live API validation uses read-only queries; no live
PR is modified by these tests. History fixtures live in `src/git/history/tests.rs`, the
UI tests in `src/app/ui_tests/git_tests.rs`, and the native harness (`--test rendering`)
renders the commit browser alongside the rest of the artifact.

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

See [QA and verification](git-feature-qa.md) for the test-isolation fixes and detailed
evidence. A prior strict Clippy run exposed four existing lint categories in unrelated
code (`type_complexity`, `needless_borrow`, `non_octal_unix_permissions`, and
`unnecessary_unwrap`); checking with only those categories allowed passes. The
existing `block` dependency also emits a future-Rust compatibility warning.

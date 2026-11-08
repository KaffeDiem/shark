<h1 align="center">Shark</h1>

<p align="center">A native macOS workbench for directing coding agents, running isolated tasks across repositories, and reviewing changes through to merge.</p>

<p align="center">
  <a href="https://github.com/KaffeDiem/shark/releases/latest"><img src="https://img.shields.io/github/v/release/KaffeDiem/shark" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/macOS-Apple%20Silicon-black?logo=apple" alt="macOS, Apple Silicon">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0-blue" alt="GPL-3.0"></a>
</p>

<p align="center">
  <a href="#install">Install</a> · <a href="#quick-start">Quick start</a> · <a href="docs/manual.md">Manual</a>
</p>

## Choose an agent. Give it a task. Review and merge.

| Concept | What it does |
|---|---|
| **Agent** | Keeps a name, instructions, skills, default repositories and model settings across tasks. Lives in the sidebar, with its own task list and history. |
| **Task** | One piece of work, with separate branches, worktrees, conversations and review state. Several tasks can run under the same agent. |
| **Task group** | Optional organization of Home's task list. Moving a task between groups never changes its agent or instructions. |

You start each task and decide what ships. Tasks of the same agent share instructions and sibling activity notes; conversations stay separate. Shark does not schedule or automatically start work.

Hand tasks from Designer to Planner to Coder while keeping the same folder, branches and earlier chats; the next agent receives the previous result in a new chat.

Claude and Codex work across a task's repositories. Shark brings versioned plans, editable diffs, review comments, linked PRs and stacked branches into one native, keyboard-driven app.

## Install

```sh
brew tap KaffeDiem/shark https://github.com/KaffeDiem/shark
brew install --cask KaffeDiem/shark/shark
```

Or [Download Shark 0.1.28](https://github.com/KaffeDiem/shark/releases/download/v0.1.28/Shark-0.1.28.dmg) for Apple Silicon and drag it to Applications. The app isn't notarized: after first launch, allow it in System Settings → Privacy & Security → Open Anyway.

Needs `git`, signed-in `gh` and `claude` on your login shell's PATH. For Codex models, also install `codex` and run `codex login`.

## Quick start

1. **Set up an agent.** Add your repositories through **Add repo** in `cmd-k`. Click **+** beside Agents, name your agent, and set its instructions and default repositories.
2. **Start a task.** Choose the agent on Home (`cmd-0`), describe the work and press Enter. `@designer fix onboarding spacing` does the same. Choose **Repository defaults** for a standalone task and attach repositories as needed.
3. **Guide the work.** Use `shift-tab` for plan mode. Review the plan and approve it when ready; a task needs a repository before building.
4. **Review and ship.** Open **Differ** (`cmd-shift-d`) to inspect changes and send comments back to a chat. Use **Git** (`cmd-shift-g`) for PR discussions and the git menu to create, update and merge PRs.
5. **Finish the task.** Delete it when done; this removes its conversations, plans and worktrees. The agent stays available and records the task in History.

For existing branches or PRs, use **New task from branch or PR…** in `cmd-k`. A selected agent carries through to that task. Every command and its shortcut is discoverable in `cmd-k`.

## Documentation

- [Manual](docs/manual.md): daily workflow, skills, shortcuts and recovery.
- [Design](docs/design.md): product boundaries, UI rules and components.
- [Language intelligence](docs/language-intelligence.md) and [Git feature](docs/git-feature.md): focused references.
- [Research](docs/research.md): dated background, not the current product specification.

## Development

Requires Xcode for Metal; Rust is pinned in `rust-toolchain.toml`.

```sh
cargo run
cargo test -- --test-threads=1
cargo test --features test-support -- --test-threads=1
```

Tests run serially because some change process environment variables. The opt-in `--features test-support --test rendering` target checks real GPU output. Ignored `live_claude` and `live_codex` tests call real models and incur usage; `live_usage` checks signed-in allowances without inference.

To release, merge a PR labeled `release`. CI builds the DMG and updates the version in Cargo, the Homebrew cask and this README. See [release workflow](.github/workflows/release.yml).

System notifications open the finished chat when clicked. They need the bundled `Shark.app` and notification permission; `cargo run` builds skip them.

## License

GPL-3.0 with attribution: copies and derived works must credit “Shark by Kasper Munch Jensen”. See [LICENSE](LICENSE).

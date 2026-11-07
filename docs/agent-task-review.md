# Agent and task user-story review

Review of [PR #83](https://github.com/KaffeDiem/shark/pull/83), head `13978a000663e6eb5b8798080c7527c48442af5c`, against the requested direction: **choose an agent → give it a task → review and merge the result**.

**Implementation follow-up:** all five findings have been addressed, and the full serial test suite passes. The original review was against the head above; the fixes and regression coverage are recorded below.

Three reviewers examined navigation, identity/instructions/skills, and task lifecycle, then implemented the fixes with focused regression tests. Integration review also corrected agent deletion protection while an explicitly named task is being created.

## User stories and acceptance criteria

“Pass” means supported by inspected code and existing automated coverage where cited. It does not imply a live Claude/Codex session or a real GitHub merge was performed in this review. “Partial” means the normal path works but an acceptance criterion fails. “Fail” identifies a specific broken journey.

### Choose and reuse an agent

| ID | User story and acceptance criteria | Verdict | Implementation evidence |
|---|---|---|---|
| S1 | As a developer, I configure **Designer** once with instructions, skills, default repositories, model and thinking settings, so I can reuse that way of working after restarting Shark. The agent's identity survives name edits. | **Pass on normal storage paths** | `Agent` persists its configuration; skills live in a per-agent source directory. Agent defaults take precedence for new chats. Stable identity and persistence are tested in `agent_settings_creates_and_edits_a_stable_identity`; defaults in `agent_defaults_and_instruction_context`. See [state.rs](../src/state.rs), [agent_settings.rs](../src/app/agent_settings.rs). Skill error handling is assessed separately in S10. |
| S2 | As a developer, I select an agent in the sidebar and visit **Tasks**, **Instructions & skills**, or **History**, including when it has no tasks. | **Pass** | Sidebar agent rows derive from persisted agents; the three page tabs and empty state are exercised by [agent_settings_tests.rs](../src/app/ui_tests/agent_settings_tests.rs). |
| S3 | As a developer, I choose Designer on Home, type “Fix onboarding spacing,” and press Enter. The task uses Designer's defaults and instructions; switching agents preserves my draft. | **Pass** | Home selection and typed `@slug` share a source of truth. `home_agent_picker_preserves_draft_and_standalone_choice` and `persistent_agent_starts_isolated_tasks_with_instructions` exercise the flow. See [home_tests.rs](../src/app/ui_tests/home_tests.rs), [agent_tests.rs](../src/app/ui_tests/agent_tests.rs). |
| S4 | As a developer, I choose Designer and start work from an existing branch/PR or Linear issue. The task still belongs to the selected agent and receives its configuration. | **Pass — F1 fixed** | Branch/PR and Linear launchers capture the visible agent before opening their dialogs. Branch tasks strip the recognized mention and preselect agent repository defaults. Linear retains its existing repository-free Plan flow while applying agent instructions, skills and model settings. Tests cover later draft changes and skill links for both harnesses. |

### Run isolated pieces of work

| ID | User story and acceptance criteria | Verdict | Implementation evidence |
|---|---|---|---|
| S5 | As a developer, I run two Designer tasks concurrently. Each has its own branch, worktrees, conversation and review state; I can switch between them. | **Pass for structural isolation** | Separate task identities, directories and chat IDs; two-task coverage in `persistent_agent_starts_isolated_tasks_with_instructions`. Review state is task keyed. See [agent_tests.rs](../src/app/ui_tests/agent_tests.rs), [state.rs](../src/state.rs). Behavioral independence is assessed in S7. |
| S6 | As a developer, I delete an unfinished task and later give Designer the same request again. The new task starts fresh rather than silently resuming the abandoned branch. | **Pass — F2 fixed** | Automatic launches use the existing fresh-branch planner, which fetches and rejects retained local or remote refs before worktree creation. A collision preserves the prompt and gives retry guidance. Explicit branch/PR checkout still supports deliberate reuse. |
| S7 | As a developer, I ask Designer to explore two alternative fixes to the same file. Each task can modify its isolated copy without restrictions imposed by the other task. | **Pass — F3 fixed** | Sibling notes explicitly state that separate worktrees permit independent edits. The context regression checks that sibling activity introduces no avoid-editing restriction. |
| S8 | As a developer, I open a new task with the same agent without inheriting other tasks' conversations. Tasks begin only when I explicitly start them. | **Pass within reviewed paths** | Shared context contains instructions and sibling metadata, not sibling transcripts or archived history. Creation paths are user initiated; no scheduling or automatic task creation was introduced. See [state.rs](../src/state.rs), [home.rs](../src/app/home.rs), [task.rs](../src/app/task.rs). |

### Keep instructions consistent and groups organizational

| ID | User story and acceptance criteria | Verdict | Implementation evidence |
|---|---|---|---|
| S9 | As a developer, I edit Designer's instructions and have its existing and future tasks receive them in Claude and Codex. Failed root-document writes are surfaced and can be retried. | **Pass on tested paths** | Instructions are written to both root documents and delivered to existing chats through fingerprinted context notes. Tests cover failed document refresh and startup recovery after settings were saved before the dialog closed. See [agent_settings_tests.rs](../src/app/ui_tests/agent_settings_tests.rs), [agent_tests.rs](../src/app/ui_tests/agent_tests.rs), [task.rs](../src/app/task.rs). |
| S10 | As a developer, I add or refresh Designer's skills once and use them in its tasks. A failed refresh reports an error and preserves previously installed skills. | **Pass — F4 fixed** | Failed source scans preserve existing links; scan, manifest and install errors reach Refresh skills and settings. An absent source still removes old links. Codex startup reports refresh failures as a notice while actual instruction-write failures stop startup. Filesystem tests exercise scan and install failures, recovery and ownership boundaries. |
| S11 | As a developer, I organize tasks into optional groups under **All tasks**. Moving or regrouping them never changes their agent, instructions or agent-page membership. | **Pass** | Group moves change section membership only. Tests cover unchanged `AgentLink`, legacy groups, ungrouped tasks and drag boundaries. See [home_tests.rs](../src/app/ui_tests/home_tests.rs), [drag_tests.rs](../src/app/ui_tests/drag_tests.rs). |

### Review, ship and return

| ID | User story and acceptance criteria | Verdict | Implementation evidence |
|---|---|---|---|
| S12 | As a developer, I inspect a task's diff, send review feedback and decide what to merge. Actions apply to that task, and agent overview pages cannot operate on a hidden task. | **Pass in inspected code and automated coverage; live merge unverified** | Existing Diff/Git flow remains task scoped. `agent_pages_do_not_mutate_hidden_chats_or_run_hidden_tasks` guards navigation boundaries. See [agent_navigation_tests.rs](../src/app/ui_tests/agent_navigation_tests.rs), [task_review_tests.rs](../src/app/ui_tests/task_review_tests.rs), [git_tests.rs](../src/app/ui_tests/git_tests.rs). |
| S13 | As a developer, I finish or delete all of Designer's tasks and return later. Designer and its configuration remain available, with records of prior work. | **Pass, bounded history** | Task removal preserves the separate agent entity and records task metadata. The two-task integration test deletes both tasks and checks retained history. History retains the newest 100 archived task records; skills remain in the agent source folder. |
| S14 | As a developer, I read Designer's History and distinguish fully merged work from unfinished or uncertain work across multiple repositories. | **Pass — F5 fixed** | Every relevant repository must have merge evidence for the whole task to be marked Merged. Multi-repository archive tests cover all merged, merged/open and merged/unknown outcomes. |
| S15 | As an existing Shark user, I upgrade without losing task identities, groups, worktree paths or conversations. | **Pass in compatibility coverage** | Legacy serialized workspace names remain supported; `pre_rename_state_round_trips_unchanged` checks the pre-rename fixture. See [state.rs](../src/state.rs), [pre-task-rename-state.json](../tests/fixtures/pre-task-rename-state.json). |

## Fixes and regression coverage

| Finding | Implemented change | Regression evidence |
|---|---|---|
| F1 · Selected agent lost by alternate launchers | Capture the visible agent in the branch/PR form and Linear picker. Strip recognized mentions from branch-task prompts; use agent repository defaults for initial selection. Track pending task-to-agent identity instead of inferring it from branch names, preventing deletion during asynchronous creation. | `branch_task_captures_selected_agent_and_repository_defaults`, `starting_from_an_issue_keeps_the_agent_chosen_before_lookup`, existing hidden-draft coverage, and `agents_do_not_create_sections_and_deletion_rechecks_open_tasks`. |
| F2 · Automatic task adopts an abandoned branch | Automatic launches require fresh refs; explicit branch/PR launches retain reuse semantics. Errors keep the Home prompt and explain retrying. | `automatic_tasks_refuse_retained_branches_but_explicit_tasks_can_reuse_them` checks retained local refs, remote-only refs, unchanged old commits and deliberate reuse. |
| F3 · Sibling file restrictions undermine independence | Replace the restriction with an explicit statement that other worktrees do not restrict edits here. Update the manual. | `sibling_context_includes_empty_state_and_tracks_only_content_changes` verifies independent-edit wording and context refresh fingerprints. |
| F4 · Skill refresh hides failures | Return errors from scanning and reconciliation, preserve links when scanning fails, surface errors in explicit refresh/settings, and send startup refresh warnings through the existing harness notice event. Keep root instruction-write errors separate. | `agent_skill_refresh_preserves_links_on_scan_failure_and_reports_install_errors`, existing skill ownership/collision/symlink tests, and the harness initial-notice assertion. |
| F5 · Partially merged task labelled Merged | Aggregate merge evidence across every task repository. Known open work is not merged; incomplete status remains unknown. | `agent_history_requires_all_repositories_to_be_merged` exercises all-merged, merged/open and merged/unknown task deletion. |

## Validation and limits

- Original review baseline: `cargo test --features test-support -- --test-threads=1` passed **964 tests**, with **21 ignored**.
- Implementation validation: `cargo test --features test-support -- --test-threads=1` passed **969 tests**, with **0 failures and 21 ignored**, in 418.48 seconds. Focused regressions, `cargo fmt -- --check`, and `git diff --check` also passed.
- New automatic tasks reject branch collisions rather than silently reusing branches; retrying generates another candidate name.
- Linear issue creation remains repository-free and in Plan mode until repositories are attached. Selecting an agent supplies its identity, instructions, skills and model settings.
- No live provider session, real remote PR merge, or native appearance/accessibility pass was performed. Scheduling, automatic task starts and shared conversational memory remain outside scope.

The skills page uses a folder plus Refresh rather than a built-in skill editor. History retains the newest 100 archived metadata records and PR links, not conversations. Neither is treated as a missing feature under the supplied scope.

Skill refresh is not transactional across tasks: some tasks may update before another reports an error. Failed scans preserve that task's existing links, and retrying is safe after resolving the reported filesystem problem.

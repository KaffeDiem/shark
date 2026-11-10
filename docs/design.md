# Shark design rules

Shark is a local workbench for human-directed coding with persistent agents. People give work to an agent, inspect its changes, guide it when needed and decide what ships. The interface should make identity, current work and the next human decision easy to see. It does not turn agents into an unattended scheduling or approval system.

Read this before changing UI. This document defines design constraints; [the manual](manual.md) explains core workflows and essential shortcuts; the command palette is the complete command and key reference. Keep detailed behavior in the owning component and its tests. If a feature needs a different rule, update this document in the same change.

## Agents, tasks and ownership

An **agent** is a reusable name, icon, instructions, skills and defaults. It has a stable slug and survives completed work. An agent is a first-class sidebar destination, including when it has no tasks. Its page has no tabs: the header is its settings. Click the 40px avatar to change it, the name to rename it in place, and type straight into the instructions below the name. A quiet line shows **1 skill** (or the plural count) and **Hands off to nobody** (or the next agent's name); each opens an anchored menu. The header's `…` offers identity editing and deletion; the sidebar's right-click also offers task and skills actions. Below the header sit the composer, the agent's tasks and its history. Identity, instructions, skills and handoff edits save at once. Repository, model and thinking defaults appear once, as the composer's untouched state. Composer controls affect only the draft until **Save as default**, which writes repositories, model and thinking to the agent. Branch, PR, issue and file choices are never saved as defaults. A single **Edited** control appears whenever repository, model or thinking settings differ; its menu names removed repositories and provides **Reset** and **Save**. **Reset** restores the agent defaults without clearing the prompt, issues or files. The composer model menu offers **Follow app default**; saving it stores unset model and thinking values, retaining repository or app inheritance.

A **task** is one isolated piece of work, with its own branches, worktrees, chats and artifacts. `Task.name` is immutable internal identity; `Task.title` is editable display text. `Task.agent` determines agent membership. Several tasks can belong to one agent and run concurrently. A task without an agent is valid. A **harness** is the Claude or Codex process serving a chat, not the persistent agent.

A **task group** is optional organization of Home's list. It does not assign instructions, alter agent membership or create an agent. Plain `Section` entries provide these groups; legacy agent-bound sections remain persisted for compatibility but do not control navigation or membership. Agent navigation comes directly from `State.agents` and `Task.agent`.

The shell owns destinations, task navigation, artifact tabs and feature launchers. Artifacts own their controls and view state: chats own transcripts, composers and plans; Diff owns its changed-files tree; Git owns PR conversation and commits; file editors own buffers. The Features rail has **Differ and Git**. Each opens or focuses one artifact per task. Browsing either must not require a running model. Reuse this lifecycle when adding a feature; do not introduce a plugin architecture or persistence format for ordinary UI work.

### Persistent state and delivery

`state::Agent` holds instructions, repository/model/effort defaults and up to 100 history records. `AgentLink` snapshots keep root-document generation, harness startup and task context independent of global `State`. Instructions reach both harnesses through root `CLAUDE.md` / `AGENTS.md`; changed instructions reach live chats through fingerprinted context. Do not replace a harness's coding or plan-mode prompt to provide agent instructions.

`Shark::sync_agent` refreshes sibling snapshots and publishes tasks to loaded chats. Instruction snapshots advance only when root docs can deliver them; failed writes stay visible and retryable. Dialog close and explicit Refresh skills update docs and skill links. Avoid filesystem scans and Git remote lookups on every keystroke or render.

Skills have one harness-neutral source: `support_dir()/agents/<slug>/skills/<skill>/SKILL.md`. `HARNESS_SKILL_DIRS` exposes owned links in `.agents/skills` and `.claude/skills`. Repo-skill settings do not disable agent skills. Each discovery folder has its own ownership manifest; pruning must preserve files and links Shark does not own.

Skill import accepts GitHub sources and local skill files or folders. Preview and selection happen before copying complete skill folders into the agent's existing storage; import never executes bundled code, installs plugin hooks, connects services or silently replaces an existing skill. Record source provenance with the imported copy. Refresh task links automatically after import and distinguish refresh failure from import failure. Keep manual folder access and Refresh skills available. Installing a skill makes it available; persistent activation belongs in the agent's instructions. Do not grow this importer into a marketplace or plugin manager.

Sibling notes describe other open tasks, their directories and changed files, including the empty state. Background refreshes must ignore removed or archiving tasks, avoid publishing unchanged results and preserve known file lists when another task starts. These notes support coordination; they are not locks or read-only permissions.

Keep existing installs readable: serialized legacy names, the `NEW_TASK` stored sentinel, `~/shark/workspaces` paths, old root-document attachment parsing and `SHARK_WORKSPACE` environment variables remain compatible. New task environment aliases supplement the old names. Renaming Rust or UI vocabulary is not permission to move worktrees or rewrite the storage contract.

## Product and visual principles

1. **Reading comes first.** Prose, code and evidence dominate; chrome stays quiet. Contrast and line length matter more than decoration.
2. **Human decisions stay explicit.** Users start tasks, answer questions, approve plans and invoke Git actions. Scheduling, automatic task assignment, shared conversational memory and permission templates are outside the current model.
3. **Keyboard and accessibility are primary.** Every command has a keyboard path; pointer-only previews cannot contain the only copy of important information.
4. **One accent, two equal modes.** Indigo marks focus, model activity and primary actions. Status uses icons and readable text rather than colored chrome. Check dark and light.
5. **Reuse before building.** Use gpui-kit and existing house patterns. Keep each feature responsible for its own view; share a control or formatter when it expresses the same behavior, not merely similar pixels.

## Theme
- **Source of truth:** `assets/themes/*.json`, one `ThemeSet` per family with `<Family> Dark` and `<Family> Light`, listed in `THEMES` in `src/settings.rs`. Change colors there, not in Rust. `linear.json` (the default) is hand-tuned; the others are generated by `python3 docs/themes.py` from codigrate palettes, popular editor themes' official palettes and Shark's own, which lifts text colors to the contrast floors below. Edit the palette in the script and regenerate; don't hand-edit generated files.
- **Applying:** `settings::apply` installs the saved light and dark themes with font overrides baked in (a mode switch reloads the config), and `Shark` re-applies it on system appearance changes while the mode is System.
- **The table below lists Linear's values.** Every theme must define the same tokens and meet the same contrast rules.
- **In code:** read colors from `cx.theme()` (`ActiveTheme`). No hard-coded hex values in views. Use `faint()`, `side_hover()` and `side_on()` for their existing theme-derived blends. Plan changes may encode theme-derived colors in markdown marks; rebuild them when the theme changes.

### Color tokens
| Token (`cx.theme().…`) | Dark | Light | Use for |
|---|---|---|---|
| `sidebar` | `#09090a` | `#efeff0` | Window chrome: sidebar and the frame around the content island |
| `background` | `#121213` | `#f9f9fa` | The content island |
| `border` | `#232426` | `#dedede` | Island edge, header divider, cards. Use sparingly |
| `foreground` | `#e8e9eb` | `#2f2f31` | Body text, titles, active tab label |
| `sidebar_foreground` | `#8f8f91` | `#656567` | Inactive left-navigation labels, metadata, headers and ordinary icons, expanded or rail |
| `muted_foreground` | `#c9c9cc` | `#535355` | Secondary text people read: section headers, tool summaries, metadata, repo names |
| `faint(theme)` | `#808082` | `#959597` | `muted_foreground` 60% over `background`. Hints (empty-state hints) and idle/decorative icons only. Never information |
| `side_hover(theme)` | `#19191a` | `#e9e9ea` | `foreground` 7% (dark) / 3% (light) over `sidebar`. Row hover in the navigation and Features rail |
| `side_on(theme)` | `#222223` | `#e5e5e6` | `foreground` 11% / 5% over `sidebar`. The grey pill: the open item, and the cursor while its pane has focus. Light takes less so `muted_foreground` and status text keep their floors |
| `secondary` | `#1a1a1b` | `#ffffff` | Raised cards: user messages, composer, active tab pill |
| `secondary_hover` | `#222324` | `#f5f5f5` | Hover on a card |
| `muted` | `#161617` | `#f1f1f2` | Code and tool-output blocks |
| `primary` | `#5e6ad2` | `#5e6ad2` | Focus ring and bar, running indicator, primary button (white text, 4.7:1) |
| `list_active` | indigo 20% | indigo 6% | Keyboard-selected row while its list has focus (always together with a `primary` ring or bar) |
| `list_hover` | `#1e1e20` | `#efeff0` | Row hover, and selected row of an unfocused list, **inside the island** |
| `sidebar_accent` | `#18181a` | `#e7e7e8` | Drop targets in the sidebar. Rows use `side_hover` / `side_on` (light `list_hover` equals the sidebar color) |
| `accent` | `#28292b` | `#efeff0` | gpui-kit's neutral menu-item hover. **Not** the brand color |
| `danger_foreground`, `success_foreground`, `warning_foreground` | `#ff9c9c`, `#5fd39a`, `#f2c94c` | `#c42b2b`, `#1f7a50`, `#8a6800` | Status **text and icons** |
| `danger`, `success`, `warning` | | | Fills only (danger button). Too low-contrast for text |
| `link` | `#a9b2ff` | `#4061da` | Links in markdown |
| `chart_1` … `chart_5` | Theme palette | Theme palette | Background task identity: blue, green, yellow, red, purple; Lc ≥60 on the transcript and hover card |

### Contrast rules
Except for dimmed navigation and file-tree paths, measure APCA (Lc) against **every surface the text can sit on**, including hover and selected rows (`background`, `sidebar`, `secondary`, `muted`, `list_active`, `list_hover`, `sidebar_accent`, `side_hover`, `side_on`, `popover`, `accent`):

| Text | Minimum | Current lowest |
|---|---|---|
| Prose and titles on `background` / cards | Lc 90 | 92.2 dark, 96.2 light |
| Any other readable text: labels, metadata, `muted_foreground`, text in selected/hover rows | Lc 70 | 70.4 dark, 71.4 light |
| Status text and links | Lc 60 | 60.0 dark, 60.1 light |
| Hints (`faint`), never essential | Lc 30 | 33.0 dark, 38.5 light |

Left navigation labels, chevrons and path prefixes use `sidebar_foreground` at **WCAG 2 contrast ≥ 4.5:1** on normal, hover, selected and drop-target backgrounds. Generated themes blend `muted_foreground` 30% toward `sidebar`, then raise contrast to that floor. Navigation rows share these surfaces, so `python3 docs/themes.py` validates every theme, including Linear. The Diff file tree uses `foreground` for folder labels, chevrons and path prefixes on the island background, with `list_hover` and `list_active` row states. This exception does not change the APCA rules for filenames or other text elsewhere.

The displayed destination (Home, agent or task) and keyboard-focused cursor rows use `foreground` for labels and ordinary icons; active/focused task metadata keeps `muted_foreground`. The cursor retains its focus ring, hover keeps its background, and inline naming fields use full foreground. Warning/unread icons and running spinners keep their status colors. Apply foreground colors locally, never whole-row opacity or shared side-row defaults.

Run `python3 docs/contrast.py '<fg>' '<bg>' …` for every new pair. Also: no pure white on near-black, and no font weight below 400.

## Typography
- **UI font:** Inter by default (Settings can swap the UI and code font for any installed font), embedded from `assets/fonts/` (400, 500, 600, 700, italic 400). Use only those weights.
- **Mono font:** JetBrains Mono NL (no ligatures), 400 and 700. **Not inherited:** set `.font_family(theme.mono_font_family.clone()).text_size(theme.mono_font_size)` on code and tool output.
- **Character variants:** `inter_features()` in `src/app.rs` turns on serifed I, tailed l and slashed 0 so paths and hashes are unambiguous. It is applied on the root view and must also be applied to every dialog (`d.font_features(inter_features())`), because dialogs, the palette and notifications render outside the root view.
- **Sizes:** `font.size` is 15 and the window rem equals it:

| Helper | Size | Use for |
|---|---|---|
| `text_xs()` | 11.25px | Metadata, section headers, info rows, status labels |
| `text_sm()` | 13.1px | Default UI: sidebar rows, tabs, buttons, tool rows |
| `text_size(px(14.))` + `line_height(relative(1.5))` | 14px | Transcript prose and user messages |
| `text_base()` | 15px | Empty-state and dialog titles |

- **Weights:** 400 for text; `FontWeight::MEDIUM` for emphasis (active sidebar item, tool names, section headers); `SEMIBOLD` for page titles and markdown headings.
- Sentence case for labels, headers and menu items ("Tasks", not "TASKS"; "Copy branch name", not "Copy Branch Name"). Proper nouns keep their capitals ("Reveal in Finder"). The macOS menu bar is the one exception: it uses Title Case, as every Mac app does.
- **Line length:** prose sits in a centered `max_w(px(680.))` column (~70 characters). The composer card is `max_w(px(712.))`.

## Navigation and layout

The window uses an inverted L: sidebar and top chrome on `sidebar`, with one content island on `background`. Below the 44px titlebar, inset the island 8px on the right and bottom; use `radius_lg`, a 1px `border` and clipped contents. Reserve the first 80px of the titlebar for traffic lights. Only designated empty chrome can drag the window; buttons and tab gestures must keep their events.

The expanded sidebar exposes Home, agents and their tasks, plus unassigned tasks. Home is the one overview: its composer starts tasks and its list makes every task reachable (there is no separate All tasks page). Agent headers remain visible when empty. Folding an agent hides its child tasks without changing membership; a cursor on a hidden task moves to its agent header. Selected identities must survive insertions, deletions and reordering: use task names or agent slugs when vector indexes can shift. The 72px collapsed rail retains tooltips, accessible names and keyboard activation.

Home and agent pages share a multiline task composer. Enter or Cmd+Enter starts; Shift+Enter adds a line. A single compact row uses the task header's `AttachRow` and 26px `Shark::chip` controls: Home's agent first, then repositories, issues, files and Attach; model, Plan and Start stay on the right. The agent picker and typed `@slug` share the prompt prefix, preserving the body on selection changes. With no agent selected, the chip reads **@ Agent**. Each repository is one keyboard stop; its label adds a branch or PR only when it differs from a new branch from the default. Its menu begins with **Branch or pull request…**, followed by repository switching and Remove. Issue chips show identifiers and reuse the existing detail hover card; file chips show names. No inline remove buttons appear. Without repositories, **Plan only** is locked and its tooltip explains how attaching a repository enables implementation. There is no empty repository slot or explanatory note below the row.

The row shares the header's measured overflow: Attach loses its label first, hidden chips move into **+N**, the model label shortens to the model name, and Plan moves into the model menu before the model can truncate further. Start stays visible. The overflow menu lists hidden chips and opens each chip's own menu. Arrow keys traverse only visible chips, +N, Attach and Edited; Enter or Space opens the focused menu, Backspace removes only a focused chip, and Down or Escape returns to the prompt. Focus follows item identity on resize; a newly hidden focused chip moves focus to +N. Cmd+I opens Attach, Cmd+Shift+B opens the focused or first repository's branch picker, Cmd+/ opens model selection and Shift+Tab toggles Plan. The hint row appears only for a focused empty prompt, with no Cmd+I hint.

A leading `@` lists agents; elsewhere it lists committed files from each selected start ref. `#` lists issues and PRs and `/` lists skills. Enter mentions a named issue or PR; Option+Enter attaches it without starting. Unknown slugs remain ordinary text. Each agent keeps its draft and untouched repositories follow its defaults. Composer choices affect only the draft until saved through Edited. Tab without completion toggles Ask Shark on Home; Ask mode hides the row except Start. Agent pages remain overview destinations and never silently act on a chat or draft hidden behind them. Explicit navigation can return to an open task, and New task from an agent page prepares that agent's prompt.

Home's list can group by attention status or optional task groups. Every task stays reachable, including Ungrouped. Use group identity rather than its display label: a user may name a group “Ungrouped.” Empty groups remain manageable through Task groups…. Dragging tasks changes order only within compatible agent/group membership; it never changes either membership. Preview items occupy valid task slots and must not overlap fixed agent headings.

The **All repositories** control beside grouping narrows Home to one repository or **No repository**; a task matches either an attached repository or one being attached. The selected repository appears in a filled pill. The picker shows task and needs-you counts in repository order. A filtered list ends with the count of tasks in other repositories, their needs-you count and **Show all**. With no matches, show **No tasks in {repo}**, the count elsewhere and **Show all repositories**. The filter stays in memory until restart; agent pages, the sidebar, Dock badge, cmd-j and task picker ignore it. Task menus offer **Show tasks in…** and open Home with that filter.

Use `side_row` / `sidebar_row`, `side_header`, `cursor_row` and `list_cursor_row` instead of restyling lists. Ordinary side rows have a 32px pitch, 16px icons, `text_sm`, `mx_2`, and a flexible truncated label. Sidebar task rows are one line; a task that needs you adds the required action below in warning text. An agent row shows one fact on the right, the most urgent: tasks needing you, else running tasks, else (folded) its task count; hovering it reveals New task. Counts and metadata use `text_xs`.

Island task lists (Home, agent pages) use one 36px line per task: status icon, title, what it waits on, then agent (Home only), repository status and age on the right. Branch and chat count live in the tooltip and accessible label. The open destination and the keyboard cursor are distinct states; show the focus ring only in the focused pane.

Task headers expose agent identity as a navigation link, repository/issue attachments and Git controls. Keep controls reachable when narrow: truncate names, let labels yield before essential actions and wrap toolbar rows where needed. A hover card may explain branch, base, path or status, but clicking or keyboard activation must expose the same facts. Failure-first overflow keeps hidden attachment problems reachable.

### Artifact tabs and feature panes

Chats, Diff, Git and file editors share the persisted mixed tab strip. New or explicitly reopened artifacts append; replacing a file preserves its slot. Feature launchers focus an existing artifact or reopen it without duplicates. Closed feature tabs stay closed across restart until explicitly reopened. Tab navigation counts only visible tabs.

Tabs are 28px tall, up to 200px wide, shrinking to 100px before scrolling. Use `secondary` and `foreground` for the active tab; inactive labels use `sidebar_foreground`, with `foreground` on hover. Long labels fade over their final 24px only when overflowing, with the full value in a tooltip. Reserve stable close/copy hit areas: hover must not move text. File tabs show unsaved state; chat tabs show activity or unread state. Background closure preserves the current artifact and focus.

The 72px Features rail is present beside task artifacts and absent from overview destinations and Zen. Differ and Git have named, keyboard-reachable launchers; the active artifact has the pill. Feature-specific controls belong inside their artifact, not the rail. Pane navigation includes only visible panes.

Diff's comparison toolbar spans its artifact and wraps when narrow. Its file tree lives on the right, below that toolbar, on the island background. It is shown by default, resizes from 200–560px with a 244px default, and retains its saved width. Hiding it returns tree/filter focus to Diff and removes it from pane navigation. Files uses the shared list cursor treatment, 30px rows, 12px indentation and folder-first ordering; compact single-child folders and decorative guides must remain correct after filtering and folding. Folder labels and path prefixes use `foreground`; filenames preserve viewed/open state. Never dim a whole row.

Zen is session-only and shows the current artifact on the island background, with a 44px bar protecting traffic lights. It hides navigation, task header, tabs and Features rail while preserving shortcuts and artifact controls. Asking for Home or the sidebar leaves Zen. File operations must preserve the user's existing sidebar mode.

### Task quick open

The dedicated task picker (`src/app/task_picker.rs`) includes every live task regardless of grouping or folding. Reuse `CommandState`, variable-height `CommandItem` rows and the 640px `menu_dialog`, constrained to the viewport. Focus search immediately; arrows navigate, Enter/click opens and Escape restores focus. Releasing Command does not confirm this picker.

Score task and repository names separately, then use interaction recency and sidebar order. With no query, prefer the most recent other task, or the most recent task from Home; query edits select the best match. Background status updates must preserve result order and selected identity. Show task status, explicit PR state, repositories and recency; distinguish no tasks from no matches. Overflow tooltips and accessible labels retain complete names and state.

`Task.last_interacted_at` records explicit successful user interaction, not startup restore, polling, model output or browsing the picker. Coalesce saves and flush at close/quit. Resolve a destination by name again after any unsaved-edit confirmation; cancellation or a disappeared task must preserve buffers and must not record a successful visit.

## Controls, menus and settings

Use `ui::toolbar(id, button)` after configuring the house button so its action, focus, disabled/loading state and accessibility survive styling. Default controls are 32px high with 16px icons: icon-only controls are circular and labeled controls are capsules. `ui::quiet_toolbar` is for chrome icons with no resting fill or border. Selected/open controls stay surfaced until dismissed; primary and destructive actions stay prominent. Use at most one primary action per view.

Hover feedback takes 120ms; press, keyboard focus and selected states respond immediately. Keyboard focus must remain distinct from a mouse click's focus. Reduced motion resolves transitions immediately. Toolbar labels meet WCAG 4.5:1 on their actual surfaces; ordinary text follows the APCA table above.

Shared menus live in `src/menu.rs`. Use `menu::chrome`, `menu::highlight` and house item builders, including composer completions. Geometry: `popover`, 14px corners, 6px outer inset, normally 200–440px wide, with an 8px window margin; 36px action rows, 16px icons, 14px type and muted hints. The shared ring and soft shadow replace a hard border. Labels take space before hints; fixed icon/check/chevron slots stay outside text. Show full clipped labels in tooltips.

Dropdowns anchor to their control; context menus open at the pointer. Submenus use rendered bounds and open on the roomier side. Preserve a traversable gap and directional hover protection. When a side-by-side submenu cannot fit, click/keyboard activation replaces the parent; Back, Left or Escape restores its selection and scroll. Keyboard movement scrolls the highlighted row into view. Outside clicks dismiss without activating underlying content.

Menus nest one level. "Open in…" folds out into every installed app; an app with several targets (task folder and each repository) opens a picker of them.

Menu items perform actions. A noninteractive heading may explain a group of actions, as Edited does with its defaults summary. Put other facts in hints or hover cards. Hide inapplicable actions; show checked choices with the real `Check` slot. Destructive items go last. Use sentence case and an ellipsis when opening a picker or confirmation. The command palette carries shortcut hints; ordinary menus need not repeat them. The task attachment picker keeps its Suggested rows fixed while open. Background results remain searchable in their own groups; refreshed suggestions apply on the next opening.

`COMMANDS` in `src/app/palette.rs` is the shared command catalog. Add labels, keywords, action and handler together; keep `GROUPS` and conditional command lists consistent. Palette rows use `ui::command` in `menu_dialog`, with native keyboard navigation. Filter backend-specific commands by capability. Menu-bar items dispatch the same actions; standard macOS menus keep their platform behavior.

Settings is an 800px dialog with a 168px navigation column and shared `section()`, `card()` and `row()` patterns. Its pages are General, Appearance, Prompts, Repositories and Linear. Agents are direct destinations, not a Settings page. Repository details use a focused dialog over their destination; entering a task closes any covering dialogs. Settings apply immediately. Use `Select` for multi-choice settings and a textarea for instructions; avoid an unnecessary second Save flow.

Dialogs use `ui::dialog` / `ui::alert_dialog`: 14px corners, 20px form padding and compact 14px text, with `inter_features()`. Menu dialogs retain the menu's smaller inset. Native 180ms fades must keep hit targets still and respect reduced motion. Prefer an existing small `Render` entity for a form over custom overlay infrastructure.

## Artifact content

### Chat, plans and questions

Closed Mermaid fences render inside the code-block surface across Markdown views, scaled down to the column and capped at 560px tall (900px in Git); Copy keeps the source and Open opens the full-size image in the system viewer. Streaming or failed diagrams keep their source; failures add one muted error line.

Transcript prose is primary; tool calls are evidence. Group consecutive ordinary calls between prose into one summary row counted by verb. Show the running call and failures on the collapsed summary so collapse never hides a failure. Expand to compact 28px rows and individual outputs. Skills and subagent invocations remain standalone intent rows (`BookOpen` / `Bot`). Paths are task-relative. End turns with duration and done/interrupted state, including interruptions recovered on replay.

Keep prose in the 680px reading column and composer in its 712px card. Code and output explicitly use the mono font; do not inherit UI typography. User messages, tools and rendered markdown use the shared copy controls. Reserve their space so hover or the copied check neither shifts nor covers text. Keyboard copy and palette actions must remain available.

Composer completion (`src/complete.rs`) opens upward on the shared menu surface, at composer width, with eight visible 36px rows and scrolling. File/skill sources load in the background and reuse their previous results while refreshing. A file choice is an atomic inline token with task-relative identity; the repo is part of what gets sent. Escape closes completion without losing composer focus. Completion must not block typing or confuse a highlighted suggestion with a sent message.

Handoff uses the shared `aside_card` surface above the composer: a compact completion bar with the next agent, an explicit handoff action, Other and dismiss. The receiving chat shows a removable quote, collapsed to three lines and expandable with bounded scrolling. Keep the composer empty and focused, with the receiving agent named in its placeholder. Chat tabs retain their starting agent icon; activity indicators take priority.

The message queue stays inside the composer card and exists only while nonempty. It shows send order, stable previews and send-now/edit/delete actions. Explain whether Enter sends, queues or saves an edit. Editing pauses delivery; moving a queued row must preserve its message identity and focus. Use the same list selection and keyboard patterns as other queues.

Plan mode owns a pane inside Chat, not a global sidebar: 48% width, minimum 340px, versioned markdown, a Changes toggle and explicit build actions. Approved state stays attached to the right version. Theme-derived added/removed marks rebuild on theme changes; code changes use diff blocks. Questions use gpui-component's `Questionnaire`, one question at a time, with a scrolling body and fixed actions. Take focus on arrival only from the same chat's transcript, queue or empty composer; never steal it from work elsewhere. A side question stays explicitly separate from the main conversation.

### Git, attachments and integrations

Git (`src/features/git/mod.rs`) is the PR artifact, with Conversation/Commits, repository selection when needed, local drafts and Refresh. Reuse the centered reading column and house virtual list. PR title, state, branch and author metadata precede the timeline. Comments and threads use `secondary` cards with a 1px border; event-only and collapsed rows stay compact. Review state and resolution always have text as well as icons. Long paths give way from the folder prefix so `file:line` remains visible.

Reading and editing share `edit_box` geometry so focus does not move surrounding content. Local drafts remain distinct from submitted comments. Save/Comment/Reply are explicit; Keep draft and Discard state their effect. Timeline keyboard selection is shown only after keyboard navigation in the focused pane. Source links and copy actions reserve a stable slot.

Repository chips show repository identity and attachment status, with branch details available through the menu/card. Successful probes describe the actual checkout; saved data is the fallback before a probe or on failure. Repository actions target that repository; task-wide stack actions remain task actions. Detached HEAD, changed checkout and unavailable status must stay distinguishable from ordinary branch state.

The header has one Attach entry. Repository attachment and Linear search remain separate flows: one mutates local worktrees and waits for safe task state, the other searches remote issues and can operate mid-turn. Lists share keyboard marking/confirmation conventions, explicit counts and status text. Failed or pending operations open recovery, not a misleading ready menu. The Git button uses primary for the next runnable step, outline for attention and ghost for waiting; writing steps cannot run over incompatible active work.

Linear controls outside Settings are gated by `linear_in_use`: a connection in any state or an attached issue. Do not add unsolicited connect prompts. Disconnected issue data remains readable, with a route to Settings › Linear. Remote state, missing snapshots and unavailable service responses must not be presented as successful completion.

## Status, accessibility and interaction

Use Lucide through `IconName`, not Unicode glyphs standing in for icons. Default sizes are 16px in navigation/menus, 14px in island rows/tabs, 12px beside metadata, 32px in empty states and 40px for the agent header avatar (22px symbol in a secondary circle, 26px emoji or circular image). Icons inherit text color unless they communicate status. Vendored Material file icons (`src/file_icon.rs`) are the full-color exception; unknown types fall back to neutral Lucide icons and light variants must remain legible.

| State | Shared treatment |
|---|---|
| Idle task | `CircleDashed`, muted |
| Running task/chat | `LoaderCircle` / `Spinner`, `primary` |
| Needs human action | `CircleAlert`, `warning_foreground`, explicit action text |
| Finished unseen | `CircleCheck`, `success_foreground` until opened |
| Failed tool or operation | `X` / `CircleX` / `TriangleAlert`, `danger_foreground` or warning as appropriate, error text |
| Ordinary waiting/syncing | Static `RefreshCw`, `Clock` or PR icon; muted unless attention is needed |
| Open/draft/merged/closed PR | `GitPullRequest` success / `GitPullRequestDraft` muted / `GitMerge` muted / `GitPullRequestClosed` danger, plus state text |
| Unsaved file / changed on disk | Foreground dot / warning `TriangleAlert` with Reload and Keep mine |
| Viewed / changed since viewed | Checked checkbox and muted name / warning dot |
| Added/deleted lines | `success_foreground` / `danger_foreground`, signed counts or diff marks |
| Resolved thread / hidden comment | Success `Check` / muted `EyeOff`, explicit collapsed label |
| Skill / subagent invocation | Muted `BookOpen` / `Bot`, standalone row |

Running outranks needs-you, which outranks unread. Plans or questions awaiting an answer count as needs-you even when a turn has ended. Unknown PR state is unknown, not “not merged”; history uses “PR state unknown.” Status belongs in text and accessible labels as well as color. Ready attachments stay neutral so their icon cannot be mistaken for passing checks. Terminal PR state outranks draft.

Use animation sparingly: model activity may spin in primary; watched loading, questions and current attachment steps may use muted/text-colored spinners. Background shell/subagent activity uses the existing animal/terminal markers and reduced-motion fallback. Do not animate ordinary Git polling or checks. Quota rows show percentages and reset/pace information; unknown values show a dash rather than a reassuring success color.

Focus in side navigation is a `side_on` pill plus a 1px primary border, without indigo fill. Island list focus uses `list_active` plus a primary border or transcript bar; tint alone is insufficient. Inputs/buttons use the component ring. Hover changes surface or label color but is never the only discovery mechanism. Disabled controls use the component's disabled state, no hover and no focus. Text selection stays translucent so selected text remains readable.

Use real buttons with accessible names for icon-only actions; a tooltip is not an accessible label. Div-based controls need the appropriate role, label, focus handling and keyboard activation. Full clipped values and state must be available without hover. Empty states distinguish absence, loading, error and no matches, and name an available next action.

Every keyboard view has a `FocusHandle` and `key_context`. Scope list keys to the list, not typing fields. Bindings live in `bind_keys` (`src/main.rs`); commands without frequent shortcuts are still available through the palette. New global commands need the action, command row and appropriate menu entry together. Add a manual shortcut only when it is essential to a core workflow; keep the complete command and key inventory in the palette. Add list contexts to the existing navigation sets and use `nav_actions` rather than rebuilding navigation.

Vim is one setting for editor, composer and lists. Gate Vim bindings through the root `Vim` context and keep non-Vim alternatives: arrows, Home/End, Page Up/Down and pane navigation. Restore useful focus after dialogs, hiding panes and switching tabs. Agent tab changes focus the page and reset its scroll; opening an agent selects one of its own tasks. Destructive row commands must resolve the focused visible task, never an unrelated hidden active chat.

## Data safety and failure behavior

Confirm when data is lost. Task deletion and force deletion remove conversations and plans; agent deletion removes instructions, history and skills. Dialog titles use “Verb name?”, descriptions explain consequences and the affirmative button names the verb. Ordinary reversible navigation and setting changes do not need extra approval dialogs.

Refuse deleting an agent with open tasks or a task still being created. Recheck conditions after confirmation, not only before opening it. Save state before destructive folder cleanup and retain a recoverable state on failure. Validate folder identity and ownership; do not traverse an unexpected parent symlink. A task root containing unowned files, including harness settings alongside skill links, must fail archive preflight rather than silently removing them.

Task switching, deletion, file replacement and quitting must check every dirty buffer. Re-resolve asynchronous destinations before changing buffers; canceled or failed operations preserve edits and drafts. Background tab closure must not change active view/focus. Persist file identities separately from loaded buffers, validate restores and report failed restores together while retaining retryable identities.

History is captured only after successful task deletion using available PR/merge evidence, not a model call. Keep unknown state distinct from a verified result. Errors identify the affected task/repo and a useful recovery action. Use existing notifications rather than custom toasts; ongoing progress ends with an ellipsis. Avoid presenting a saved, submitted or synchronized state until the underlying operation succeeds.

## Shared components and validation

| Need | Reuse |
|---|---|
| Form/settings structure | `ui::dialog`, `section`, `card`, `row`, small `Render` entity |
| Input or instructions | `Input` / `Textarea`; `appearance(false)` inside a card that owns its border |
| Menu or picker | `menu` builders, `open_menu` / `open_grouped`, `ui::command`, `CommandState` |
| Markdown / copy | `TextView` / `TextViewState`, `ui::markdown`, `ui::copy_button`, `Clipboard` |
| Keyboard hints | `Kbd` and `ui::key_footer` |
| Destructive confirmation | Shared alert dialog and danger button |
| Feedback | `window.push_notification` with existing notification types |
| Multi-select | Shared Command row plus checkbox and explicit mark/confirm semantics |

Use the gpui spacing scale and established geometry: `radius` for rows/inputs/code, `radius_lg` for island/cards, 14px for menus/dialogs, full rounding for pills/dots. Prefer a surface change or one border over stacked separators. Do not clone the whole Theme per render; copy needed colors before mutable `cx` calls. Unknown theme keys are silently ignored, `list.active.background` alpha is capped at 20%, and `tab.*` theme keys are unused: check the schema and existing tab code before adding tokens.

For a UI change, verify these five things:

1. It preserves agent/task/group identity and uses the existing artifact lifecycle and controls.
2. All text pairs meet the contrast rules on normal, hovered and selected surfaces; colors come from tokens and typography remains readable.
3. Keyboard navigation, named controls, focus restoration, clipping and narrow-window behavior work without a mouse.
4. Appropriate state and headless UI tests pass, including `cargo test --features test-support -- --test-threads=1`; exercise cancellation and failure paths when data is involved.
5. Inspect actual light and dark windows, including empty, busy and error states relevant to the change. Launch a process with `-NSRequiresAquaSystemAppearance YES` to check light without changing system appearance; respect reduced motion.

Design references are Linear's calm navigation and menus, APCA contrast guidance and the project's existing components. Preserve the constraints here rather than copying screenshots at the expense of readable content or predictable behavior.

# Shark manual

Shark is a native macOS app for working with reusable coding agents. Give an agent a name, instructions, skills and repository defaults; start an isolated task when you have work for it; review the result and take it through to merge. You choose when tasks start and which changes ship.

An **agent** lasts across tasks. A **task** is one piece of work, with its own branches, worktrees, chats, changes and review state. A **task group** organizes Home's task list; it never changes a task's agent or instructions. Claude and Codex are the **harnesses** that run a chat.

Use `cmd-k` for the complete command list and current shortcuts. This guide covers the workflow and behaviors that matter when using it. See the [README](../README.md) for installation, [Git workflows](git-feature.md) for the Git artifact, and [Language intelligence](language-intelligence.md) for editor tooling.

## Get started

Shark needs `git`, `gh` logged into GitHub, and `claude` on your login shell's PATH. Codex models also need `codex`, logged in with `codex login`. Home shows missing tools and a setup checklist.

1. **Add repo** from `cmd-k` or Settings › Repositories. Repositories are registered once and can serve many agents and tasks.
2. Choose **New agent** in the sidebar and give it a name. Its page opens with editable instructions and a **Defaults** row for repositories and other settings. For example, Designer might use your web repository with “Always use Mobbin to ground your research.”
3. On Home, choose the agent, describe the work and press `enter`. The first line becomes the task's title. `@designer fix the onboarding spacing` selects the same agent.
4. Plan or work in its chat, review the changes with **Differ**, and use the git button to commit, open PRs and merge.
5. Delete the completed task when you no longer need its worktrees and conversations. The agent remains ready for its next task, with the completed task in History.

Choose **No agent** on Home to start a standalone task. No repository is required to begin planning; attach one before building. Unknown `@names` remain ordinary prompt text. Home's `tab` switches to **Ask Shark**, a one-off answer about using the app; that mode never starts an agent task.

Worktrees isolate branches, not the filesystem. Coding sessions can write outside their worktrees. Review the work and use plan mode when you want to approve an approach before building.

## Agents and navigation

The sidebar gives direct access to Home and each agent. An agent's header is its settings: click the icon to change it, click the name to rename it, and type into the instructions under the name. The quiet **Defaults** row sets repositories, model, thinking level, skills and the next agent under **Then**. **Repos** includes **Add repository…**; **Skills** includes **Add skills…**. **Inherit** uses repository or app settings. These controls save defaults for future tasks; controls inside the composer affect only its draft. Each change saves immediately. The header's **…** and a right-click on the agent in the sidebar offer New task, Rename, Change icon, Edit instructions, Add skills, Open skills folder, Refresh skills, Open in… and Delete agent. Its slug is stable even when its name changes. Below the header are the task prompt, the agent's tasks and its history.

Type into an agent page's composer and press `enter` or `cmd-enter` to start; `shift-enter` adds a line. `cmd-n`, `i` and `a` focus that composer. Each agent keeps its text, attachments, model overrides and Plan setting as a draft. Elsewhere `cmd-n` opens an empty standalone task. Home uses the same composer, with an agent picker first in its footer. The picker and leading `@slug` share the same selection and preserve the prompt's body. A leading `@` on Home lists agents; `@` elsewhere lists committed files, `#` lists PRs and `/` lists skills. `up`/`down` move through completions and `tab` or `enter` picks one. A cancelled or failed creation keeps the draft.

Use the composer's **+** to add Linear issues, repositories, a branch or PR, or files. These choices add removable pills without starting the task, so an issue and an existing branch or PR can be combined. Repository pills begin with the agent's defaults; edit their branch or remove them for this task. The model chip shows the value that will run and offers a reset after an override. `cmd-/` opens its menu, and `shift-tab` toggles Plan. Without repositories, Plan stays on. **Start** also works with empty text: an attached issue supplies its planning request; otherwise the new task opens an empty chat. Home's `tab` still toggles Ask Shark when no completion is open; Ask hides the task controls.

Several tasks can run for one agent, including in the same repository. Each has separate worktrees. Chats receive a sibling note listing the agent's other open tasks and their changed files, refreshed after turns. The note lists at most 20 files per task, plus a count of others. It is context: each task can freely edit its own copies of those files.

Agent instructions reach Claude through the task's `CLAUDE.md` and Codex through `AGENTS.md`. Open tasks refresh shortly after you stop typing the instructions; existing chats receive changed instructions with their next normal message. A failed refresh is reported so you can retry. New chats prefer the agent's valid model and thinking defaults, then repository settings and the app's defaults.

History keeps the latest 100 deleted tasks, with their titles, PR links, merge status and dates. **PR state unknown** means Shark lacked enough reliable status to determine the result for the whole task, even if some repositories had known PR states. Deleting an agent requires finishing or deleting its open tasks first; confirmation then removes its instructions, history and skills folder.

### Handing a task to another agent

Set an agent's **Then** chip to its usual next agent. When its current chat finishes a turn, **Hand off to <agent>** appears above the composer; click it or press `cmd-alt-enter`. **Other…**, the task and chat-tab **Hand off to…** menus, and the command palette let you choose another agent. Dismiss the bar with ×; it returns after the next turn.

The task keeps its folder, branches, worktrees, issues and earlier chats. It moves to the receiving agent's sidebar group, updates root instructions and skill links, and opens a focused chat using that agent's model and thinking defaults. Missing default repositories attach in the background. Finish any existing repository attachment before handing off. Plan mode is still a separate toggle.

The new chat shows a removable quote of the source chat's approved plan, latest plan, or last assistant reply. Expand it to read more, remove it with ×, or send your instructions. Sending an empty composer with a quote continues from that result. The quote survives a restart and accompanies the first delivered message, including one queued while repositories attach.

Earlier chats keep their agent identity, model and thinking level. They receive a note that the task moved on instead of the new agent's instruction updates. The task folder has one set of instructions and skill links, so a restarted earlier chat can read the receiving agent's root instructions and no longer has its previous agent's skill links. Home's task tooltip shows the agent chain; History remains for deleted tasks.

### Skills

Choose **Skills › Add skills…** in the agent header to import skills:

1. Paste a GitHub repository or skill URL, or choose a local skill folder or `SKILL.md` file.
2. Inspect the discovered skills, select the ones to add, and review their instructions and requirements. Selecting `SKILL.md` imports its containing folder, including scripts, references and other supporting files; put loose downloaded files in a dedicated skill folder first.
3. Add the selected skills. Shark copies them into this agent's storage and refreshes its open tasks. Existing skills are never silently overwritten.

For example, paste `https://github.com/DietrichGebert/ponytail`, select `ponytail`, and add it. This imports the skill instructions; plugin hooks require the plugin's own installation. Importing does not execute scripts or connect external accounts. Skills that require tools or services still need those available in the chosen harness.

Adding a skill makes it available to the agent; it does not force the agent to use it on every task. Ask the agent to use the skill when needed, or include that preference in the agent's instructions. If a running harness does not discover a new skill, start a new conversation.

**Open skills folder** remains available for manual editing. The original files live at:

```text
~/Library/Application Support/shark/agents/<slug>/skills/
```

Each skill has a `<skill>/SKILL.md` file. After manual changes, choose **Refresh skills** to refresh every open task. Shark exposes agent skills to both harnesses through links in the task root. The original skill files remain in the agent's folder when a task is deleted. Imported skills record their source and, for GitHub imports, the resolved revision; they do not update automatically. A failed task refresh is reported separately from a successful import so you can retry without importing again.

**Skills from repos** in Settings › General controls repository skills only; agent skills stay enabled. Claude discovers repository skills and commands through its added repository directories. Codex receives links to repository skills from `.agents/skills` or `.claude/skills`. Agent skills get the plain names first; collisions from repositories use prefixed names. Turning repository skills off removes only Shark's recorded repository links. Skill refresh preserves user files and links it does not own.

### Home

Home starts tasks and lists every task, grouped by **Needs you**, **Running**, **Unread** and **Idle**. Needs you includes questions, plan approvals, attachment problems and actionable git status. The sidebar and Dock badge also mark work needing attention; notifications announce completed turns and tasks that need you.

The control on the first group heading switches grouping between status and task group. **Task groups…** creates and manages groups, including empty ones; task menus move tasks between groups. Deleting a group leaves its tasks intact. Agent membership remains independent of all group operations.

`cmd-0` opens Home; from Home it returns to the active task. `esc` leaves an overview for the active task, or returns to Home if none is active. The task picker (`cmd-shift-o`) searches task titles and repositories. With no query it selects the most recent task from Home, or the most recent other task while a task is open. The app switcher (`cmd-o`, held) cycles tasks until you release Command.

## Repositories, branches and setup

A task can span several repositories, normally on one shared branch name:

```text
~/shark/workspaces/<task>/{api,web}
```

Each repository gets its own git worktree. The combined diff, PR links and merge order let you treat the change as one task. Renaming a task changes its display title only; directory and branch names remain stable.

Use **+ › Branch or pull request…** on Home or an agent page to choose repositories and a branch or PR for the draft. In the branch/PR picker, `enter` selects the highlighted entry and `alt-enter` uses it as the base of a new branch; only **Start** creates the task. **New task from branch or PR…** in the command palette uses the visible composer when one is open. New branches already checked out elsewhere are refused; Shark does not reuse another task's worktree. Gitignored files matching `.worktreeinclude` are copied into new worktrees; the default pattern is `.env*`.

Repository chips in the task header show branch and attachment state. Hover for the path, base and stack position. Their menus contain PR and branch actions, **Reveal in Finder**, **Open in** and path copying. The available external apps come from those installed on your Mac; multiple repositories or projects get a picker.

### Attach a repository

Use the header's **Attach** button or **Attach repository…** in `cmd-k`. Search registered repositories, or add a local folder. `cmd-enter` marks several; `enter` attaches the marked repositories, or the highlighted one when none are marked. The dialog shows the branch and base before anything is created. **Change** (`cmd-b`) lets you pick another branch or base.

The first repository gets a branch from the task's title and unique suffix, or its primary Linear issue. Later repositories follow the task's branch where possible. If the name is taken, Shark offers a different name rather than silently reusing it. Attaching a repository never approves a plan or leaves plan mode.

Attachment work waits until the task's chats are idle and blocks new turns while pending. Before worktree creation starts, use **Cancel attachment** or **Cancel all queued** in the attachment panel to remove a queued request. Ready, queued, running, failed and waiting attachments remain visible as chips; **Show attachments** lists hidden entries too. A new repository's setup scripts run before the task continues. On failure, use **Open setup log**, **Retry** or **Continue anyway**. **Dismiss** closes the panel without resolving the failure. A repository that cannot attach is rolled back, while the remaining selection continues.

After a restart, Shark checks recorded attachments against their paths and branches. Completed work is adopted; setup does not rerun automatically. Interrupted or ambiguous work asks for your decision. **Check again** retries identification; **Stop tracking it** deletes nothing. An unresolved attachment blocks task deletion.

Chats learn about attached repositories with their next normal message, including after reopening. Slash commands leave those updates pending. New conversations read the task's root instructions.

### Repository scripts

Settings › Repositories contains default model settings and setup, run and archive scripts. Reorder repositories there to change the order their scripts start. **Run** in the task header, or `cmd-r`, starts or stops run scripts. Its menu opens output and script settings.

Scripts receive `SHARK_TASK` and `SHARK_TASK_PATH`. Existing `SHARK_WORKSPACE` and `SHARK_WORKSPACE_PATH` variables remain available with the same values. A failing archive script keeps the worktree during ordinary deletion; force deletion can continue past it.

## Chats and plans

Mermaid diagrams render in Markdown replies and panes once their code fence closes; use Copy for the source or Open for the full-size image.

A task can have several chat tabs, each with its own session, model, transcript and queue. Chats within the same task share its worktrees; tasks of the same agent do not. `cmd-t` starts another chat. Closing a running chat asks before stopping it. **Reopen closed chat…** or `cmd-shift-t` restores a recent session; Shark keeps the latest 20 closed chats. **Start a new conversation** resets the shown chat's model session.

Choose a model and thinking level with the composer button or `cmd-/`. Defaults apply to new chats; changing one chat does not replace the others. Changes take effect with the next message, restarting and resuming a Claude process where necessary. Codex's **Fast** mode (`cmd-shift-f`) selects its priority service tier and uses more of your plan's allowance.

`enter` sends a message. While a turn runs, another message joins the queue above the composer. Queued messages proceed after a successful turn; a stop or error leaves them waiting. Edit, remove or reorder them before sending. **Send now** (`cmd-enter`) steers the current turn. The queue is kept only in memory.

`ctrl-c` interrupts the shown chat. For Claude, the process normally stays warm; another interrupt stops it, and a stuck turn is terminated after a delay. Interrupting one chat does not stop the task's other chats. `cmd-q` warns while agents work: hold it or press it again within a second to quit. The menu’s **Quit** action quits without that warning.

### Plan before building

Toggle **Plan** with `shift-tab`. The model reads and proposes an approach without building. Every proposed plan is saved as a version; the plan pane shows additions, removals and changed code blocks. Reply in chat to revise it, step through versions, then choose **Approve and build**. **Build in new chat** starts a separate chat with the plan as its first message.

Tasks without repositories stay in plan mode. Building is also blocked while attachments or required setup are unresolved. Attaching a repository does not count as approval. A mode change during a turn applies from the next message.

When the model needs an answer, a question card presents its choices and a freeform alternative. Numbers select choices; `space` toggles multi-select answers; `enter` proceeds. **Skip** leaves a question unanswered. Closing the card tells the model you will answer in chat and ends that turn. The transcript keeps the question and response.

Claude's **Ask a side question** (`cmd-shift-enter` on a draft) answers from the current conversation without tools or adding to its transcript. It works during a turn. Only the latest answer is retained; Codex chats do not offer this feature.

### Files and references in chat

Drop or paste files into the composer to attach them. Outside files are copied into the task's `.context/attachments/`; files already inside the task are referenced in place. Outside folders are referenced by absolute path. Deleting the task removes its copied attachments.

Type `@` to mention a repository file or folder, `/` for skills and commands, and `#` to find a PR across the task's repositories. A PR number, URL or title can identify it. References help the model find material; they do not attach another repository to the task.

## Review and edit

**Differ** (`cmd-shift-d`) opens the task's combined diff and changed-files tree. The default comparison is against the base branch; **Uncommitted only** compares against `HEAD`. Untracked files are included. Changed images appear side by side and supported videos play inline; unsupported media opens in an external app.

Click a file in the tree, or navigate the diff with the keyboard. A dot marks uncommitted changes. Marking a file viewed records its current patch; another change marks it unviewed again. **Files** in the diff bar shows or hides the tree; the right-hand Features rail remains separate.

Press `c` on a line to comment, optionally with a suggested replacement. `cmd-enter` keeps the comment pending; `cmd-shift-enter` sends it to the shown chat immediately. **Send review** (`shift-c`) gathers selected threads and an optional overall note into one message, with a preview. Send it to this chat, another open chat or a new chat on a model you choose.

Threads track pending, queued, sent, addressed and outdated states. Addressed means the agent reported addressing it; inspect the change before accepting that report. Removing a queued review returns its threads to pending. The review bar can resolve addressed threads, requeue outdated ones or discard pending comments.

You can also publish comments to the repositories' open GitHub PRs as reviewer notes or Comment reviews. Pushed lines that can be anchored appear inline; other comments quote the relevant code. Shark warns before sharing unpushed lines. Publishing sends the selected material to GitHub.

### File editing

`cmd-p` opens any tracked or untracked repository file in a full-width tab. Command-click keeps additional files open. Press `i` in a diff to edit its working-copy lines, or open the full editor for files unsuitable for inline editing. `cmd-s` saves. Unsaved edits require saving or discarding before the tab or task is left.

The editor follows disk changes when clean and refuses to overwrite a file that changed on disk without resolving the conflict. Right-click a changed file and choose **Discard uncommitted changes…** to restore its last committed version after confirmation; discarding a new file deletes it. This also accounts for staged changes and unsaved editor edits, and waits for running chats and saves.

Drag Chat, Diff, Git and File tabs into order. Each task remembers its tabs and restores files from disk. `cmd-w`, the close button or middle-click closes a tab. Closing a background tab preserves the current view. Failed file restores are reported and retried later. Optional Vim mode covers the editor, composer and lists; see [Language intelligence](language-intelligence.md) for supported editor features.

## Git, PRs and merging

The git button shows the next step across the task's repositories: commit and push, fix checks, address review, merge or delete the completed task. Its menu exposes other git actions. Steps needing judgment send a request to the selected chat's queue and preserve the composer draft. Mechanical steps run directly. A waiting state opens the relevant PR instead of starting work.

Shark refreshes git and PR state in the background, retains the last successful result on network errors, and rechecks before direct changes. Direct git writes wait until every chat in the task is idle. Merge, force push and task deletion ask first. Multi-repository merge follows the task's chosen order and stops at the first failure.

**Create or update PRs** pushes committed work and creates linked PRs for repositories with changes. Uncommitted work blocks creation. Composer text can supply the title and body: a dialog previews the title, and the draft clears after the PRs exist. **Create draft PRs** leaves them draft until **Ready for review**. After a merged PR, further commits can get a new PR.

**Git** (`cmd-shift-g`) opens the PR artifact. Choose a repository to read or edit its description, inspect reviews, post replies and resolve threads according to your GitHub permissions. Drafts stay with their PRs across task changes and restarts. Before saving a description, Shark checks for remote edits; conflicts preserve your draft. Failed reads keep the last good view. Writes are not automatically retried when their outcome is uncertain.

Review checkouts show **Fetch updates** when the remote PR changes. It moves the review branch only when the worktree is clean, on the expected branch and commit, and no merge or rebase is underway. Local edits or commits produce guidance instead of overwriting your work. Ignored-file collisions also block fetching.

### Stacked work

Start a new task from another branch or PR, use **New task on top…**, or **Insert task below…**. Each repository has its own stack; a multi-repository task can be stacked in one repository and based on the default branch in another.

Repository menus show the stack, let you open related tasks or PRs, change a base and rebase onto a parent. Inserting below pushes a new intermediate branch and retargets the existing task's PR. When a parent is rewritten or squash-merged, **Restack** reapplies only the child's own commits. A merged parent prompts retargeting to its base; a closed parent is flagged for your decision.

A stacked PR waits while its parent PR remains open; Shark refuses merging it into that parent branch. Creating its PR requires the base to exist on GitHub. Related PR links connect the repositories, but GitHub cannot provide a stack across different repositories.

### Finish and delete

**Delete remote branches after merge** is off by default. Enable it in the git menu or palette if desired; repositories with a merge queue may refuse that option. Ordinary task deletion later removes local branches whose work was merged, or branches with no commits of their own, while preserving other branches.

**Delete task** (`cmd-backspace` or `cmd-k`) confirms removal of conversations, plans, copied attachments and worktrees. Uncommitted or untracked files, locked worktrees, submodules and unexpected files in the task root can block it. Running chats must stop first. Ordinary deletion checks repositories before removing them.

**Force delete task…** discards uncommitted changes and removes the entire task root, including dirty worktrees, submodules and extra files such as user-created harness settings. It keeps branches and still refuses locked worktrees. Read its confirmation before proceeding. Agent history retains a summary, not the deleted conversation or files. Deleting a task does not delete its agent or change anything in Linear.

Ordinary deletion protects user files in harness folders. For example, `.claude/settings.local.json`, or skill links it does not own, can prevent ordinary deletion. Resolve the reported files before retrying.

## Linear

Connect in Settings › Linear using browser sign-in where available, or a personal API key with Read access. Shark reads Linear; it does not update issues. Credentials are verified and saved in macOS Keychain before replacing a working connection. `state.json` holds connection metadata, not the credential. Connection errors show **Reconnect** or **Try again**. **Disconnect** confirms and removes the credential; attached issue snapshots remain readable.

**Attach → Linear issue…** searches assigned and recent issues, identifiers such as `ENG-123`, pasted URLs or free text. `cmd-enter` marks several issues; `enter` attaches the selection. A failed request preserves the query and offers Retry. Rate limits pause searches. Attaching reference material can happen during a turn and does not send a message or change the task's title.

The first issue is primary. Its chip menu offers **Open in Linear**, **Refresh**, **Make primary** and **Remove attachment**. Refresh is explicit; a failure keeps the old snapshot. The primary issue names a new branch before the first repository is attached, but changing the primary never renames an existing branch or task.

Every chat receives changed issue text with its next normal message. It is marked as external reference material, not instructions or permission to attach repositories. Slash commands and mid-turn messages leave the update pending. Removing an issue stops including it in future context; earlier transcript entries remain.

**+ › Linear issue…**, from Home or an agent page, adds issue pills to the draft. The first issue names the task; all selected issues attach before its first turn. Keep or change the repository pills to combine an issue with new branches, existing branches or PRs. Start sends the notes you typed; with no notes it sends the issue's planning request. With no repositories, Plan stays on. Switching Linear organizations or disconnecting keeps snapshots readable but prevents refreshing issues from the previous connection. Task deletion removes local snapshots only.

## Settings, shortcuts and local data

Settings (`cmd-,`) applies changes immediately. General includes stay awake, Vim mode and repository skills. Appearance chooses light/dark/system mode, themes and installed fonts. Prompts edits the git requests sent to chats; `{repos}` and `{branch}` are filled in, and **Reset to default** removes an override. Repositories and Linear have their own pages.

The footer's **Usage** panel shows Claude and Codex allowance and reset times. Green, amber and red dots compare remaining allowance with an even burn through the window; unknown or stale readings are muted. It uses signed-in CLI accounts without sending an inference prompt. Refresh is available in the panel.

| Action | Shortcut |
|---|---|
| Commands and current shortcuts | `cmd-k` |
| Home / return to active task | `cmd-0` |
| New task; on an agent page, focus its prompt | `cmd-n` |
| Find task / cycle recent tasks | `cmd-shift-o` / hold `cmd-o` |
| New chat / close tab / reopen chat | `cmd-t` / `cmd-w` / `cmd-shift-t` |
| Next or previous tab | `ctrl-tab` / `ctrl-shift-tab` |
| Model / plan mode / interrupt | `cmd-/` / `shift-tab` / `ctrl-c` |
| Differ / Git / file picker | `cmd-shift-d` / `cmd-shift-g` / `cmd-p` |
| Run repository scripts | `cmd-r` |
| Next task needing attention | `cmd-j` |
| Sidebar rail / Zen mode | `cmd-b` / `cmd-.` |

Lists use arrows, Home/End and Enter; Vim mode adds `j/k` and `gg/G`. Home and agent task lists use `i`/`a` to reach a prompt. Pane-navigation keys (`cmd-alt-←/→`) reach the sidebar, artifact and Features rail. The palette and menus expose less frequent actions with their active shortcuts.

Shark stores state, transcripts, plans, comments and Linear snapshots under `~/Library/Application Support/shark/`. Agent skills live there too. Worktrees remain under `~/shark/workspaces/`; the older directory name and serialized `workspaces` fields are retained for existing installations. Do not move worktrees manually: their git metadata refers to their locations.

On restart, Shark restores the saved task and tabs, or Home, and resumes saved chat sessions where the harness still has them. Interrupted turns are marked interrupted. File tabs reopen from disk; queues are not restored. If a model session is unavailable, the next message starts a fresh conversation. Root `CLAUDE.md` and `AGENTS.md` describe attached repositories and current agent instructions, so both harnesses start from the same task context.

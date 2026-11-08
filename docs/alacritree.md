# Alacritree

Alacritree is a native desktop terminal built on `alacritty_terminal`, the headless PTY, VT parser and grid inside Alacritty, and drawn with egui/eframe. It adds a worktree-aware sidebar, workspaces that each keep their own sessions, and a git status panel, so one window can hold several branches of work side by side.

This document describes what Alacritree ships today. [`features.md`](./features.md) covers which of Alacritty's terminal features (selection, links, vi mode, search, hints) alacritree has. For the full set of key bindings, see [`keyboard-shortcuts.md`](./keyboard-shortcuts.md).

## Workspaces and sessions

A **workspace** in Alacritree is either the *home* workspace or a specific Git **worktree** registered in the left sidebar. Home sessions start in `[general] working_directory`, or in the directory alacritree was launched from when that is unset. Each workspace keeps an independent list of PTY-backed **sessions**, and the active session per workspace is remembered as you switch between them.

- `Ctrl+T` (or `Cmd+T` / `Cmd+N` on macOS) opens a new shell session in the current workspace. The session inherits the workspace directory as cwd.
- `Ctrl+Tab` / `Ctrl+Shift+Tab` cycle sessions within the current workspace. On macOS, `Cmd+1` to `Cmd+9`, `Cmd+Shift+]` and `Cmd+Shift+[` mirror Terminal.app.
- `Alt+Right` / `Alt+Left` jump between workspaces.
- Sessions are **not** killed when you switch workspaces. Scrollback, running commands, and PTY state survive switches between worktrees. A native session ends when you close it or quit the app. With `[integrations.herdr_core] session_backend = "herdr_core"`, a new session runs in a herdr-core core instead, so quitting leaves it running and the next launch puts it back in its workspace. On a side without `hc`, the session opens native and alacritree offers to run hc's official installer there: the PowerShell script on Windows, and the shell script inside a WSL distro, on Linux and on macOS. The popup shows the exact command, and the command runs only after you confirm. After a decline, later sessions on that side get the plain notice for the rest of the run.

### Workspace scratchpads

`Ctrl+Backtick` opens the scratchpad tab dedicated to the current workspace. If its editor tab already exists, Alacritree selects that tab; otherwise it creates and activates a new tab. Scratchpads take part in the normal tab strip, sidebar session list, session cycling, command palette, and close flow. Pressing `Ctrl+Backtick` while the scratchpad is already active closes it immediately without confirmation, because every edit has already been saved.

The first invocation creates a Markdown document in alacritree's state directory and opens it in a built-in, borderless text editor. Its padded writing area takes the terminal background and has no toolbar or save button. Every text change is written to the backing file immediately, including typing, deletion, paste, undo, and redo. A filesystem error is shown inside the pane while the in-memory text remains intact.

Switching to another tab or workspace leaves the editor state intact. Closing the scratchpad tab releases that state, and the next invocation reloads the same auto-saved file. Deleting a worktree also keeps its scratchpad document.

Files live in the `scratchpads` folder of the state directory (see [Persistence](#persistence)). Home uses `home.md`. Worktrees use a readable leaf name plus a stable path digest, so same-named worktrees cannot collide.

### Workspace tasks

`Ctrl+Shift+Backtick` (`OpenTasks`) opens a tab with the task lists for the current workspace, kept in [taskwarrior](https://taskwarrior.org) 3, and pressing it again closes the tab. Agents write the same lists with `task`, so the people and the agents working in a worktree share one checklist. The tab stays off until the integration is turned on:

```toml
[integrations.taskwarrior]
enabled = true
```

With it off, the binding does nothing and the command palette leaves the action out. `path` and `wsl_path` point at a `task` binary that is not on `PATH`, as in the other `[integrations]` tables.

Every task belongs to one taskwarrior project, and the project name says which scope holds it:

| Scope | Project |
|---|---|
| Global | `global` |
| Project | `<repo>` |
| Workspace | `<repo>.<branch>` |
| Agent session | `<repo>.<branch>.claude-<id>` or `<repo>.<branch>.codex-<id>` |

`.`, `/` and `\` inside a name become `-`, so the branch `feat/v1.2` is `feat-v1-2` and never adds a level. A session is the agent's own conversation, keyed by the harness's session id, so resuming a Claude Code or codex conversation finds its tasks again. `alacritree task scope` prints the project for the current directory and agent, and answers with no window running.

Subtasks and ordering use two user-defined attributes: `subof` holds the parent's uuid and `order` places a task among its siblings. Run `alacritree task setup` once per machine. It declares both in the taskrc on Windows and in every WSL distro, so an agent calling `task` directly stores them instead of folding them into the description. A side with no taskrc yet gets an empty one first, since taskwarrior refuses to run without one when no terminal can answer its offer to create it. With `[integrations.taskwarrior]` enabled, `alacritree doctor` reports whether each side has them.

The tab shows the global section, the project, the workspace, and one section for each agent session under the workspace, newest session first. Home shows only the global section. Clicking a section header collapses it, and the tab remembers which sections are collapsed across restarts. "hide completed" at the top leaves out every completed task whose subtasks are all completed too (`ToggleCompletedTasks`), and is remembered the same way. A task with subtasks folds from the chevron beside it and shows how many rows it hides. That fold lasts until the window closes. Siblings keep the order they are moved into, whether done or not, and ties fall back to creation time and then the task's id. A parent shows how many of its direct subtasks are done, as `(done/total)`. A started task is marked and tinted, and `[ui.tasks]` styles the tab. Long text wraps beside its checkbox. In a row:

- the checkbox completes the task, and clearing it makes the task pending again
- Enter starts a new row below, and "+ add a task" starts one at the end of a section
- Tab nests the row under the one above it, and Shift+Tab moves it back out
- Up on a row's first line moves to the row above, and Down on its last line to the row below, keeping the column
- Alt+Up and Alt+Down move the row past its neighbouring sibling
- dragging the grip left of the checkbox moves the row beside the row it is dropped on, in the same section
- Backspace on an empty row deletes the task
- right-click starts or stops the task, and indents, dedents, moves or deletes it, showing the key bound to each

Deleting a task that has subtasks deletes them too, after asking. The keys act on the row being edited, or the last one that was, which the tab highlights. They are the task actions under [Keyboard shortcuts](keyboard-shortcuts.md#tasks-tab), so they can be rebound, and they fire only while the tasks tab is focused.

The tab checks the replica stamps every second while it is shown, and exports when they differ, so tasks an agent adds appear on their own. It asks for the global list, the repository's own node, and the workspace's subtree. A worktree does not receive a sibling worktree's session tasks. Until git has named the workspace, it asks for the repository subtree. The last listing for each workspace is kept in the log directory (see [Persistence](#persistence)), so reopening the tab shows it at once while the first reload runs. Two worktrees of one repository keep separate listings. A project inside WSL reads and writes that distro's taskwarrior, through `[wsl] resident_helper` when it is on, so a call skips starting `wsl.exe`. Taskwarrior 3 has no Windows build, so a Windows project uses the default distro's taskwarrior when `task` is not on the Windows `PATH`. Linux and macOS always use the native install.

#### Tasks in a sidebar

While the integration is on, a sidebar docks the current workspace's tasks at its bottom, so an agent's progress shows without leaving the terminal. `[ui.tasks] sidebar` picks where: under the projects (`"left"`, the default), under the git status (`"right"`), or nowhere (`"off"`). A hidden sidebar hides its tasks with it, and `ToggleTasksSidebar` hides the tasks alone, remembered across restarts.

```toml
[ui.tasks]
sidebar = "right"
```

The section holds the tab's sections that have tasks, less the global list, which it shows only on Home. Each section is named below the workspace, so an agent session reads as `claude-<id>`, and carries a `done/total` count and a bar. Rows are read-only: a filled box marks a started task and a ticked one a completed task. Clicking a section header folds it in the sidebar and the tab alike. The "hide completed" button beside the heading is the tab's filter, so it hides the completed tasks in both, while the counts and bars still include them. Right-clicking the `Tasks` heading opens the workspace's tasks tab, or brings it on screen when it is already open. Drag the divider above the section to resize it, or double-click the divider to fit the section to its tasks, up to half the sidebar. `[ui.tasks.sidebar_colors]` sets every color it draws, listed in the [config reference](config-reference.md).

The section lists only while its sidebar shows it, checking stamps every second like the tab and exporting when they differ. When the workspace's tasks tab is open, both draw the same listing.

#### A task store of your own

A program you write can keep the lists instead of taskwarrior. `[integrations.tasks.command]` names it and gives one argument list per operation, the way a custom diff viewer does:

```toml
[integrations.tasks.command]
enabled = true
path = "todo-bridge"
list = ["list", "--json"]
add = ["add", "{project}", "{order}", "{parent}", "--", "{description}"]
move = ["move", "{id}", "{parent}", "{order}"]
reorder = ["move", "{id}", "--order", "{order}"]
describe = ["edit", "{id}", "--", "{description}"]
done = ["done", "{id}"]
undone = ["reopen", "{id}"]
start = ["start", "{id}"]
stop = ["stop", "{id}"]
delete = ["rm", "{id}"]
agent_guide = "Tasks live in todo-bridge. Add yours with `todo-bridge add {project} ...`.\n"
```

`list` prints the pending and completed tasks as one JSON array, in the shape `alacritree-tasks.json` describes. That schema is attached to every release beside the config schema, and `schema/alacritree-tasks.json` in this repository holds the current one. alacritree keeps the tasks under the nodes the tab or the hook is showing, so the program may print all of them. The other operations answer through their exit status: 0 means done, and anything else puts the program's standard error on the row. `{parent}` is empty for a task at the top level. Each placeholder fills exactly one argument, since no shell splits the words. An operation left empty is one the store cannot do, and the tab says so when it is asked for.

With the command enabled, the tab turns on without `[integrations.taskwarrior]`, and `alacritree hook` reads the lists through the command. `agent_guide` is what an agent reads above its lists, with `{project}` standing for the node it writes its own tasks to. For a project inside WSL, the command runs in that distro: `wsl_path` as written, or the file name of `path` through the distro's login shell.

#### Agent hooks

`alacritree hook <event> --harness <claude|codex>` is the command an agent's hook config runs. Its `session-start` and `user-prompt-submit` events give an agent its lists when a session starts, and again before a prompt whenever they changed. Each prints one JSON object for the harness to add to the model's context, or nothing, and always exits 0, so a missing `task` never blocks a turn. The agent sees its own session's list and every scope above it, never another agent's.

Claude Code, in `settings.json`:

```json
{
  "hooks": {
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "alacritree hook session-start --harness claude" }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "alacritree hook user-prompt-submit --harness claude" }] }]
  }
}
```

codex, in `hooks.json` next to its `config.toml`:

```json
{
  "hooks": {
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "alacritree hook session-start --harness codex" }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "alacritree hook user-prompt-submit --harness codex" }] }]
  }
}
```

Add `Stop` too and an agent that finishes with [temporary worktrees](#temporary-worktrees) still open is asked about them before its turn ends. It is asked once for each set of them, so one it means to keep is not nagged about at every stop. The command is `alacritree hook stop --harness <claude|codex>`, in the same shape as the two above. Update alacritree before adding it: an older binary rejects `stop` with exit status 2, which Claude Code reads as a request to keep going.

#### Agents' worktrees

Three more events keep the worktrees agents make inside alacritree, so each one gets the [create flow](#creating-a-worktree), shows in the sidebar and runs the [checkout hooks](#checkout-hooks):

- `worktree-create` makes the worktree Claude Code asks for, from `claude --worktree`, the `EnterWorktree` tool or an isolated subagent, the way `alacritree worktree create` does. It goes through the running window, or straight to git when none is listening. The branch is the name Claude Code picked, cut from the project's default branch after a fetch, and stdout is the new worktree's path. On failure it exits 1 with the reason on stderr, and Claude Code gives up instead of running its own `git worktree add`. Every isolated subagent waits on that fetch and leaves a branch and a sidebar row behind. A name whose branch a linked worktree already has comes back to that worktree, so a `--worktree` session can be started again. It makes an ordinary worktree, not a [temporary one](#temporary-worktrees), since a `--worktree` session can be work someone comes back to.
- `worktree-remove` keeps the worktree. It exits 1 naming the worktree, so Claude Code reports it kept instead of removing it with its own `git worktree remove`. Delete it from the sidebar when it is done.
- `pre-tool-use` denies a shell command that runs `git worktree add` and tells the agent to run `alacritree worktree create` instead, with `--temporary` for throwaway work. It splits the command the way a shell does, so the phrase in a quoted argument, a comment or a heredoc passes, and so does a command that only mentions it. It catches the call an agent types, not a script or alias that wraps it. Every other call, and any payload it cannot read, goes on to the harness's own permission flow.

The worktree events are Claude Code's own; codex sends none, so it gets the shell check only.

Claude Code, in `settings.json`:

```json
{
  "hooks": {
    "WorktreeCreate": [{ "hooks": [{ "type": "command", "command": "alacritree hook worktree-create --harness claude" }] }],
    "WorktreeRemove": [{ "hooks": [{ "type": "command", "command": "alacritree hook worktree-remove --harness claude" }] }],
    "PreToolUse": [{ "matcher": "Bash|PowerShell", "hooks": [{ "type": "command", "command": "alacritree hook pre-tool-use --harness claude" }] }]
  }
}
```

codex, in `hooks.json`:

```json
{
  "hooks": {
    "PreToolUse": [{ "hooks": [{ "type": "command", "command": "alacritree hook pre-tool-use --harness codex" }] }]
  }
}
```

## Projects and worktrees in the left sidebar

The left sidebar (`Ctrl+B`) lists projects you have registered and, under each project, its Git worktrees.

- **Adding a project.** Drop any directory in. If it's a Git repo, Alacritree lists its worktrees, through libgit2 for a native repo and `git worktree list` for one inside WSL. If it's not, a single pseudo-worktree pointing at the directory is created so you can still spawn a shell there. `[integrations.git] enabled = false` treats every project that way.
- **Default-branch detection.** Tried in order: the branch `refs/remotes/origin/HEAD` points at, then the first of `main`, `master`, `trunk` and `develop` that exists, then `init.defaultBranch` when it names a branch the repo has. `init.defaultBranch` comes last because it says what `git init` names new repositories, so a global setting would otherwise override a repo whose real default differs. This is what the create dialog branches from and what the right sidebar diffs against.
- **Persisted state.** The list of project roots, their labels and expand/collapse state, and the sidebar visibility flags are written to `state.toml` in the state directory (see [Persistence](#persistence)). Failures are logged and ignored, so a missing or corrupt state file never crashes the app.

### Multiplexer panes

herdr and zellij are terminal multiplexers. When one is running, its panes appear in the sidebar under the worktree each pane's working directory matches, dimmed and carrying the multiplexer's icon, which says the pane belongs to it rather than to alacritree. The icons default to a ram's head for herdr and a hexagon for zellij, drawn from alacritree's bundled symbol font. `[integrations.herdr] icon` and `[integrations.zellij] icon` change them. A pane whose directory matches no worktree is listed under Home, including one whose checkout has been removed, unless that integration's `show_unmatched` is off.

The same attach, create and list work for both from outside the window. `alacritree multiplexer attach <side> <terminal-id>` and the `attach_multiplexer_pane` MCP tool name a pane by the side and terminal id `alacritree multiplexer list` reports, and both wait to reply until the session can be read. `alacritree multiplexer create` and the `create_multiplexer_pane` MCP tool open a new pane: the side defaults to the one the focused session's own pane belongs to and the workspace to the focused one, and the new pane appears in the sidebar under the workspace its directory matches, like any other. A workspace a WSL side has no path for, such as one on a network share, is refused before the multiplexer is asked, so no pane opens somewhere else under its name. Each command takes `--multiplexer` to pick one, and `--no-focus` leaves the window where it is. The same create is bindable as `NewMultiplexerPane` and reachable from the command palette (`Ctrl+K`). `AttachAllMultiplexerPanes` opens a session on every listed pane nothing holds yet, except the herdr-core panes, and leaves the user in the workspace they asked from, and `DetachAllMultiplexerPanes` ends every session attached to a pane, asking `confirm_session_detach` once for the batch rather than once per session. Neither carries a default key binding.

The command palette (`Ctrl+K`) lists panes alongside sessions. An attached one sits under *Open sessions*, named the way its sidebar row is, and one nothing is attached to sits under *Multiplexer panes*, where Enter attaches it in the workspace its working directory matched. Typing `herdr` or `zellij` brings up both kinds. The sidebar's own search (`/`) reaches pane and session titles too when `[ui] search_depth` is `"sessions"`, so a query naming one agent shows that agent rather than its whole workspace. At `"workspaces"` a query only ever matches project and worktree names, and a query naming the workspace still shows everything under it. A session the sidebar does not list has no row to match. Unless `session_display.sidebar_always` is on, a workspace's only shell is folded into its workspace row.

A workspace draws its own shell sessions first, then every pane it holds, in the multiplexer's own order. The sessions attached to a pane and the panes nothing is attached to share that order. Attaching therefore changes how a pane is drawn and never where it sits, and neither does detaching or a restart. Reordering is for alacritree's own sessions: a pane's place belongs to its multiplexer, so drag and `MoveSessionUp` / `MoveSessionDown` pass over one. Sort the panes in the multiplexer and the sidebar follows.

#### herdr

herdr is a terminal workspace manager for coding agents. It needs herdr 0.9.1 or newer, since alacritree follows herdr's event stream. A herdr row names the agent's pane title where herdr reports one, with the agent kind in front of it as context, so two agents of one kind in one checkout can be told apart. An agent with no title is named by its kind alone.

- **Panes with no agent.** `[integrations.herdr] show_panes` widens the listing from the panes herdr detected an agent in to every pane it owns, so a herdr pane running a plain shell gets a row too. Such a row is named by the pane's own title and carries no status word, since `unknown` is what herdr calls an agent it cannot classify rather than a way of saying there is none. Opening one shares herdr's view of the tab that holds it: every `herdr agent` subcommand resolves its target through the agent registry, which holds nothing for such a pane, so a direct attach is never offered for it whatever `attach` says. The unit herdr can be pointed at is the tab, so in a tab holding several panes the attach lands on the whole split. The pane is drawn, but keyboard focus sits wherever herdr last left it, and herdr's own pane chord moves it.
- **Attaching.** Enter or a click opens a session attached to that agent, and the row is replaced by the session's own row, which keeps the herdr icon so an attached agent still says where it lives. Hovering either row spells out the same sentence. It gives the state, the harness, and what herdr calls the pane, with the way out after them. Detaching is herdr's own chord, not one of alacritree's, read from herdr's `config.toml` so a rebound `keys.prefix` or `keys.detach` is what you are told. The row's `×` ends the attach and leaves the pane running under herdr, so it offers to detach rather than to close, and the agent's own row comes back. Whether it asks first is `[ui] confirm_session_detach`, a switch of its own. A detach destroys nothing, so the busy question `confirm_session_close` asks has no answer here, and turning one off says nothing about the other. A pane created from outside the window runs a shell, so its session shares herdr's view of the tab holding it, as for any pane with no agent.
- **Status.** A herdr row draws the same status marks as a native session, from alacritree's own `[ui] status_indicators` set, so a pane and a native session in the same state look the same. herdr reports blocked, working, done and idle, and the row keeps all four apart. A status alacritree does not recognise shows as unknown rather than idle. An attached agent's session row takes herdr's word too, so a dialog the pane title never mentions still reaches the sidebar.
- **Focus.** `[integrations.herdr] follow_focus` decides whether a focus change made inside herdr moves alacritree to the matching session: `"off"` never, `"herdr"` only while the active session is already showing herdr's view, and `"always"` from a plain native session too.
- **Native Windows.** herdr cannot attach a single agent there, so attaching focuses the pane in your own herdr window and shares the whole herdr session. The view resizes with the alacritree pane, and the tooltip says `shared view`. Each row still gets its own session, so every attached agent keeps its `×` and its place in the tab cycle. Every one of herdr's clients draws the same focused pane, though, so alacritree points herdr at the pane of whichever shared view you are looking at: switching to a session moves herdr's focus, in your own herdr window too. Moving that focus inside herdr rather than from the sidebar leaves the session showing a pane its row does not name, until you pick a row again. Both the focus and the session-name lookup are herdr processes, so they always run off the UI thread and the window keeps painting while herdr answers. `[integrations.herdr] attach = "session"` asks for that same shared view on every side. It is worth having where a direct attach costs you something: herdr repaints an attached pane row by row, so the terminal sees each soft wrap as a line break and a selection copies a wrapped command broken across lines, while herdr's own client knows where the wraps are.
- **Discovery.** Alacritree asks the native `herdr` on your `PATH` and each running WSL distro. Nothing is installed or started. A side with no herdr on it is asked once and then left alone, so a machine without herdr pays a single failed spawn. A side whose herdr answered, even to say its server is down, is retried, so starting herdr after alacritree brings its agents in without a restart. `[integrations.herdr] enabled = false` turns it off.

#### zellij

`[integrations.zellij] enabled = true` lists the panes of every running zellij session, re-listed every `poll_interval_ms`. zellij has no attach for a single pane, so opening a row attaches to its whole session with that pane focused. zellij panes carry no agent status. A new pane opens in `[integrations.zellij] session`, or in the one session running on that side when it is unset, and the create is refused when several are running.

#### herdr-core terminals

The native herdr-core core also runs terminals that alacritree did not create, such as the ones `hc run`, herdr-app or an agent started. Each live one appears in the sidebar as a pane, under the worktree its working directory matches or under Home. A row names the terminal by the name its creator gave it, then by its `where` label, then by its program's title. alacritree reads the list from a core that already serves and never starts a core to list it.

- **Opening.** Enter or a click takes the terminal over. The session types into it and sizes it, the same as a terminal alacritree created. Closing the session lets go of the terminal and leaves its program running, so its row comes back.
- **Watching.** The row's menu offers `Watch`, a session that shows the terminal and follows its output, and never types into it or resizes it.
- **Killing.** The row's menu offers `Kill terminal`, and `KillTerminal` does the same from the command palette or a key binding. Both ask first, since the program ends for every client attached to it. The terminal's record stays in the core after its program ends, for the client that created it to read and remove.
- **Opt-out.** `[integrations.herdr_core] enabled = false` hides these panes.

### Sub-agents

A session running Claude Code or Codex lists the sub-agents its agent has running, one level below the session's row. Each row carries the working mark and is named by the task the sub-agent was given. A workspace whose only session is folded into its row lists them under that row instead. Hovering a row adds what else the agent says about the sub-agent: for Claude Code its type, or its workflow and phase, and for Codex its nickname. Clicking one brings its session on screen. A row leaves within a couple of seconds of its sub-agent finishing or being interrupted. A Claude Code sub-agent that stops to wait on background work it started, such as a long command, stays listed until that work reports back. The rows are not sessions, so the cursor, search and reordering pass over them.

Both are read from the files each agent keeps, for a session on the native backend or in a herdr-core core alike. Each backend reports the program in the session's foreground, and that program names the agent:

- **Claude Code** registers each process in `~/.claude/sessions/<pid>.json`, so the foreground program's pid names its conversation. Its sub-agents, workflow agents included, are read from their transcripts under `~/.claude/projects/`. alacritree reads `CLAUDE_CONFIG_DIR` from its own environment, so set it where alacritree starts too if you move the folder.
- **Codex** registers no process, so a session running Codex lists the sub-agents of the thread started in the foreground program's directory, read from the rollouts under `~/.codex/sessions/`. Two Codex sessions in one directory list none, since nothing says which thread is whose. `CODEX_HOME` moves the folder, read the same way.

For a session inside a WSL distro, alacritree reads these files from the distro's home over its `\\wsl.localhost` share. A scan starts no `wsl.exe`. The distro's resident helper reports the folder once per distro. It reads `CLAUDE_CONFIG_DIR` or `CODEX_HOME` from the helper's own environment, which does not run your shell profile, and falls back to `~/.claude` or `~/.codex`. `[integrations.claude] subagents` and `[integrations.codex] subagents` turn each listing off.

### Creating a worktree

The create modal validates the proposed branch name against `git check-ref-format` rules (no whitespace, no `..`, no `~^:?*[\`, no leading `-`, no trailing `.` or `.lock`, etc.) before doing anything. Creation runs on a background thread and streams progress steps back to the UI:

1. Verify `origin` is configured.
2. Verify the base branch exists locally or on `origin`.
3. `git fetch origin <base>`.
4. `git worktree add <target> -b <branch> origin/<base>` (or local fallback).
5. Copy AI-assistant configs from the project root into the new worktree: `CLAUDE.md`, `CLAUDE.local.md`, `.claude/`, `.clauderc`, `AGENTS.md`, `.cursorrules`, `.cursor/`, `.aider.conf.yml`, `.aiderignore`, `.copilot-instructions.md`, `.github/copilot-instructions.md`, `.windsurfrules`, `.roomodes`, `.roo/`, `.codeium/`, `.continue/`. Existing destination files are left alone.
6. Run the [checkout hooks](#checkout-hooks).

`alacritree worktree create` and the `create_worktree` MCP tool run the same steps from outside the window.

Worktrees are created under `<base>/<project>-<hash>/<branch>`. `[workspace] worktree_dir` sets `<base>`, and `[[workspace.overrides]]` sets it for a single project. Unset, it is `~/.alacritree/worktrees`, in the distro's home for a repo inside WSL, so worktrees never clutter the repo's parent directory and stay grouped per app. Changing it never moves existing worktrees, because alacritree asks git where they are. The `<hash>` tells same-named repos in different locations apart. `/` and whitespace in branch names become `-`, and a numeric suffix is appended if the target already exists.

### Deleting a worktree

The delete dialog shows how many files are staged, modified and untracked, reusing the git panel's cached status when it has one, so you can see what would be lost before confirming. It won't confirm until that count is known. Sessions open in the worktree close first. Confirmation runs `git worktree remove`, with `--force` when the count found changes, and then `git branch -D <branch>`. The branch deletion is best-effort, so a detached HEAD doesn't block worktree cleanup. If git still refuses because the tree changed since the count, the dialog reopens as a forced retry. The [checkout hooks](#checkout-hooks) run after that, from the main checkout.

A worktree whose directory is already gone is pruned instead. The dialog asks whether its branch goes too, and no checkout hook runs.

### Temporary worktrees

A temporary worktree is for throwaway work, such as a build, a test run or a look at other code. It is made like any other, with the fetch, the copied configs and the checkout hooks, but it stays out of your project's list of worktrees and is removed by whoever made it.

```sh
alacritree worktree create --temporary . scratch
alacritree worktree remove <path>
```

Over MCP these are `create_worktree` with `temporary: true`, and `remove_worktree`. `create` takes any path in the checkout to make it from, and prints the new path on standard output with a reminder on standard error. `remove` also deletes the branch, as the sidebar's delete does, and refuses a tree with modified or untracked files unless given `--force`. It refuses any worktree that is not temporary, so an agent cannot remove a checkout you rely on. It leaves a session open in the worktree running, and that session's row stays until the session ends, as it does for a worktree removed outside alacritree.

**In the sidebar.** Temporary worktrees sit in a collapsed `Temporary (n)` section rather than among the project's worktrees:

- Under the session that made one, when that session has a row. The CLI and the MCP server read `ALACRITREE_SESSION_ID` to know which that is.
- Otherwise under the checkout it was made from, which is where it goes when its session closes or the app restarts, and where it starts when a checkout's only session is folded into the checkout's row. A checkout that has gone hands it to the main checkout.

Click the header, or press Enter or the arrow keys on it, to open and close it. Sections start closed after a restart. One opens by itself while the workspace on screen is inside it, and a search or filter reaches inside closed sections and shows what matches. Sections are skipped by `SelectNextWorkspace` unless a session is running inside, and their worktrees poll no PR status. The mark that says a session wants attention shows on a closed section's header.

**What an agent is told.** With [agent hooks](#agent-hooks) set up, `alacritree hook` explains all this to an agent in any repository whether or not a task store is configured, and lists the temporary worktrees that agent has made and not yet removed. Each agent sees only its own. With the `Stop` hook, it is also asked about them when it finishes.

A worktree counts as temporary because of where it lives, `<base>/<project>-<hash>/.temporary/<origin>/<branch>`, so the path alone decides how it is drawn, with or without a window. Moving one out of there makes it an ordinary worktree.

### Checkout hooks

Checkout hooks run when alacritree creates a worktree, the first time this process opens a shell in one, and after it removes one. Opening covers worktrees made outside alacritree, such as a plain `git worktree add`. An opening hook runs again after a restart, so it must be safe to repeat.

The built-in hooks run first, then your own:

- **Doppler.** When the main checkout has `doppler setup` scopes, a new or opened worktree gets the same scopes, and a removed one gives them back. Without this, `doppler run` in a worktree fails with "You must specify a project". For a worktree inside a WSL distro, the distro's own `doppler` does this, so `doppler run` inside the distro sees the scopes. Turn it off with `[integrations.doppler] enabled = false`.
- **Your own commands**, one table each under `[integrations.checkout_hooks.command]`, run in name order:

  ```toml
  [integrations.checkout_hooks.command.mise]
  path = "mise"
  on_created = ["trust", "{checkout}"]
  on_opened = ["trust", "{checkout}"]

  [integrations.checkout_hooks.command.cleanup]
  path = "forget-checkout"
  on_removed = ["{checkout}", "{main}"]
  ```

  `{checkout}` is the worktree and `{main}` the project's main checkout. Each placeholder becomes exactly one argument, however many spaces or quotes the path holds, since no shell is involved. An event whose list is empty is skipped. Creation and opening run the command in the worktree. Removal runs it in the main checkout, since the worktree is gone.

  For a worktree inside a WSL distro, the command runs in that distro, with Linux paths. Set `wsl_path` to the program there. Without it, alacritree looks up the file name of `path` through the distro's login shell and skips the hook in a distro that doesn't have it. A Windows `path` is never run inside the distro.

  The hooks are tables keyed by name rather than a list, so one can be changed or switched off by name, for example from a `-o` override, without restating the others:

  ```toml
  [integrations.checkout_hooks.command.mise]
  enabled = false
  ```

A hook that fails does not stop the ones after it. Its error shows up as a "Hook failed" step while a worktree is being created, and in the log at `warn` for opening and removal. A hook that runs past alacritree's time limit is killed and reported as timed out, with the limit in the error. Hooks read the config alacritree started with, so a new hook needs a restart.

## Git status in the right sidebar

The right sidebar (`Ctrl+G`) shows live status for the active workspace's worktree:

- Current branch (or short OID on detached HEAD).
- Staged and unstaged file lists with one-character glyphs (`A`/`M`/`D`/`R`/`?`/`!`).
- A file-level diff summary against the **merge base** with the base branch, so local-only commits still show up when the base branch hasn't moved.

Status is cached per worktree and refreshed on a throttle (`StatusCache`), so the panel stays responsive on large repos. `M`, `D` and `U` filter the lists while the panel has focus, see [Sidebar filters](keyboard-shortcuts.md#sidebar-filters).

Clicking a file opens its diff in a pane, and clicking it again closes the pane. `[integrations.diff_viewer]` picks what that pane runs: delta, tuicr for a review whose comments agents can read, or a custom command. With `section_buttons = true` each section header also gets a button that opens the whole section at once, and the `ReviewStaged`, `ReviewUnstaged` and `ReviewBranch` actions do the same from the palette or a key binding.

### Per-worktree base branch

The git panel diffs each worktree against an automatically picked base: the open PR's base branch if there is one, otherwise the project's default branch. To override it for a single worktree (e.g. a branch cut from `develop`):

- right-click the worktree in the left sidebar and pick *Set base branch*, or
- click the `vs <branch>` label in the git panel, which is hidden while the worktree is on the base branch itself, or
- bind the `SetBaseBranch` action and press it (targets the sidebar-cursored worktree when the sidebar has focus, the current worktree otherwise):

  ```toml
  [[keyboard.bindings]]
  key = "B"
  mods = "Control|Alt"
  action = "SetBaseBranch"
  ```

The picker lists local and `origin/*` branches. Picking *Auto* returns to automatic detection. Overrides persist in `state.toml` per worktree path.

## Terminal grid

Alacritree draws its grid with an instanced OpenGL shader, with the cell size computed from the configured font. It needs a GL 3 or GLES 3 context. Where the driver cannot build the shader, the grid stays blank and the error dialog says why, while the rest of the window keeps drawing. Resizing the window resizes the PTY on the fly. The terminal drains pending events on every frame and handles:

- `Title`, which sets the session's title in its tab and sidebar row. A spinner title changing to a plain one also marks an agent as done.
- `ChildExit`. By default the session closes. `[ui] hold_exited_sessions` can keep it on screen with a line naming the key that closes it.
- `Bell`, which marks the session as wanting attention.
- `PtyWrite`, which forwards bytes from terminal modes (e.g. color and size replies) back into the PTY.
- `ClipboardStore` and `ClipboardLoad`, an OSC 52 write or read, routed through the same clipboard wrapper described below. `[terminal] osc52` decides which of the two an application may use, and by default it may only write.

### Built-in box-drawing glyphs

Box-drawing, block element, legacy computing and Powerline glyphs are drawn from a vector spec (`builtin_font.rs`) rather than taken from the font file. The cells join up whatever monospace font you pick, so borders and Powerline separators tile without gaps. `font.builtin_box_drawing = false` turns it off.

### Over-wide icons

An icon outside the built-in ranges comes from whichever fallback face has it, sized to *that* face's em rather than to the cell. On a narrow cell, such as a CJK-derived face's half-width advance, a Nerd Font icon is wider than the column the terminal gave it and spills into the next one, where the next run's background paints over the part that escaped.

Such an icon is drawn across the blank cells that follow it instead, centred on the span it ends up with, up to four extra cells. This is kitty's behaviour. alacritty and Windows Terminal both let the overflow happen.

Only the private use areas grow. A letter that happens to arrive from an over-wide fallback face keeps its cell however far it overruns. Ordinary text can never move, which is why this needs no switch. `U+E0A0` to `U+E0A3` and `U+E0C0` to `U+E0C7` are held back as well, matching kitty's `narrow_symbols` default. Those marks read as part of the segment beside them, so a wider one looks wrong where a clipped one only looks cramped.

Blanks are the only cells an icon may take, since anything else is a character it would paint over. An icon wanting more room than it gets stays where it is rather than being pulled left. A blank draws nothing but its background, so it counts even when its foreground differs from the icon's, which is what lets an icon and the differently-highlighted space beside it share a run. Icons are commonly authored with exactly that trailing space (`" "`).

Two things it leaves alone. A double-width character already owns two columns and is sized to them, so it never grows. It also never shares a run, since the flags marking its second column end one. And a block cursor parked on a grown icon redraws it at its own cell while the cursor is there, so the icon shifts back for as long as the cursor sits on it.

### Clickable links

URL detection mirrors Alacritty's default URL hint:

- **OSC 8 hyperlinks** take priority over regex matches, because they carry an explicit URI that may differ from the visible text.
- **Regex matches** use exactly Alacritty's URL pattern (`ipfs:`, `ipns:`, `magnet:`, `mailto:`, `gemini://`, `gopher://`, `https://`, `http://`, `news:`, `file:`, `git://`, `ssh:`, `ftp://`).
- **Post-processing** strips trailing punctuation and unbalanced brackets so a URL embedded in prose (`see (https://example.com).`) opens at the right bound.

Clicking a recognised link hands it to the OS handler: `xdg-open` on Linux/BSDs, `open` on macOS, `cmd /c start` on Windows.

### Clipboard

Two clipboards are distinguished:

- **System clipboard.** `Ctrl+Shift+C` / `Ctrl+Shift+V` (also `Cmd+C` / `Cmd+V` on macOS).
- **PRIMARY selection** on Linux. A finished mouse selection is copied to it, and `Shift+Insert` or a middle click pastes it. arboard's `SetExtLinux` / `GetExtLinux`, backed by `wayland-data-control`, make it work on X11 and Wayland. Platforms without a separate PRIMARY fall back to the system clipboard.

OSC 52 writes and reads go through the same wrapper.

`[ui.paste]` lets Paste handle a clipboard with no text. Text always wins: a clipboard carrying both text and an image pastes the text. Only the regular clipboard is checked for files and images. The X11 PRIMARY selection is text, so middle-click paste is unchanged. With `files` and `image` both `false`, a paste with no text does nothing. A default Unix image directory is private to its owner (`0700`), and generated files are `0600`. A directory named with `image_dir` keeps its existing sharing permissions and is never swept, while the generated image files remain `0600`. A pasted path is quoted and, inside a WSL session, translated exactly as a dropped one. Both follow `[ui.drop]`'s `quote` and `wsl_translate`, so those keys still apply even with `[ui.drop] enabled = false`. `quote = "posix"` is the only mode that makes an arbitrary filename inert.

### Drag and drop

Dropping files on the window does something different per region:

- **Terminal grid.** The paths are *pasted*, quoted for the shell, joined by spaces, with a trailing space. It is a paste, not typing: bracketed paste is honoured, and any path carrying a control character is left out of the payload (see below), so what arrives is a command line waiting on your Enter. Dropping an image on a session running Claude Code gives you `[Image #N]`, because Claude Code resolves a pasted image path.
- **Projects sidebar.** A folder is added as a project, and a file adds the folder containing it. Several files from one folder add it once.
- **Scratchpad tab.** The plain path is inserted at the cursor, one per line and unquoted, since a document is not a command line. Dropping mid-line puts the block on lines of its own rather than welding it to the text either side.

Paths dropped into a WSL session are rewritten to their distro-side spelling (`C:\pics\a.png` becomes `/mnt/c/pics/a.png`) and quoted POSIX-style, because a `C:` path resolves to nothing inside a distro. That POSIX quoting applies even when `quote` names another mode, because `windows` quoting fed to `bash` is broken by construction. A `\\wsl.localhost\<distro>\...` path is only rewritten when it belongs to the distro the session is running in. One from a *different* distro is pasted as-is, because the same Linux path means a different file there.

Filenames carrying any control character are skipped. An application that has not enabled bracketed paste cannot tell a paste from typing, and a line editor acts on those bytes as commands: a newline arrives as Enter, and readline accepts the line on `Ctrl-O` as well. Quoting is no defence, because the editor binds the byte before the shell ever parses the quotes around it.

While files hover, the region that would receive them is tinted.

The sidebar target and the tint work on Windows only: winit reports no cursor position during a drag on other platforms. Drops still reach the terminal or the scratchpad there, chosen by which tab is active.

### Images

Alacritree draws images sent with the kitty graphics protocol. A program decides whether to send any before it sends the first one, and each program decides differently. Every pane carries `TERM_PROGRAM=alacritree` and `TERM_PROGRAM_VERSION` for programs that go by the environment, and alacritree answers the kitty `a=q` query for programs that ask. `TERM` is left alone, because a kitty or Ghostty value would claim capabilities alacritree does not have. `[env]` can override both variables, as it overrides `TERM`.

Where the program runs changes what reaches it:

- **Linux.** Replies reach the program, and the PTY reports the pane's size in pixels.
- **WSL on Windows.** Replies to the program's queries reach it unchanged. The pseudoconsole carries no pixel size, so the program reads 0 for the pane's width and height in pixels.
- **Native Windows programs.** Image output reaches alacritree unchanged. A reply reaches the program only if it has put its console input in VT mode (`ENABLE_VIRTUAL_TERMINAL_INPUT`). Otherwise the pseudoconsole drops the kitty reply, turns its closing `ESC \` into an Alt+`\` key press, and drops pixel-size replies as well. A program that queries before drawing sees no answer there.

| Program | How it decides | Linux | WSL | Native Windows | What helps |
|---|---|---|---|---|---|
| `kitten icat` | kitty `a=q` query, and the pane's pixel size | yes | with `--use-window-size` | not covered | In WSL, pass the pane's size, `--use-window-size cols,rows,width_px,height_px`, since the pixel size reads 0. `--transfer-mode=stream` skips the query. |
| yazi | kitty `a=q` query | yes | yes | yes | Nothing. It asks for the cell size with `CSI 16 t` where the pixel size reads 0. `yazi --debug` prints what it detected. |
| zellij | kitty `a=q` query at client startup | yes | yes | yes | `support_kitty_graphics_protocol true` in `config.kdl`, which is the default. zellij redraws its panes' images in alacritree. |
| timg | `TERM` of `xterm-kitty` or `xterm-ghostty`, or kitty or Ghostty named in the `XTVERSION` reply. It never sends the kitty `a=q` query | with `-pk` | with `-pk` | not covered | `-pk`, or `TIMG_PIXELATION=kitty` in the shell's environment. Inside herdr it needs neither, since herdr sets `TERM=xterm-ghostty`. Inside zellij it still does, since zellij answers `XTVERSION` with its own name. It asks for the cell size with `CSI 16 t` where the pixel size reads 0. |
| herdr | its own config, no query | yes | yes | yes, at an assumed cell size | `[terminal] kitty_graphics = true`, which is the default. Natively on Windows it cannot learn the cell size and assumes 8x16 pixels. |
| Codex | `KITTY_WINDOW_ID`, or kitty, Ghostty or WezTerm named in `TERM_PROGRAM` | with `KITTY_WINDOW_ID` | with `KITTY_WINDOW_ID` | with `KITTY_WINDOW_ID` | A wrapper that sets `KITTY_WINDOW_ID` for Codex alone, below. Only its pets use images, and never inside tmux or zellij. |
| Claude Code | kitty `a=q` query, then a terminal name from `XTVERSION` or the environment, which must be kitty 0.28.0 or later or Ghostty | with the variable | with the variable | with the variable | `CLAUDE_CODE_FORCE_TERMINAL_IMAGES=1` skips the query and the name check. It draws only through kitty Unicode placeholders. |

A program inside zellij asks zellij, which answers from its own kitty support. Codex refuses to draw inside zellij or tmux. Claude Code turns images off under tmux and screen, but it reads `CLAUDE_CODE_FORCE_TERMINAL_IMAGES` first, so the variable forces images on inside tmux too. Setting it in `[env]` reaches every pane, including a tmux session started in one, so set it in the shell that starts Claude Code outside tmux if you also use tmux. alacritree started from another terminal drops the variables that terminal sets to name itself, such as `WT_SESSION` and `KITTY_WINDOW_ID`, so a program in a pane does not mistake alacritree for its host. Without that, yazi picks sixel under an inherited `WT_SESSION`.

Alacritree also draws sixel images, the format of programs such as `img2sixel`, lsix, gnuplot and matplotlib's sixel backends. They find it through attribute 4 in the DA1 reply, and `XTSMGRAPHICS` reports 256 colour registers. A sixel image draws under the text, so text printed over it shows on top and printing spaces over it leaves it in place. Clearing the screen removes it, and so does a later sixel image that covers all of its cells.

#### Answering to kitty's name

Codex draws images only for terminals it knows by name, and reads `TERM` only when `TERM_PROGRAM` is unset, so `TERM` cannot change its mind in a pane. It accepts any terminal that sets `KITTY_WINDOW_ID`, whatever the value. A shell wrapper sets it for Codex alone, and only when Codex runs in alacritree, since another terminal would get images it cannot draw.

Nushell:

```nu
def --wrapped codex [...args] {
    if $env.TERM_PROGRAM? == 'alacritree' {
        with-env { KITTY_WINDOW_ID: '1' } { ^codex ...$args }
    } else {
        ^codex ...$args
    }
}
```

Bash and zsh:

```sh
codex() {
    if [ "$TERM_PROGRAM" = alacritree ]; then
        KITTY_WINDOW_ID=1 command codex "$@"
    else
        command codex "$@"
    fi
}
```

PowerShell:

```powershell
function codex {
    $set = $env:TERM_PROGRAM -eq 'alacritree' -and -not $env:KITTY_WINDOW_ID
    if ($set) { $env:KITTY_WINDOW_ID = '1' }
    try { & (Get-Command codex -CommandType Application | Select-Object -First 1) @args }
    finally { if ($set) { Remove-Item Env:KITTY_WINDOW_ID } }
}
```

To have every program in every pane take alacritree for kitty, set the variable in `alacritree.toml` instead:

```toml
[env]
KITTY_WINDOW_ID = "1"
```

yazi then detects kitty rather than alacritree, and a tmux or zellij session started in a pane inherits the variable. In kitty the value names a window for kitty's remote control, which alacritree does not answer, so a tool that reads the variable as a sign it can drive kitty through `kitten @` fails in a pane.

## Input and key bindings

Input handling is layered:

1. **Key bindings**, parsed from `[[keyboard.bindings]]` in the TOML config. Alacritty's default set is preloaded, and so are alacritree's own (sidebar toggles, workspace switches, session spawn / cycle, palette). Your entries are checked first, so any default can be overridden, forwarded to the terminal (`action = "ReceiveChar"`), or consumed without an action (`action = "None"`).
2. **Modal keys and navigation keys**: Enter and Escape in a dialog, and the arrows, Enter, Escape and `/` in the sidebars and the palette. These are not bindings and cannot be rebound.
3. **Egui text events**, preferred for printable input because they handle dead keys and IME correctly. Control bytes (`Ctrl-<letter>`), CSI sequences for arrows / function keys, and `ESC + key` for `Alt+<key>` are derived directly from `egui::Event::Key`.

Alacritree has no vi mode, search mode or hints. A binding with a `mode` is dropped when the config loads, and a binding naming an action alacritree does not implement consumes its key and logs the name at `debug`.

The full action list, defaults, and customisation examples live in [`keyboard-shortcuts.md`](./keyboard-shortcuts.md).

## Fonts

The terminal font comes from the system, through fontconfig on Unix and `fontdb` elsewhere, and is registered with egui at startup. The default size matches Alacritty's 11.25 pt. Change it with `font.size`, since the font size actions do nothing at runtime. Bold, italic, and bold-italic faces can be picked independently in `font.normal` / `font.bold` / `font.italic` / `font.bold_italic`. Per-cell `offset` and per-glyph `glyph_offset` tuning is supported, again to match Alacritty's config. `[font] fallback` lists faces to try for glyphs the main font lacks, and `[font] color_glyphs` draws color emoji.

### `builtin_symbols`

Alacritree bundles a small font carrying only the glyphs it paints itself, such as `⌂`, `⌫`, `⇅` and `✓`, registered as the **last** entry in each chrome font family.

Because it is last, a font that already has a glyph keeps rendering it. Your font choice is unaffected, and the bundled face is reached only where the alternative was an empty box.

Set `false` to skip it entirely.

```toml
[ui.font]
builtin_symbols = false
```

A font can claim a codepoint in its character map and still have nothing to draw for it. egui takes the first font that claims a glyph, so such a font wins over the bundled one and the box stays empty. This is uncommon, and no ordering fixes it.

Run `alacritree --licenses` for the bundled font's licence.

## Configuration

Two TOML files are loaded and **deep-merged using Alacritty's own merge semantics**: arrays concatenate (so `[[keyboard.bindings]]` in `alacritree.toml` *adds to* upstream bindings rather than replacing them), tables merge recursively, primitives replace.

Search path, the same as Alacritty's:

1. `$XDG_CONFIG_HOME/alacritty/alacritty.toml`
2. `$XDG_CONFIG_HOME/alacritty.toml`
3. `~/.config/alacritty/alacritty.toml`
4. `~/.alacritty.toml`
5. `/etc/alacritty/alacritty.toml`

On Windows the only location is `%APPDATA%\alacritty\alacritty.toml`. `alacritree.toml` is looked up in the same places. `--config-dir <dir>` reads both files from that directory instead, and `-o 'key=value'` overrides a value on top of both.

The two-file split keeps shared options (palette, cursor, scrolling, shell, key bindings) in `alacritty.toml`, which both the upstream alacritty terminal and Alacritree can read. Alacritree-only options live in `alacritree.toml`, under `[ui]`, `[workspace]`, `[integrations]`, `[wsl]` and a few keys in `[general]`, `[font]` and `[debug]`:

```toml
[ui]
sidebar_accent  = "#6a9fb5"
upstream_status = true

[integrations.gh]
pr_status = true

[workspace]
worktree_dir = "~/dev/worktrees"
```

The [configuration reference](config-reference.md) lists every key with its type, default and what it does, including which file it belongs in. It is generated from the same doc comments as the JSON Schema below, so the two cannot disagree.

When the row under the projects sidebar cursor stops being drawn, because a filter hides it or its session or worktree is removed, the cursor climbs or slides to a nearby row rather than jumping to the first one. Closing a session is the one removal the cursor cannot speak for: `Ctrl+Shift+W` from the terminal and a shell exiting on its own both originate outside the sidebar, so there is no cursor to slide. Under the default `sidebar_focus = "preserve"`, the workspace falls back to its first session, whichever one closed. `"follow"` lands on the closed session's neighbour instead. That is the next one, or the previous one when the last session closed, which is the same ordinal rule the cursor slides by.

Alacritty's palette, cursor, scrolling, selection, mouse, font, window, shell, env, and binding tables are read by the same `Raw*` structs, so those parts of an existing `alacritty.toml` carry over. The structs cover the fields alacritree acts on rather than Alacritty's full schema. The JSON Schema below lists exactly what a given table accepts.

### Editor completion and validation

Alacritree publishes a JSON Schema for everything it reads out of the two files. Editors that speak the TOML language server, such as [taplo][taplo] and the [Even Better TOML][ebt] VS Code extension built on it, use it for key completion, hover documentation and validation.

Point a config at it by running:

```sh
alacritree schema init                        # the alacritree.toml in use
alacritree schema init path/to/alacritty.toml # or a specific file
```

which prepends a header naming the published schema:

```toml
#:schema https://github.com/alacritree/alacritree/releases/latest/download/alacritree-config.json
```

When the file does not exist yet, `schema init` creates it with the header and every setting commented out. `latest/download` always resolves to the newest released schema. To validate against the version you actually run, name your tag instead, as in `releases/download/v0.9.0/alacritree-config.json`. A file that already carries a `#:schema` header is left alone, so the command is safe to re-run.

`alacritree schema` prints the document to stdout if you would rather host it yourself or point at a local copy. It is also committed at `schema/alacritree-config.json` in this repository.

Two things worth knowing about what the schema does and does not do:

- **Unknown keys are not errors.** The two files are layers, and `alacritty.toml` legitimately carries keys only the real alacritty acts on, such as `[hints]`, `[bell]` and `[general] import`. Those get no completion, but they are not flagged.
- **Closed-value keys are completed.** A key with a fixed set of values, such as `confirm_session_close`, `sidebar_focus`, `search_depth` or `drop.quote`, offers its accepted spellings. A binding's `action` completes from every action alacritree implements but rejects nothing, so an alacritty-only action still validates. Cursor `shape` and `blinking`, where Alacritty accepts more than one spelling for the same value, are left unconstrained, so a working config is never marked wrong.

[taplo]: https://taplo.tamasfe.dev/
[ebt]: https://marketplace.visualstudio.com/items?itemName=tamasfe.even-better-toml

### Icon styling

Every `[ui.icons]` key, `[integrations.herdr] icon` and `[integrations.zellij] icon` takes either a bare glyph string or a table that styles it further:

```toml
[ui.icons]
upstream_gone = { glyph = "⌫", color = "#ff5555", bold = true, italic = false, size = 8 }
```

`glyph` is optional in table form. A table with no `glyph` key keeps that icon's default glyph and only applies the styling. `size` is in logical pixels, measured before `ui_scale`, and is clamped to the space the sidebar row reserves for that icon, so it cannot grow past its slot. Status markers, badges and the multiplexer icons paint in a 10 px slot with a 10 px default, so `size` on those can only shrink. The project expand/collapse arrow and the action-button icons (`add_project`, `new_worktree`, `new_session`, `remove_project`, `delete_worktree`, `close_session`, `refresh`, `reorder`) paint in a 16 px slot with a 12 px default, and the sidebar search icon has a slot of its own tied to the UI font size. Those can grow past their defaults.

### Shell launch profiles

Named launch profiles live in `alacritree.toml`:

```toml
[ui]
default_profile = "ubuntu"       # what plain new-session (Ctrl+T) uses

[[ui.profiles]]
name = "ubuntu"
program = "wsl.exe"
args = ["-d", "ubuntu"]

[[ui.profiles]]
name = "pwsh"
program = "pwsh"
args = ["-NoLogo"]
```

Launch a profile from the small **+** segment at the right end of the session tab strip (left-click: default new session, right-click: pick a profile), from a worktree row's "Open session" menu, or from the command palette. Bind one to a key with the `SpawnProfile1` to `SpawnProfile9` actions (1-indexed into the `[[ui.profiles]]` order), or right-click a project row and pin a profile as that project's shell override.

Shell selection order for a plain new session: the per-project override, then WSL auto-selection by project location, then `default_profile`, then `[terminal.shell]` or the OS default.

## Persistence

Alacritree writes to three places.

The **state directory** is `%APPDATA%\alacritree` on Windows and `$XDG_CONFIG_HOME/alacritree` (usually `~/.config/alacritree`) elsewhere, or `[general] state_dir` when set. It holds:

- `state.toml`: project roots, labels and expanded state, sidebar visibility, per-project shell overrides, per-worktree base branches, and the tasks tab's collapsed sections.
- `scratchpads/*.md`: one persistent Markdown scratchpad per workspace. Worktree deletion does not remove these notes.
- `task-hooks/`: what `alacritree hook` last showed each agent, so it only speaks up when a list changed.

The **log directory** is `%LOCALAPPDATA%\alacritree` on Windows and `$XDG_STATE_HOME/alacritree` (usually `~/.local/state/alacritree`) elsewhere, or `[debug] log_dir` when set. It holds:

- a crash log per GUI process, written while `[debug] crash_log` is on. `alacritree crashes` lists them.
- a session log per process, written only with `[debug] persistent_logging`. `--log-file <path>` writes it to that path instead.
- `tasks/`: the last task listing of each workspace, so the tasks tab opens without waiting.

Elsewhere, alacritree writes pasted clipboard images to `[ui.paste] image_dir`, or a cache directory when that is unset. On Windows, with `[ui] notifications` on, startup writes the `HKCU\Software\Classes\AppUserModelId\alacritree` key and `%LOCALAPPDATA%\alacritree\toast-icon.png`, so toasts carry alacritree's name and icon.

There is no telemetry. alacritree reaches the network only through the tools it runs. The git panel asks `gh` for the PR a branch belongs to, to pick its diff base, whenever `gh` is installed and signed in. `[integrations.gh] pr_status = true` also polls the PR of every worktree in an expanded project. Creating a worktree runs `git fetch origin`.

## Quit confirmation

`Ctrl+Q` (or `Cmd+Q` on macOS) opens a quit modal, and Enter/Escape in it are intercepted before the terminal sees them. The window's close button quits without asking.

## MCP server to drive Alacritree from an LLM

Alacritree exposes its features to LLM agents through the [Model Context Protocol](https://modelcontextprotocol.io). `alacritree mcp` runs a stdio MCP server that talks to the running app, so an agent can browse your projects and worktrees, open shells in them, type into terminals, read their output, and inspect git state. Register it with your MCP client, e.g.:

```sh
claude mcp add alacritree -- alacritree mcp
```

Tools:

| Tool | What it does |
| --- | --- |
| `list_projects` | Sidebar projects with their worktrees, branches, and default branch |
| `add_project` | Add a project by its root path; a folder that is not a git repository gets a single pseudo-worktree |
| `rename_project` | Set a project's display label, or restore the directory name |
| `remove_project` | Remove a project from the sidebar, touching no files, worktrees or branches |
| `refresh_project` | Re-scan a project's worktrees |
| `list_sessions` | All sessions: id, title, workspace, kind, size, active tab, attention flag, agent and multiplexer pane |
| `list_multiplexer_panes` | Every herdr, zellij and herdr-core pane alacritree has detected, attached or not, with the side and terminal id an attach takes |
| `attach_multiplexer_pane` | Open a session on a detected pane, as clicking its sidebar row does |
| `create_multiplexer_pane` | Open a new multiplexer pane and a session on it |
| `select_workspace` | Focus a workspace, like clicking it in the sidebar |
| `create_session` | Open a new shell session in a workspace |
| `close_session` | Close a session |
| `move_session` | Re-home a session under another worktree (`alacritree session move <session_id> <path>`); path may be anywhere inside it |
| `send_text` | Type into a terminal or insert into a scratchpad; scratchpad changes auto-save |
| `read_screen` | Read a session's screen text, cursor position, and optional scrollback |
| `read_scratchpad` | Read the auto-saved Markdown scratchpad for the current, Home, or a specified workspace |
| `git_status` | Staged/unstaged files and per-file +/- vs the default branch |
| `create_worktree` | Create a worktree + branch, same flow as the sidebar's `+` button, or a [temporary one](#temporary-worktrees) |
| `remove_worktree` | Remove a [temporary worktree](#temporary-worktrees) and its branch |
| `run_action` | Run any key-binding action by name, as if its key were pressed |

`read_scratchpad` reads the backing file directly, so it remains useful when the editor tab is closed. Because the built-in editor writes every change immediately, MCP clients see the same auto-saved contents as the editor.

Under the hood this mirrors Alacritty's IPC design, on every platform. On Unix, the app listens under `$XDG_RUNTIME_DIR/alacritree`, or under `/run/user/$UID/alacritree` on Linux when that environment variable is absent. If the runtime path cannot be created, it falls back to the system temporary directory. On Windows it uses a named pipe at `\\.\pipe\alacritree-<pid>.sock`. The path is advertised to child PTYs via `ALACRITREE_SOCKET`, so an agent running *inside* an Alacritree session targets the instance hosting it. Other clients fall back to scanning the socket directory, or can pass `alacritree mcp --socket <path>` explicitly. Set `ipc_socket = false` under `[general]` (shared with Alacritty's option of the same name) to disable the socket entirely.

### Shell integration: following the cwd

alacritree never guesses a session's directory. A session tells it, via `ALACRITREE_SESSION_ID` (exported into every session) and `alacritree session move`. Two opt-in hooks cover the common flows. Add the ones you want to your shell config.

**Sidebar follows the shell.** Report the cwd at every prompt:

```sh
# bash (~/.bashrc)
_alacritree_report_cwd() {
  [ -n "$ALACRITREE_SESSION_ID" ] || return 0
  alacritree session move "$ALACRITREE_SESSION_ID" "$PWD" >/dev/null 2>&1 || true
}
PROMPT_COMMAND="_alacritree_report_cwd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"

# zsh (~/.zshrc)
precmd_functions+=(_alacritree_report_cwd)
```

```powershell
# PowerShell ($PROFILE). Wrap your existing prompt function.
function prompt {
  if ($env:ALACRITREE_SESSION_ID) {
    alacritree session move $env:ALACRITREE_SESSION_ID "$PWD" *> $null
  }
  "PS $PWD> "
}
```

alacritree rejects paths outside any known worktree and the hook ignores the error, so `cd /tmp` moves nothing.

**Shell follows the sidebar.** When an agent moved the session (e.g. via the `move_session` MCP tool), land the shell there at the next prompt. Only the shell can change its own cwd, which is why this is a hook and not an app feature (requires `jq`):

```sh
# bash (~/.bashrc)
_alacritree_follow() {
  [ -n "$ALACRITREE_SESSION_ID" ] || return 0
  local ws
  ws=$(alacritree session list --json 2>/dev/null | jq -r --arg id "$ALACRITREE_SESSION_ID" \
    '.sessions[] | select((.id | tostring) == $id) | .workspace // empty')
  [ -n "$ws" ] || return 0
  case "$PWD" in "$ws"|"$ws"/*) ;; *) cd "$ws" ;; esac
}
PROMPT_COMMAND="_alacritree_follow${PROMPT_COMMAND:+;$PROMPT_COMMAND}"

# zsh (~/.zshrc)
precmd_functions=(_alacritree_follow "${precmd_functions[@]}")
```

Both hooks cost one local-socket round trip per prompt. Running both at once is fine, because `_alacritree_follow` only `cd`s when the session's workspace points outside the current worktree, so it doesn't fight `_alacritree_report_cwd` over ordinary subdirectory moves within the same worktree. If you install both, `_alacritree_follow` must run before `_alacritree_report_cwd` in the same prompt (as shown above with `PROMPT_COMMAND`/`precmd_functions` prepending), so the follow hook `cd`s into an agent-moved workspace before report-cwd stamps the session with the (otherwise stale) `$PWD`.

# Integrations

Alacritree works with other tools through integrations, grouped by what they do. Each one has a table under `[integrations]` in `alacritree.toml`, and the linked [config reference](config-reference.md) section gives its keys and defaults. [`docs/alacritree.md`](alacritree.md) covers how each one behaves in the app.

Integrations that run a program take `path` for the program on Windows or natively and `wsl_path` for the program inside a WSL distro. An empty `wsl_path` finds the program by name through the distro's login shell.

## Version control

- [git](https://git-scm.com) reads repositories and their worktrees, and runs the git commands Alacritree spawns. With it off, every project is a plain folder. [`[integrations.git]`](config-reference.md#integrationsgit)

## Forges

- [GitHub CLI](https://cli.github.com) looks up each branch's open pull request. It drives the PR badges in the sidebar and the [base branch](alacritree.md#per-worktree-base-branch) the git panel diffs against. [`[integrations.gh]`](config-reference.md#integrationsgh)

## Diff viewers

Clicking a file in the [git panel](alacritree.md#git-status-in-the-right-sidebar) opens its diff in the viewer `[integrations.diff_viewer] preset` picks. [`[integrations.diff_viewer]`](config-reference.md#integrationsdiff_viewer)

- [delta](https://github.com/dandavison/delta) pages git's diff. [`[integrations.delta]`](config-reference.md#integrationsdelta)
- [tuicr](https://github.com/agavra/tuicr) opens a review whose comments agents can read. [`[integrations.tuicr]`](config-reference.md#integrationstuicr)
- A program of your own, run either as git's pager or as a viewer that renders the diff itself. [`[integrations.diff_viewer.custom]`](config-reference.md#integrationsdiff_viewercustom)

## Multiplexers

Panes of a running multiplexer appear in the sidebar under the worktree their directory matches, and opening one attaches a session to it.

- [herdr](https://github.com/herdrdev/herdr) lists the coding agents it manages, with their status. See [herdr agents](alacritree.md#herdr-agents). [`[integrations.herdr]`](config-reference.md#integrationsherdr)
- [zellij](https://zellij.dev) lists the panes of every running session. Opening one attaches to the whole session with that pane focused. [`[integrations.zellij]`](config-reference.md#integrationszellij)

## Task lists

The [tasks tab](alacritree.md#workspace-tasks) shows the lists for the current workspace, and `alacritree hook` hands them to agents.

- [taskwarrior](https://taskwarrior.org) keeps the lists, and agents write them with `task`. [`[integrations.taskwarrior]`](config-reference.md#integrationstaskwarrior)
- A program of your own can keep them instead. See [a task store of your own](alacritree.md#a-task-store-of-your-own). [`[integrations.tasks.command]`](config-reference.md#integrationstaskscommand)

## Checkout hooks

[Checkout hooks](alacritree.md#checkout-hooks) run when Alacritree creates a worktree, first opens a shell in one, or removes one.

- [Doppler](https://www.doppler.com) gives each worktree the main checkout's `doppler setup` scopes. [`[integrations.doppler]`](config-reference.md#integrationsdoppler)
- Commands of your own, one table each, run in name order. [`[integrations.checkout_hooks.command.<name>]`](config-reference.md#integrationscheckout_hookscommandname)

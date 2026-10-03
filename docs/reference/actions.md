# Actions

Every action is an entry point Herdr can bind or invoke. Run one with
`herdr plugin action invoke <action> --plugin nono.sandbox`, or with
`sh scripts/run-action.sh <action>` from the plugin directory, which waits for
the result. Each action prints a [result line](result-line.md) first.

| Action | Context | What it does |
| --- | --- | --- |
| `doctor` | global, workspace, pane | Checks `nono --version`, resolves the client and server profiles, runs an escape probe inside a throwaway sandbox with the server profile, and reports whether an OpenCode host service runs and can be reached. Starts no agent. |
| `install-keybindings` | global, workspace, pane | Appends the four key bindings listed below to Herdr's `config.toml` when they are missing, then reloads the config. |
| `start-agent` | workspace, pane | Splits a pane, starts the agent's server sandbox and launches the client sandbox in the pane, with the worktree granted read-write to both. |
| `reconnect` | pane | Launches the mapped agent again in its pane, with a fresh server and the adapter's resume arguments (OpenCode: `--continue`). |
| `open-shell` | pane | Splits a pane below and opens a shell in a new nono sandbox with the server's profile (the tools' policy) and the workspace grant. |
| `stop` | pane | Stops the agent's client session (`nono stop`), waits for the pane to record the exit, and stops a server session that outlived its pane. |
| `info` | pane | Prints the mapping, the resolved agent and profile, the live nono sessions and the last verification. |
| `verify-sandbox` | pane | Verifies the running sandboxes now: every process confined, the agent's server inside the server sandbox, the client pointed at it, no connection to an unsandboxed host service. |
| `prune-mappings` | global, workspace, pane | Drops mappings whose pane is gone and whose agent and shells are not running. |
| `sandboxes` | global, workspace, pane | Opens the interactive [overlay](overlay.md). |
| `list-sandboxes` | global, workspace, pane | Lists every mapping with its nono session state and verification. |
| `forget-mapping` | pane | Drops the focused pane's mapping. Refuses while its agent or a shell runs. |

## Which pane a pane action uses

- The focused pane's mapping.
- When that pane has no mapping but the workspace has exactly one, that one,
  so an action from the pane next to the agent works.
- `reconnect`, when the mapped pane no longer exists (for example after a
  Herdr restart): a new pane next to the focused one, and the mapping moves
  there (`adoptedFrom` in the result).
- `reconnect`, when the pane does not take the typed command within four
  seconds because it is not at a shell prompt: a fresh pane (`movedTo`).

`reconnect` and `forget-mapping` refuse with `conflict` while the agent's
bridge is alive or Herdr detects an agent in the pane.

## Key bindings

`install-keybindings` adds these chords:

| Chord | Action |
| --- | --- |
| `prefix+shift+a` | `start-agent` |
| `prefix+shift+b` | `reconnect` |
| `prefix+shift+s` | `open-shell` |
| `prefix+shift+o` | `sandboxes` |

## Hooks

| Event | What it does |
| --- | --- |
| `worktree.removed` | Stops the agents working in the removed worktree and forgets their mappings, unless `cleanupOnWorktreeRemoved` is `false`. |

## Compared with herdr-sbx-plugin

The action names, chords and result line match
[herdr-sbx-plugin](https://github.com/dirien/herdr-sbx-plugin). Its
`fetch-changes`, `open-port` and `replace-sandbox` have no counterpart: a
nono sandbox is a process, not a VM, so there is no clone to fetch from, no
port to publish and nothing to delete. `verify-sandbox` is new.

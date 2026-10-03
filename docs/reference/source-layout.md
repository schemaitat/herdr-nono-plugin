# Source layout

| Path | Purpose |
| --- | --- |
| `herdr-plugin.toml` | Manifest: actions, the `worktree.removed` hook, the overlay pane |
| `profiles/herdr-opencode-client.json`, `profiles/herdr-opencode-server.json` | The nono profiles OpenCode's client and server run under |
| `bin/run.sh`, `scripts/write-node-path.sh` | Node shim and the build step that records the path of a Node 20+ binary |
| `scripts/install-keybindings.sh` | Adds key bindings to the Herdr config and reloads it; the `install-keybindings` action runs it |
| `scripts/run-action.sh` | Invokes an action from a terminal and waits for its result line |
| `src/action.mjs`, `src/action-main.mjs` | Entry point that always prints the result marker, and the action handlers |
| `src/bridge.mjs`, `src/bridge-main.mjs` | Runs in the pane: launch under nono, verify, record the exit |
| `src/events.mjs`, `src/events-main.mjs` | The `worktree.removed` hook |
| `src/sandboxes-pane.mjs`, `src/sandboxes-pane-main.mjs` | The overlay |
| `src/lifecycle.mjs` | Launch (server and client), shell, stop, verify, describe; ownership records |
| `src/verify.mjs`, `src/procfs.mjs` | Process-tree verification over `/proc` |
| `src/hostservice.mjs` | Detection of the OpenCode host background service |
| `src/probes.mjs` | The escape probe `doctor` runs inside a sandbox |
| `src/nono.mjs`, `src/herdr.mjs` | CLI wrappers and failure classification |
| `src/agents.mjs` | Built-in adapter and custom agent validation |
| `src/context.mjs` | Herdr environment, invocation context, workspace root rules |
| `src/state.mjs`, `src/config.mjs` | Mapping store and config |
| `src/result.mjs`, `src/errors.mjs` | Result line and error kinds |
| `src/naming.mjs`, `src/shell.mjs`, `src/constants.mjs` | Session names, pane command quoting, constants |
| `test/` | `node:test` suites; `test/fakes/` holds the fake `nono`, `herdr` and `opencode` |
| `docs/` | This documentation; `mkdocs.yml` and `.github/mkdocs/` build the site |

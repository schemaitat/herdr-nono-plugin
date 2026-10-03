# Actions and scripting

Run any action with `herdr plugin action invoke <action> --plugin nono.sandbox`.
That returns as soon as Herdr has started it; `sh scripts/run-action.sh <action>`
from the plugin directory waits for the result and exits 0 when it is `ok`.

| Action | Context | Chord | What it does |
| --- | --- | --- | --- |
| `doctor` | any | | Checks nono, resolves both profiles, runs the escape probe, reports the OpenCode host service. |
| `install-keybindings` | any | | Adds the [key bindings](key-bindings.md) to Herdr's config. |
| `start-agent` | workspace, pane | `prefix+shift+a` | Starts OpenCode: server sandbox, then client sandbox in a new pane. |
| `reconnect` | pane | `prefix+shift+b` | Starts the agent again with fresh sandboxes and `--continue`. |
| `open-shell` | pane | `prefix+shift+s` | Opens a shell sandboxed with the server profile. |
| `stop` | pane | | Stops the client, then the server. |
| `info` | pane | | Mapping, profiles, live sessions, last verification. |
| `verify-sandbox` | pane | | Verifies both sandboxes now. |
| `prune-mappings` | any | | Forgets mappings whose pane is gone and that run nothing. |
| `sandboxes` | any | `prefix+shift+o` | Opens the [overlay](overlay.md). |
| `list-sandboxes` | any | | Lists every mapping with its sessions and verification. |
| `forget-mapping` | pane | | Forgets the focused pane's mapping; refuses while it runs. |

The `worktree.removed` hook stops and forgets the agents of a removed
worktree.

## Result line

The first stdout line of every action is:

```text
HERDR_SANDBOX_RESULT: {"schemaVersion":1,"plugin":"nono.sandbox","action":"start-agent","ok":true,"paneId":"w1:p2",...}
```

Strip the marker and parse JSON:

```bash
sh scripts/run-action.sh list-sandboxes | sed -n 's/^HERDR_SANDBOX_RESULT: //p' | jq '.mappings[] | {paneId, running, verified}'
```

Fields on success, besides `schemaVersion`, `plugin`, `action` and `ok`:

| Action | Fields |
| --- | --- |
| `doctor` | `nonoBin`, `nonoVersion`, `versionWarning`, `node`, `pluginRoot`, `stateDir`, `configDir`, `agentKind`, `agentBinary`, `launchArgv`, `serverArgv`, `profile`, `serverProfile`, `hostService`, `probes`, `probeError`, `warnings` |
| `install-keybindings` | `configPath`, `added`, `existing`, `warnings`, `reloaded` |
| `start-agent` | `paneId`, `sourcePaneId`, `sessionName`, `agentKind`, `localPath`, `workdir`, `profile`, `serverProfile`, `launchArgv`, `serverArgv`, `openIn` |
| `reconnect` | `paneId`, `sessionName`, `agentKind`, `mode`, `argv`, `adoptedFrom`, `movedTo` |
| `open-shell` | `paneId`, `mappedPaneId`, `sessionName` |
| `stop` | `paneId`, `sessionName`, `sessionId`, `serverSessionIds` |
| `info` | `paneId`, `mapping`, `agent`, `sessions`, `sessionError`, `verification` |
| `verify-sandbox` | `paneId`, `sessionName`, `verified`, `report` |
| `prune-mappings` | `pruned`, `kept` |
| `sandboxes` | `entrypoint` |
| `list-sandboxes` | `mappings`, `sessionError` |
| `forget-mapping` | `paneId`, `sessionName`, `removed` |

On failure: `"ok": false`, `errorKind`, `message`, and the captured CLI
`output` when there was any. `doctor` and `verify-sandbox` keep their payload.

| `errorKind` | Meaning |
| --- | --- |
| `not-found` | No such session or profile, or nothing running to stop or verify |
| `permission` | nono reported a permission error |
| `conflict` | The agent or a shell still runs, or the mapping changed meanwhile |
| `config` | Invalid `config.json`, unresolvable profile, missing agent binary, or Herdr rejected the key bindings |
| `target` | No usable pane, worktree or mapping in the context |
| `startup` | The plugin could not run, or OpenCode's server did not come up |
| `unconfined` | Escape probe got through, verification failed, or `hostServiceCheck` refused |
| `unknown` | Anything else |

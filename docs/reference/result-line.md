# Result line

Every action prints one line first on stdout:

```text
HERDR_SANDBOX_RESULT: {"schemaVersion":1,"plugin":"nono.sandbox","action":"start-agent","ok":true,"paneId":"w1:p2","sessionName":"herdr-opencode-8eaa04f412b7",...}
```

The marker is the same as herdr-sbx-plugin's. Diagnostics go to stderr.

## Common fields

| Field | Meaning |
| --- | --- |
| `schemaVersion` | `1` |
| `plugin` | `nono.sandbox` |
| `action` | The action id |
| `ok` | `true` on success |

## Fields on success

| Action | Fields |
| --- | --- |
| `doctor` | `nonoBin`, `nonoVersion`, `versionWarning`, `node`, `pluginRoot`, `stateDir`, `configDir`, `agentKind`, `agentBinary`, `launchArgv`, `serverArgv`, `profile` and `serverProfile` (each `ref`, `name`, `extends`, `egress`, `allowDomains`, `afUnixMediation`, `workdirAccess`), `hostService` (with `reachable`), `probes` (`check`, `result`, `ok`, `severity`, `why`), `probeError`, `warnings` |
| `install-keybindings` | `configPath`, `added` and `existing` (each entry has `key` and `action`), `warnings`, `reloaded` |
| `start-agent` | `paneId`, `sourcePaneId`, `sessionName`, `agentKind`, `localPath`, `workdir`, `profile`, `serverProfile`, `launchArgv`, `serverArgv`, `openIn` |
| `reconnect` | `paneId`, `sessionName`, `agentKind`, `mode` (`connect`, or `start` when the agent never ran), `argv`, `adoptedFrom`, `movedTo` |
| `open-shell` | `paneId` (the shell pane), `mappedPaneId`, `sessionName` |
| `stop` | `paneId`, `sessionName`, `sessionId`, `serverSessionIds` |
| `info` | `paneId`, `mapping`, `agent`, `sessions` (`agent`, `server`, `shells`), `sessionError`, `verification` |
| `verify-sandbox` | `paneId`, `sessionName`, `verified`, `report` (`processes`, `client`, `server`, `hostConnections`, `hostService`, `problems`, `warnings`) |
| `prune-mappings` | `pruned`, `kept` (each with `paneId`, `sessionName`, and a `reason` for kept ones) |
| `sandboxes` | `entrypoint` |
| `list-sandboxes` | `mappings` (each with `paneId`, `paneExists`, `sessionName`, `agentKind`, `localPath`, `workdir`, `lifecycleState`, `running`, `sessionId`, `serverRunning`, `shells`, `verification`, `verified`), `sessionError` |
| `forget-mapping` | `paneId`, `sessionName`, `removed` |

## Fields on failure

| Field | Meaning |
| --- | --- |
| `ok` | `false` |
| `errorKind` | One of the kinds below |
| `message` | What went wrong |
| `output` | Captured CLI output, trimmed to 4000 characters, when there was any |

`doctor` and `verify-sandbox` keep their payload on failure, so the probe
results and the report are there too.

## Error kinds

| `errorKind` | Meaning |
| --- | --- |
| `not-found` | nono does not know the session or profile, or no session is running for `stop` and `verify-sandbox`. |
| `permission` | Classified from nono's output. |
| `conflict` | The agent's bridge or a shell still runs, Herdr detects an agent in the pane, or a mapping changed meanwhile. |
| `config` | `config.json` or a custom agent is invalid, the profile cannot be resolved, the agent binary is missing, or `herdr config check` rejected the key bindings. |
| `target` | No usable pane, worktree or mapping in the invocation context, or a path the pane shell cannot quote. |
| `startup` | The plugin itself could not run (missing nono, missing Herdr directories, unreadable state), or the agent's server did not come up. |
| `unconfined` | The escape probe got through or the OpenCode host service runs (`doctor`), the verification failed (`verify-sandbox`, a stopped launch), or `hostServiceCheck` refused a launch. |
| `unknown` | Anything else; the captured CLI output is included. |

# State

Mappings live as one JSON file per pane under `panes/` in the plugin's state
directory; `doctor` prints the path. Writes go through short-lived `.lock`
files next to the mappings and are atomic.

## Mapping fields

| Field | Meaning |
| --- | --- |
| `sessionName` | `<sessionNamePrefix>-<agentKind>-<12 hex chars>`; the server's session adds `-server` |
| `localPath` | The granted workspace root |
| `workdir` | Where the agent starts |
| `agentKind` | The adapter |
| `launchArgv`, `profile`, `serverProfile` | The last launch |
| `port` | The port between client and server |
| `supervisorPid`, `serverSupervisorPid` | The nono supervisors |
| `serverLog` | `logs/<session>-server.log` in the state directory |
| `sessionId` | nono's session id |
| `lastExitCode` | Exit code of the last run |
| `lastError` | Why the last launch failed |
| `verification` | The last verification report |
| `lifecycleState` | See below |

## Lifecycle states

| State | Meaning |
| --- | --- |
| `provisional` | `start-agent` split the pane; the bridge has not acknowledged yet |
| `starting` | The bridge is starting the sandboxes |
| `running` | The client runs |
| `exited` | The client exited |
| `failed` | The launch failed or was stopped by the verification |

## Busy mappings

The bridge records its process id, with its start time so a recycled pid
never counts, while the agent runs; open shells do the same. `reconnect`,
`forget-mapping` and `prune-mappings` use these records to leave busy
mappings alone.

A nono sandbox keeps no state of its own: a session ends with its process.

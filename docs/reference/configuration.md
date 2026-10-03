# Configuration

The config file is `config.json` in the directory printed by
`herdr plugin config-dir nono.sandbox`. Unknown keys are rejected, so a typo
fails with `config` instead of silently using a default.

```json
{
  "agentArgs": { "opencode": ["--auto"] },
  "readPaths": ["/home/me/reference-docs"],
  "nonoArgs": ["--memory", "4G"]
}
```

## Keys

| Key | Default | Meaning |
| --- | --- | --- |
| `agentKind` | `"opencode"` | Which adapter to launch. Custom kinds come from `customAgents`. |
| `agentArgs` | `{}` | Per-kind argument list that replaces the adapter's default arguments. Required arguments (OpenCode's `--server http://127.0.0.1:<port>`) always come first, and copies of them here are dropped. |
| `resumeArgs` | `{}` | Per-kind arguments `reconnect` adds, replacing the adapter's (OpenCode: `["--continue"]`). |
| `agentEnv` | `[]` | `KEY=VALUE` entries set in the agent's environment. Keep secrets out; see nono's credential proxy. |
| `customAgents` | `{}` | Extra adapters, see [Custom agent fields](#custom-agent-fields). |
| `profile` | `null` | nono profile for the pane's sandbox (OpenCode's client): a name (`nono profile list`) or an absolute path. `null` uses the adapter's, for OpenCode [`profiles/herdr-opencode-client.json`](../../profiles/herdr-opencode-client.json). |
| `serverProfile` | `null` | nono profile for the server sandbox, where the agent's tools run. `null` uses the adapter's, for OpenCode [`profiles/herdr-opencode-server.json`](../../profiles/herdr-opencode-server.json). |
| `allowPaths` | `[]` | Extra absolute directories granted read-write (`--allow`). |
| `readPaths` | `[]` | Extra absolute directories granted read-only (`--read`). |
| `nonoArgs` | `[]` | Extra `nono run` flags placed before `--`, for example `["--memory", "4G"]` or `["--rollback"]`. |
| `nonoBin` | `null` | Path to the `nono` executable. Falls back to `HERDR_NONO_BIN`, then `nono` on `PATH`. |
| `silent` | `false` | Pass `--silent` so nono prints no banner or summary in the pane. |
| `shell` | `"bash"` | Shell `open-shell` starts (bash, zsh and fish are started without start-up files, which nono denies). |
| `paneDirection` | `"right"` | Where `start-agent` splits: `right` or `down`. |
| `paneRatio` | `0.5` | Split ratio between 0 and 1 for `start-agent`. |
| `openIn` | `"split"` | Where `start-agent` puts the agent: a `split` next to the focused pane, or a new `tab`. |
| `reportAgentStatus` | `true` | Announce agents without a `herdrDetectionKind` to Herdr through `pane report-agent` while they run. |
| `sessionNamePrefix` | `"herdr"` | Session names look like `<prefix>-<agentKind>-<12 hex chars>`; they show up in `nono ps`. |
| `cleanupOnWorktreeRemoved` | `true` | When Herdr removes a worktree, stop the agents working in it and forget their mappings. |
| `hostServiceCheck` | `"refuse"` | What to do when an unsandboxed OpenCode background service is running **and** the server profile lets the tools connect to localhost directly (it does not with the shipped profile): `refuse` (do not start the agent; a framed message in the pane, a toast, and `doctor` fails), `warn` (start anyway with a pane line and a toast), `off`. |
| `verifyAfterStart` | `true` | Verify every launch from outside while it starts (Linux `/proc`). |
| `onVerificationFailure` | `"stop"` | `stop` ends a session whose verification fails; `warn` only shows a toast and records the report. |

## Built-in agents

| `agentKind` | Client (pane sandbox) | Server (server sandbox) | Resume | `HERDR_AGENT` |
| --- | --- | --- | --- | --- |
| `opencode` | `opencode --server http://127.0.0.1:<port>` | `opencode serve --hostname 127.0.0.1 --port <port>` | `--continue` | `opencode` |

The port is picked per launch on the host. The password is generated per
launch and handed to both sandboxes as `OPENCODE_PASSWORD`. The server's
output goes to `logs/<session>-server.log` in the plugin's state directory.

## Custom agent fields

Each entry of `customAgents` runs in one sandbox.

| Field | Required | Meaning |
| --- | --- | --- |
| `title` | yes | Name shown in the pane title. |
| `command` | yes | Non-empty argv launched inside the sandbox. |
| `profile` | yes | nono profile name, or the path of a profile JSON file. |
| `requiredArgs` | no | Arguments placed right after `command` that `agentArgs` can never remove. |
| `defaultArgs` | no | Arguments used when `agentArgs` has none for this kind. |
| `resumeArgs` | no | Arguments `reconnect` adds. |
| `herdrDetectionKind` | no | Value of `HERDR_AGENT` in the pane command, so Herdr's screen detection recognises the agent. `null` (default) makes the bridge announce it through `pane report-agent`. |
| `serverPattern` | no | Regular expression one sandboxed process's command line must match for the verification to pass. |

## Environment variables

Set these in the environment Herdr runs plugins with.

| Variable | Default | Meaning |
| --- | --- | --- |
| `HERDR_NONO_BIN` | `nono` on `PATH` | Path to `nono`, when `nonoBin` is not set. |
| `HERDR_NONO_NODE` | the recorded path | Node binary the shim `bin/run.sh` uses. |
| `HERDR_NONO_TIMEOUT_MS` | `30000` | Limit on captured nono calls. |
| `HERDR_NONO_SERVER_READY_TIMEOUT_MS` | `30000` | How long the bridge waits for the agent's server to answer. |
| `HERDR_NONO_VERIFY_WINDOW_MS` | `20000` | How long the bridge keeps verifying a launch. |
| `HERDR_NONO_BRIDGE_START_TIMEOUT_MS` | `4000` | How long an action waits for a typed bridge command to start. |
| `HERDR_NONO_LOCK_WAIT_MS` | `5000` | How long a process waits for a mapping lock. |

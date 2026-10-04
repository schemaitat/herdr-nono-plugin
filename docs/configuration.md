# Configuration

The config file is `config.json` in the directory printed by
`herdr plugin config-dir nono.sandbox`. Unknown keys are rejected. Changes
apply to the next `start-agent` or `reconnect`.

```json
{
  "serverProfile": "/home/me/my-opencode-server.json",
  "readPaths": ["/home/me/reference-docs"],
  "nonoArgs": ["--memory", "4G"]
}
```

## Keys

| Key | Default | Meaning |
| --- | --- | --- |
| `serverProfile` | `null` | nono profile of the server sandbox, where the tools run. A name or an absolute path; `null` is the shipped one. See [Profiles](profiles.md). |
| `profile` | `null` | nono profile of the pane's sandbox (OpenCode's client). |
| `allowPaths` | `[]` | Extra absolute directories granted read-write. |
| `readPaths` | `[]` | Extra absolute directories granted read-only. |
| `nonoArgs` | `[]` | Extra `nono run` flags, for example `["--memory", "4G"]`. Network flags, `--profile` and `--allow-cwd` are rejected ([Profiles](profiles.md#what-the-plugin-grants-per-launch)). |
| `agentKind` | `"opencode"` | Which agent to launch; other kinds come from `customAgents`. |
| `agentArgs` | `{}` | Per kind, arguments replacing the defaults. Required ones (OpenCode's `--server`) always stay. |
| `resumeArgs` | `{}` | Per kind, arguments `reconnect` adds (OpenCode: `["--continue"]`). |
| `agentEnv` | `[]` | `KEY=VALUE` entries for the agent. Keep secrets out. `NONO_*` variables that nono reads as flags (`NONO_ALLOW_DOMAIN`, ...) are rejected. |
| `customAgents` | `{}` | Extra agents, see below. |
| `hostServiceCheck` | `"refuse"` | When the server profile can reach localhost (open egress, or an allowed domain such as `"*"` that covers it; not the shipped one), whether or not the OpenCode host service runs: `refuse` to start, `warn`, or `off`. |
| `verifyAfterStart` | `true` | Verify every launch from outside. |
| `onVerificationFailure` | `"stop"` | `stop` the agent when verification fails, or only `warn`. |
| `openIn` | `"split"` | Open the agent in a `split` or a new `tab`. |
| `paneDirection` | `"right"` | Split `right` or `down`. |
| `paneRatio` | `0.5` | Split ratio. |
| `shell` | `"bash"` | Shell for `open-shell`. |
| `silent` | `false` | Hide nono's banner in the pane. |
| `nonoBin` | `null` | Path to `nono`; else `HERDR_NONO_BIN`, else `PATH`. |
| `sessionNamePrefix` | `"herdr"` | nono session names are `<prefix>-<agentKind>-<12 hex>`. |
| `reportAgentStatus` | `true` | Announce agents Herdr cannot detect while they run. |
| `cleanupOnWorktreeRemoved` | `true` | Stop and forget agents of a removed worktree. |

## Agents

| `agentKind` | Client | Server | Resume |
| --- | --- | --- | --- |
| `opencode` | `opencode --server http://127.0.0.1:<port>` | `opencode serve --hostname 127.0.0.1 --port <port>` | `--continue` |

The pane command sets `HERDR_AGENT=opencode` so Herdr detects the agent. The
server's output goes to `logs/<session>-server.log` in the state directory
(`doctor` prints it).

### Custom agents

Untested. A custom agent runs in **one** sandbox, so the two-sandbox layout
and its checks do not apply. Verify the result with `verify-sandbox` and
`open-shell` before relying on it.

```json
{
  "agentKind": "claude-code",
  "customAgents": {
    "claude-code": {
      "title": "Claude Code",
      "command": ["claude"],
      "resumeArgs": ["--continue"],
      "profile": "nolabs-ai/claude",
      "herdrDetectionKind": "claude"
    }
  }
}
```

| Field | Meaning |
| --- | --- |
| `title`, `command`, `profile` | Required: pane title, argv, nono profile |
| `defaultArgs`, `resumeArgs` | Arguments for a start and for `reconnect` |
| `requiredArgs` | Arguments after `command` that `agentArgs` cannot remove |
| `herdrDetectionKind` | `HERDR_AGENT` value for Herdr's detection; `null` announces the agent instead |
| `serverPattern` | Regex a sandboxed process must match, for agents that start their own server |

## Environment variables

Set in the environment Herdr runs plugins with.

| Variable | Default | Meaning |
| --- | --- | --- |
| `HERDR_NONO_BIN` | `nono` on `PATH` | Path to `nono` |
| `HERDR_NONO_NODE` | recorded at install | Node binary for `bin/run.sh` |
| `HERDR_NONO_TIMEOUT_MS` | `30000` | Limit on captured nono calls |
| `HERDR_NONO_SERVER_READY_TIMEOUT_MS` | `30000` | Wait for OpenCode's server to answer |
| `HERDR_NONO_VERIFY_WINDOW_MS` | `20000` | How long a launch is verified |
| `HERDR_NONO_BRIDGE_START_TIMEOUT_MS` | `4000` | Wait for the pane to take the launch command |
| `HERDR_NONO_LOCK_WAIT_MS` | `5000` | Wait for a mapping lock |

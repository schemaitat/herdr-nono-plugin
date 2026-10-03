# The overlay

`prefix+shift+o`, or the `sandboxes` action, opens a full-screen view of every
agent the plugin tracks. It refreshes every three seconds.

```text
 nono sandboxes                                                              1 agent · 1 running · 7:53:28 AM
┌─ Agents ───────────────────────────────────────────────────────────────────────────────────────────────────┐
│     PANE       SESSION        AGENT     STATE    NONO                VERIFIED  DIRECTORY                   │
│ ▶●  wQ:p2      …96db19cd2fd8  opencode  running  client+server       ✔ ok      ~/projects/app              │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 client ──:46541──▶ server ──▶ nono proxy ──▶ internet  ✖ localhost ✖ Herdr ✖ systemd ✖ ssh-agent
┌─ client · no network but :46541 ────────────────────┐┌─ server · on :46541, proxy egress ──────────────────┐
│ ✔ 3243039 tui    opencode --server http://127.0.0.… ││ ✔ 3243022 server opencode serve --hostname 127.0.0… │
│                                                     ││ ✔ 3243410 tool   └ bash -c npm test                 │
│                                                     ││ ✔ 3243414 tool     └ vitest run                     │
└─────────────────────────────────────────────────────┘└─────────────────────────────────────────────────────┘
┌─ Details ──────────────────────────────────────────────────────────────────────────────────────────────────┐
│ Session     herdr-opencode-96db19cd2fd8                                                                    │
│ Pane        wQ:p2 (open)  workspace wQ                                                                     │
│ Profiles    client herdr-opencode-client · server herdr-opencode-server                                    │
│ Verified    confined: 4 processes, server pid 3243022 (7:53:20 AM)                                         │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 ✔ verified: confined: 4 processes, server pid 3243022
 ↑↓/jk  select   v  verify   x  stop   p  prune   r  refresh   q  quit
```

## Areas

| Area | Shows |
| --- | --- |
| Title bar | Agent count, running count, stale count, time of the last refresh |
| Agents | One row per mapping: pane, session, agent kind, lifecycle state, live nono sessions, last verification, granted directory |
| Flow line | The selected agent's network path and what it cannot reach |
| Sandbox boxes | The client (cyan) and server (magenta) sandboxes, each titled with its network policy from the resolved profile, listing live processes from `/proc`; tools in yellow |
| Details | Session, pane, profiles, last verification, `Last error` of a failed launch. Hidden on a short screen. |
| Status line | Result of the last key action |

A server profile that leaves localhost reachable shows up in the server's
title (`OPEN egress + localhost`) and in the flow line.

## Symbols

| Symbol | Meaning |
| --- | --- |
| `●` | Running agent |
| `○` | Idle mapping |
| `✖` (row) | Failed launch |
| `✗` after the pane | Pane is gone; counted as stale |
| `✔` / `✖` (process) | Process confined / not confined |

## Keys

| Key | Does |
| --- | --- |
| `↑` `↓` / `j` `k`, `PgUp` `PgDn` | Select an agent; the details panel follows |
| `v` | `verify-sandbox` for the selected agent; the result shows in the status line |
| `x` | `stop` the selected agent and its server, after a `y/N` confirmation |
| `p` | `prune-mappings`: forget every stale mapping that runs nothing, after a `y/N` confirmation |
| `r` | Refresh now |
| `q`, `ctrl+c` | Close |

# Sandboxes overlay

`prefix+shift+o` (the `sandboxes` action) opens a full-screen view of every
agent the plugin tracks. It is the place to see, for each agent, which client
belongs to which server, what each sandbox may reach, and which processes run
inside them. It refreshes every three seconds.

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

## Reading it

- **Agents** has one row per agent. `NONO` shows which of its two nono
  sessions are alive: `client+server` is healthy; a lone `server` is a
  leftover the next launch or `x` cleans up.
- **The flow line** is the selected agent's network path: the client reaches
  only its server's port, the server reaches the internet only through nono's
  proxy, and the `✖` targets are what neither can reach.
- **The two boxes** are the selected agent's sandboxes, each titled with the
  network policy of its resolved profile. The client box (cyan) holds the TUI.
  The server box (magenta) holds `opencode serve` and, indented below it, every
  tool process it runs (yellow). Each process is marked `✔` confined or `✖`
  not, read live from `/proc`.
- **Details** shows the session name, pane, both [profiles](profiles.md), the
  last verification and, for a failed launch, `Last error`. It hides on a short
  screen.

A server profile that leaves localhost reachable shows up in the server's
title (`OPEN egress + localhost`) and in the flow line (`! localhost open`).

## Symbols

| Symbol | Meaning |
| --- | --- |
| `●` / `○` | Running agent / idle mapping (the agent exited) |
| `✖` at the start of a row | Failed launch |
| `✗` after the pane | Pane is gone; the mapping is stale |
| `✔` / `✖` on a process | Confined / not confined |

## Keys

| Key | Does |
| --- | --- |
| `↑` `↓` / `j` `k`, `PgUp` `PgDn` | Select an agent |
| `v` | Verify the selected agent now (`verify-sandbox`) |
| `x` | Stop the selected agent and its server, after `y/N` |
| `p` | Forget every stale mapping that runs nothing, after `y/N` |
| `r` | Refresh now |
| `q`, `ctrl+c` | Close |

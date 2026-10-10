# Sandboxes overlay

`prefix+shift+o` (the `sandboxes` action) opens a full-screen view of every
agent the plugin tracks. It is the place to see, for each agent, which client
belongs to which server, what each sandbox may reach, and which processes run
inside them. It refreshes every three seconds.

```text
 nono sandboxes                                          3 agents · 1 running · 1 stopped · 1 stale · 7:53:28 AM
 ↑↓/jk  select   ⏎  details   g  jump   v  verify   x  stop   p  prune   a  clean all   i  profiles   ?  help   q  quit
 ✔ verified: confined: 4 processes, server pid 3243022
┏▶ feat-tui-rework · herdr-nono-plugin ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ running ┓
┃ opencode  ·  wQ:p2  ·  client+server  ·  ✔ verified  ·  …96db19cd2fd8                                      ┃
┃ ~/.herdr/worktrees/herdr-nono-plugin/feat-tui-rework                                                       ┃
┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
╭● docs-pass · herdr-nono-plugin ─────────────────────────────────────────────────────────────────── stopped ╮
│ opencode  ·  wQ:p4  ·  -  ·  ✔ verified  ·  …0c1e55aa3b21                                                 │
│ ~/.herdr/worktrees/herdr-nono-plugin/docs-pass                                                             │
╰────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
╭● old-spike ─────────────────────────────────────────────────────────────────────────────────────── stale ╮
│ opencode  ·  wQ:p7 ✗ gone  ·  -  ·  not verified  ·  …7be2090f4d10                                         │
│ ~/projects/old-spike                                                                                       │
╰────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
```

`enter` opens the selected agent's details over the list:

```text
┌─ feat-tui-rework · herdr-nono-plugin · running ────────────────────────────────────────────────────────────┐
│  client ──:46541──▶ server ──▶ nono proxy ──▶ internet  ✖ localhost ✖ Herdr ✖ systemd ✖ ssh-agent         │
│ ┌─ client · no network but :46541 ─────────────────┐┌─ server · on :46541, proxy egress ────────────────┐ │
│ │ ✔ 3243039 tui    opencode --server http://127.0… ││ ✔ 3243022 server opencode serve --hostname 127.0… │ │
│ │                                                  ││ ✔ 3243410 tool   └ bash -c npm test               │ │
│ └──────────────────────────────────────────────────┘└───────────────────────────────────────────────────┘ │
│ ┌─ Details ────────────────────────────────────────────────────────────────────────────────────────────┐ │
│ │ Session     herdr-opencode-96db19cd2fd8                                                              │ │
│ │ Pane        wQ:p2 (open)  workspace wQ                                                               │ │
│ │ Profiles    client herdr-opencode-client · server herdr-opencode-server                              │ │
│ │ Verified    confined: 4 processes, server pid 3243022 (7:53:20 AM)                                   │ │
│ └──────────────────────────────────────────────────────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

## Reading it

- **One box per agent.** The title is the agent's **worktree name**, the
  directory name of its workspace root, which is the label the Herdr sidebar
  shows for the worktree. For a Herdr worktree (`…/worktrees/<repo>/<name>`)
  the repository follows it. Below: the agent, its pane, which nono sessions
  are alive (`client+server` is healthy; a lone `server` is a leftover the
  next launch or `x` cleans up), the verification and the session id, then the
  directory.
- **The colour is the state**, on the border and the word at its right:
  **green** `running` (a sandbox of the agent is up), **yellow** `stopped`
  (every sandbox is down but the pane still exists, so `prefix+shift+b`
  reconnects) and **red** `stale` (sandboxes down and the pane is gone; `p`
  forgets it). The selected box has a heavy border and `▶`. A list longer than
  the screen scrolls and shows `n/total` on the status line.
- **The flow line** (in the details) is the network path: the client reaches
  only its server's port, the server reaches the internet only through nono's
  proxy, and the `✖` targets are what neither can reach.
- **The two sandbox boxes** are the agent's sandboxes, each titled with the
  network policy of its resolved profile. Each lists its **processes** (with
  how many are confined, and each one's whole command line as `ps` shows it), then its **policy**: profile, network, the **allowed
  hosts** one per line, the directories it may write and read, and socket
  mediation. The client box (cyan) holds the TUI.
  The server box (magenta) holds `opencode serve` and, indented below it, every
  tool process it runs (yellow). Each process is marked `✔` confined or `✖`
  not, read live from `/proc`.
- **Details** shows the session name, pane, both [profiles](profiles.md), the
  last verification and, for a failed launch, `Last error`. It hides on a short
  screen.

The title bar, the key hints and the status line are the first three lines, so
they stay visible however tall the pane is. A server profile that leaves
localhost reachable shows up in the server's title (`OPEN egress + localhost`)
and in the flow line (`! localhost open`).

## Profiles view

`i` opens the selected agent's
[profiles](profiles.md#see-the-active-profiles): for each sandbox the profile's
name and source (shipped, your file, a nono user profile, built into nono), its
file, what it extends, its network, its directory grants, its socket mediation
and its description. Below that: the profiles the next launch uses (they
differ from a running agent's after a `config.json` change), the `config.json`
path and nono's user profile directory. With no agent mapped it shows the
next launch's profiles. `i` goes back.

## Symbols

| Symbol | Meaning |
| --- | --- |
| green / yellow / red box | Running / stopped (pane still exists) / stale (pane gone) |
| `▶` and a heavy border | The selected agent |
| `✗ gone` after the pane | Pane is gone |
| `launch failed` | The last launch failed |
| `✔` / `✖` on a process | Confined / not confined |

## Keys

| Key | Does |
| --- | --- |
| `↑` `↓` / `j` `k`, `PgUp` `PgDn` | Select an agent |
| `enter` | Details popup: both sandboxes, their processes and the details; `enter` or `esc` closes it |
| `g` | Jump to the selected agent's pane (focuses its workspace and tab) and close the overlay |
| `v` | Verify the selected agent now (`verify-sandbox`) |
| `x` | Stop the selected agent and its server, after `y/N` |
| `p` | Forget every stale mapping that runs nothing, after `y/N` |
| `a` | Clean up all: stop every running agent and forget every mapping, after `y/N` |
| `?` | Show the key reference, with the Herdr chords; `?` or `esc` goes back |
| `i` | Toggle the [profiles view](#profiles-view) |
| `r` | Refresh now |
| `q`, `ctrl+c` | Close |

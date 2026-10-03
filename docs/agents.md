# Working with agents

Pane actions act on the focused pane, or on the workspace's only agent when
the focused pane has none, so they also work from the pane next to the agent.
Actions without a chord run with
`herdr plugin action invoke <action> --plugin nono.sandbox`.

| Task | How |
| --- | --- |
| Start an agent | `prefix+shift+a`. The plugin grants the project's worktree (or the git repository of the focused pane) and refuses your home directory and `/`. `openIn`, `paneDirection` and `paneRatio` control where the pane opens. |
| See all agents | `prefix+shift+o`, the [overlay](overlay.md); `list-sandboxes` from a script |
| Resume after OpenCode exits | `prefix+shift+b` in the agent's pane. Fresh sandboxes, same conversation (`--continue`). If the pane is gone (for example after a Herdr restart), run it from any pane of the workspace: it opens a new pane and moves the agent there. |
| Stop an agent | `x` in the overlay, or `stop`. The client stops, then its server. |
| Try a command under the agent's policy | `prefix+shift+s`. A shell pane opens below, sandboxed with the server profile, the one the tools run under. |
| Check confinement now | `v` in the overlay, or `verify-sandbox`, ideally while a tool runs |
| Inspect one agent | `info`: mapping, profiles, live nono sessions, last verification |
| Forget agents whose pane is gone | `p` in the overlay, or `prune-mappings`. Running agents are kept. |
| Forget the focused pane's agent | `forget-mapping`. Refuses while it runs; `stop` it first. |

When Herdr removes a worktree, the `worktree.removed` hook stops its agents
and forgets them (`cleanupOnWorktreeRemoved`). A nono sandbox is just a
process, so there is nothing else to clean up.

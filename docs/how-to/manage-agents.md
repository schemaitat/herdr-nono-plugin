# Manage running agents

Every task below works from a key binding, from the
[overlay](../reference/overlay.md), or from any host terminal with
`herdr plugin action invoke <action> --plugin nono.sandbox`. Pane actions act
on the focused pane, or on the workspace's only agent when the focused pane
has none.

## Start an agent

Focus a pane in the project and press `prefix+shift+a` (`start-agent`). The
plugin grants the project's worktree, see
[which directory gets granted](../explanation/design.md#which-directory-gets-granted).
To open the agent in a new tab or split downwards, set `openIn`,
`paneDirection` or `paneRatio` in the [configuration](../reference/configuration.md).

## Resume after the agent exits

Press `prefix+shift+b` (`reconnect`) in the agent's pane. OpenCode starts in
fresh sandboxes with `--continue` and picks up the last conversation.

If the pane is gone, for example after a Herdr restart, run `reconnect` from
any pane of the workspace: it opens a new pane next to you and moves the
agent there.

## Stop an agent

Run `stop` in the agent's pane, or select it in the overlay and press `x`.
The client stops, then its server.

## Open a shell under the agent's policy

Press `prefix+shift+s` (`open-shell`). A pane opens below with a shell in a
new sandbox that has the server's profile, the one the agent's tools run
under. Use it to try a command the agent failed to run, or to start the agent
by hand and read its error.

## Check that an agent is still confined

Run `verify-sandbox` in the agent's pane, or press `v` in the overlay. The
plugin walks both process trees and lists every process; any process that is
not confined fails the check. [Verification](../reference/verification.md)
lists what is checked.

## Clean up

- **Forget agents whose pane is gone:** `prune-mappings`, or `p` in the
  overlay. It keeps anything still running.
- **Forget the focused pane's agent:** `forget-mapping`. It refuses while the
  agent or a shell from it runs; `stop` it first.
- **Removed worktrees** are handled for you: when Herdr removes a worktree,
  the plugin stops its agents and forgets them (`cleanupOnWorktreeRemoved`).

A nono sandbox is a process, so there are no containers or images to delete.

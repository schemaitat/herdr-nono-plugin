# Changelog

## Unreleased

- Documentation site on GitHub Pages (MkDocs Material), organised by Diátaxis into a tutorial,
  how-to guides, reference and explanation; the README is now a short landing page.
- `justfile` with `just docs` (live preview) and `just docs-build` (strict build, as in CI).

## 0.1.0 - 2026-09-29

Initial release.

- Actions: `doctor`, `install-keybindings`, `start-agent`, `reconnect`, `open-shell`, `stop`, `info`,
  `verify-sandbox`, `prune-mappings`, `sandboxes`, `list-sandboxes`, `forget-mapping`.
- OpenCode adapter that runs a private server per pane in one nono sandbox and the TUI client in
  another, joined by a port and a password the plugin picks per launch, so every tool call runs
  sandboxed and never in the host service; `reconnect` resumes with `--continue`.
- `profiles/herdr-opencode-server.json` (egress only through nono's proxy, so no direct connects to
  localhost services such as the OpenCode host service) and `profiles/herdr-opencode-client.json`
  (network blocked): the `nolabs-ai/opencode` pack plus pathname AF_UNIX mediation, stripped Herdr
  and socket variables, and no post-exit "grant denied paths" prompt.
- Process-tree verification of every launch through `/proc`, stopping sessions that fail it
  (`onVerificationFailure`), and on demand with `verify-sandbox`.
- An escape probe in `doctor` that runs inside a sandbox with the configured profile.
- Detection of the unsandboxed OpenCode host service. When a server profile leaves localhost
  open, agents are not started while it runs (`hostServiceCheck: "refuse"`), with a framed message
  in the pane and a toast, and `doctor` fails; `warn` and `off` are opt-outs.
- An escape probe check that the tools cannot connect to the host service's port.
- Custom agents with their own command, nono profile and server pattern.
- `worktree.removed` hook that stops the agents of a removed worktree and forgets their mappings.
- An interactive `sandboxes` overlay (selectable table, details panel, keys to verify, stop and
  prune with confirmation), result marker line and stable error kinds for orchestration,
  `scripts/run-action.sh` matching its invocation by `log_id`.

# herdr-nono-plugin

One coding agent per pane, its client and its server in [nono](https://nono.sh) sandboxes, driven from Herdr.

[![Herdr plugin](https://img.shields.io/badge/herdr-plugin-76e6a3)](https://herdr.dev/plugins)
[![Requires Herdr 0.9.0 or newer](https://img.shields.io/badge/herdr-%E2%89%A50.9.0-6db8ff)](https://herdr.dev/docs/plugins/)
![Linux](https://img.shields.io/badge/platform-Linux-f2c66d)
[![Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

This [Herdr](https://herdr.dev) plugin runs OpenCode inside
[nono](https://nono.sh) sandboxes (Landlock and seccomp on Linux) and gives it a
Herdr pane. Herdr keeps its status detection and key bindings, nono does the
confinement, and the plugin is the thin layer between the two. It is the nono
counterpart of [herdr-sbx-plugin](https://github.com/dirien/herdr-sbx-plugin),
which does the same with Docker Sandboxes, and keeps its action names, key
bindings and result line.

OpenCode 2.x runs every tool call in its server, so the plugin starts a
**private server per pane in one sandbox** (internet only through nono's
proxy, no localhost) and the **TUI client in a second one** (no network but
its server's port). After every launch it checks from outside, through
`/proc`, that every process is confined, and stops the agent when not.

> **Boundary:** Linux, with `nono` 0.78 or newer and the `nolabs-ai/opencode`
> pack. Verified with Herdr 0.9.1, nono 0.78.0 and OpenCode 2.0.20 on Linux
> 7.0. **Known limitation:** nono 0.78 rate-limits the connection checks it
> makes in proxy mode when AF_UNIX mediation is on. Roughly ten back-to-back
> connections from the server sandbox go through, further ones are denied
> until the burst subsides, so bursts of downloads (`npm install`) are slowed
> by retries and a single request can fail with "could not connect to proxy".
> Spacing requests by 0.1 s avoids it. See
> [What the sandbox stops](docs/explanation/security.md#nono-rate-limits-connections-in-proxy-mode).
> macOS is untested and not declared in the manifest.

## Quick start

```bash
nono pull nolabs-ai/opencode
herdr plugin install schemaitat/herdr-nono-plugin
herdr plugin action invoke doctor --plugin nono.sandbox
herdr plugin action invoke install-keybindings --plugin nono.sandbox
```

Then press `prefix+shift+a` in a project pane. You need Linux, Herdr 0.9+,
nono 0.78+, Node.js 20+ and OpenCode 2.x.

## Documentation

**<https://schemaitat.github.io/herdr-nono-plugin/>**, organised by what you
need:

| | |
| --- | --- |
| **Tutorial** | [Your first sandboxed agent](docs/tutorials/first-sandboxed-agent.md): install, start OpenCode, watch a tool fail to escape |
| **How-to guides** | [Install](docs/how-to/install.md) · [Key bindings](docs/how-to/key-bindings.md) · [Manage agents](docs/how-to/manage-agents.md) · [Change what the sandbox may touch](docs/how-to/grant-access.md) · [Run another agent](docs/how-to/custom-agent.md) · [Drive from scripts](docs/how-to/script-actions.md) · [Troubleshoot](docs/how-to/troubleshoot.md) · [Test on a real host](docs/how-to/test-on-a-real-host.md) · [Work on the plugin](docs/how-to/develop.md) |
| **Reference** | [Actions](docs/reference/actions.md) · [Overlay](docs/reference/overlay.md) · [Configuration](docs/reference/configuration.md) · [Result line](docs/reference/result-line.md) · [Profiles](docs/reference/profiles.md) · [Verification](docs/reference/verification.md) · [State](docs/reference/state.md) · [Source layout](docs/reference/source-layout.md) · [Changelog](CHANGELOG.md) |
| **Explanation** | [Design](docs/explanation/design.md) · [What the sandbox stops, and what it does not](docs/explanation/security.md) |

## Status

Version `0.1.0`, matching `package.json` and the manifest. Verified on a real
host with Herdr 0.9.1, nono 0.78.0 and OpenCode 2.0.20 on Linux, with the
two-sandbox layout: `doctor` and its probe (`opencodeServicePort: denied`
while the host service ran), `start-agent` with automatic verification of
both sandboxes, a real prompt whose bash tool could reach neither
`127.0.0.1:4096` (through the proxy or directly) nor Herdr's socket,
`verify-sandbox`, `stop` taking the server down with the client, and public
sites (GitHub, npm, PyPI, Wikipedia, opencode.ai) through the proxy. Verified
earlier with the single-sandbox layout and carried over unchanged:
`reconnect` resuming the conversation, `open-shell`, the overlay,
`list-sandboxes`, `prune-mappings` after closing a pane, and the refusals of
`reconnect` and `forget-mapping` while the agent runs. Not verified on a real
host: the `worktree.removed` hook (tested against fakes), custom agents,
macOS.

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE). The structure and
much of the plumbing (state store, locks, shim, key binding installer, pane
handling) are adapted from [herdr-sbx-plugin](https://github.com/dirien/herdr-sbx-plugin).

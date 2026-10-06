# Getting started

## Requirements

| | |
| --- | --- |
| Platform | Linux with Landlock; kernel 6.7+ for the network rules |
| [Herdr](https://herdr.dev) | 0.9.0 or newer |
| [nono](https://nono.sh) | 0.78 or newer, with the OpenCode pack (`nono pull nolabs-ai/opencode`) |
| [OpenCode](https://opencode.ai) | 2.x on the `PATH` Herdr gives plugins, signed in to a provider |
| Rust | Only to build from source: the install step downloads a prebuilt binary (x86_64 or aarch64 Linux) and builds with `cargo` only when there is none |

## Install

```bash
nono pull nolabs-ai/opencode
herdr plugin install schemaitat/herdr-nono-plugin
herdr plugin action list --plugin nono.sandbox          # twelve actions
```

Herdr clones the repository and runs its build step,
`scripts/install-binary.sh`. It downloads the release binary that matches the
plugin version into `bin/herdr-nono`, checks its sha256 against the release's
`SHA256SUMS`, and runs it once to be sure it works here. With no matching
release (an unreleased checkout) or no network it builds from source with
`cargo build --release --locked` instead, when `cargo` is on the `PATH` or in
`~/.cargo/bin`. Run it again any time with `sh scripts/install-binary.sh`;
`HERDR_NONO_NO_BUILD=1` never builds. Nothing but the binary runs at runtime:
no Node.js, no `npm`.

## Check the host

```bash
herdr plugin action invoke doctor --plugin nono.sandbox
herdr plugin log list --plugin nono.sandbox --limit 1
```

The first line of the log entry is the result line with `"ok":true`. `doctor`
resolves both [profiles](profiles.md) and runs an escape probe inside a
throwaway server sandbox; every probe should be `ok`, including
`opencodeServicePort: denied`. If not, see [Troubleshooting](troubleshooting.md).

## Add the key bindings

```bash
herdr plugin action invoke install-keybindings --plugin nono.sandbox
```

See [Key bindings](key-bindings.md).

## Start an agent

1. Open Herdr in a project and press `prefix+shift+a`. A pane
   `nono opencode <id>` opens, the server sandbox starts in the background, and
   the OpenCode TUI starts in the pane.
2. Press `prefix+shift+o`. The [overlay](overlay.md) shows the agent as
   `● running`, `client+server`, `✔ ok`, with the TUI in the client box and
   `opencode serve` in the server box.
3. Ask the agent to run a command. While it runs, its processes appear as
   `tool` under the server in the overlay.
4. Quit OpenCode. The plugin stops the server. `prefix+shift+b` resumes the
   conversation in fresh sandboxes.

To see the sandbox at work, ask the agent to run
`curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:4096/api/info`
(the OpenCode host service: `000`, unreachable), the same against
`https://github.com` (`200`, through nono's proxy) and `herdr pane list`
(fails: Herdr's socket is blocked).

## Link a checkout instead

For development. `herdr plugin link` skips the build step:

```bash
git clone https://github.com/schemaitat/herdr-nono-plugin.git
cd herdr-nono-plugin && sh scripts/install-binary.sh
herdr plugin link "$PWD"
sh scripts/run-action.sh doctor
```

# The profiles

The plugin ships two nono profiles and passes them to `nono run` by path, so
nothing is installed into `~/.config/nono/profiles`:

| Profile | Used for |
| --- | --- |
| [`profiles/herdr-opencode-server.json`](../../profiles/herdr-opencode-server.json) | OpenCode's server and every tool it runs (`serverProfile`) |
| [`profiles/herdr-opencode-client.json`](../../profiles/herdr-opencode-client.json) | OpenCode's TUI in the pane (`profile`) |

Show the resolved form with
`nono profile show --json profiles/herdr-opencode-server.json`.

## Settings both share

| Setting | Value | Effect |
| --- | --- | --- |
| `extends` | `nolabs-ai/opencode` | Everything OpenCode needs: its state and config directories, language toolchains, `/tmp`. |
| `linux.af_unix_mediation` | `"pathname"` | Denies connects to Unix sockets the profile does not grant: Herdr's control socket, the systemd user manager, the D-Bus session bus, the SSH and GPG agents, X11 and Wayland. |
| `environment.deny_vars` | `HERDR_*`, `SSH_AUTH_SOCK`, `DBUS_SESSION_BUS_ADDRESS`, `TMUX` and similar | Keeps those variables out of the sandbox. The bridge strips the same ones before it starts nono, so they stay out under any profile. |
| `filesystem.suppress_save_prompt` | `["/"]` | No "review denied paths" prompt when a session ends. Its default answer grants every path, including `~/.ssh`. Denials still happen; `nono why` explains them. |

## Network

| Profile | Setting | Added per launch | Effect |
| --- | --- | --- | --- |
| Server | `network.allow_domain: ["*"]` | `--listen-port <port>` | Egress only through nono's proxy, with `HTTP(S)_PROXY` set. Clients that honour the proxy reach any public host; direct TCP (SSH, databases, anything on localhost) and UDP are denied. The server may accept its client on `<port>`. |
| Client | `network.block: true` | `--open-port <port>` | No network except the server's port. |

## Grants added per launch

| Flag | From |
| --- | --- |
| `--allow <workspace root>` | [Which directory gets granted](../explanation/design.md#which-directory-gets-granted) |
| `--allow <dir>` | each entry of `allowPaths` |
| `--read <dir>` | each entry of `readPaths` |
| anything in `nonoArgs` | placed before `--` |

The plugin never passes `--allow-cwd`.

# Profiles

Each OpenCode agent runs under two nono profiles, one per sandbox:

| Sandbox | Runs | Config key | Default |
| --- | --- | --- | --- |
| Server | `opencode serve` and **every tool call** | `serverProfile` | [`profiles/herdr-opencode-server.json`](../profiles/herdr-opencode-server.json) |
| Client | The OpenCode TUI in the pane | `profile` | [`profiles/herdr-opencode-client.json`](../profiles/herdr-opencode-client.json) |

The server profile is the one that matters for what the agent can do; the
client only draws the TUI. `open-shell` uses the server profile too, so a
sandboxed shell behaves like the agent's tools.

## Change a profile

1. Copy the shipped profile to a path of your own and edit it. The plugin
   directory is `pluginRoot` in `doctor`'s output:

    ```bash
    cp <pluginRoot>/profiles/herdr-opencode-server.json ~/my-opencode-server.json
    ```

2. Point the plugin at it in `config.json`, in the directory printed by
   `herdr plugin config-dir nono.sandbox`. A value is a profile name from
   `nono profile list` or an absolute path; `null` means the shipped one.

    ```json
    {
      "serverProfile": "/home/me/my-opencode-server.json",
      "profile": null
    }
    ```

3. Run `doctor`. It resolves both profiles and probes the server profile from
   inside a real sandbox; every probe must stay `ok`.
4. Restart the agent: quit OpenCode and press `prefix+shift+b`. Running
   agents keep the profile they started with.

The overlay's details panel and `info` show which profiles an agent runs
under. `nono profile show --json <profile>` shows the resolved form.

### Keep these settings

Both shipped profiles extend the `nolabs-ai/opencode` pack, which grants what
OpenCode needs (its state and config directories, toolchains, `/tmp`), and add:

| Setting | Why |
| --- | --- |
| `linux.af_unix_mediation: "pathname"` | Blocks Unix sockets the profile does not grant: Herdr's control socket, systemd, D-Bus, the SSH and GPG agents. Without it the agent can drive Herdr or start processes outside the sandbox. |
| `environment.deny_vars` | Keeps `HERDR_*`, `SSH_AUTH_SOCK`, `DBUS_SESSION_BUS_ADDRESS`, `TMUX` and similar out. The plugin also strips them itself. |
| `filesystem.suppress_save_prompt: ["/"]` | Skips nono's "review denied paths" prompt on exit, whose default grants every denied path. |

The network mode of each side is what joins client and server, so keep it:

| Profile | Network | The plugin adds | Result |
| --- | --- | --- | --- |
| Server | `allow_domain: [...]` (proxy mode) | `--listen-port <port>` | Accepts its client on `<port>`; egress only through nono's proxy to the allowed hosts; no direct TCP, no localhost, no UDP |
| Client | `block: true` | `--open-port <port>` | Reaches its server's port, nothing else |

nono honours `--listen-port` only in proxy mode and `--open-port` only in
blocked mode, so another network mode breaks the connection between client
and server. A server profile with open egress, or an allowlist that covers
localhost, makes the OpenCode host service reachable; the plugin then refuses
to start (see [Security](security.md#why-egress-is-an-allowlist)).

## Allowed hosts

The shipped server profile allows one provider, GitHub Copilot:

| Host | Used for |
| --- | --- |
| `api.githubcopilot.com`, `*.githubcopilot.com` | Chat and model requests (`api.individual.`, `api.business.`, `api.enterprise.` per plan) |
| `api.github.com` | Exchanging the GitHub login for a Copilot endpoint (`/copilot_internal/user`) |
| `github.com` | The device login (`/login/device/code`) when you connect Copilot from the TUI |
| `models.opencode.ai` | OpenCode's model catalog |

Log in once on the host (`opencode auth login`, provider GitHub Copilot); the
sandbox reads the stored login from OpenCode's data directory.

## Common changes

**Allow another provider or host.** Add its API host to
`network.allow_domain` in a copy of the server profile, for example
`api.anthropic.com` for Anthropic, `opencode.ai` for OpenCode Zen, or
`registry.npmjs.org` for `npm install`:

```json
"network": { "allow_domain": ["models.opencode.ai", "github.com", "api.github.com", "api.githubcopilot.com", "*.githubcopilot.com", "api.anthropic.com"] }
```

Never add `"*"`, `localhost`, an IP address, a single-label name, a `.local`
or `.internal` name, or a wildcard DNS service such as `nip.io`: nono's proxy
connects to whatever an allowed name resolves to, so each of them opens
localhost and the plugin refuses to start.

**Grant another directory** without a custom profile, in `config.json`:

```json
{ "readPaths": ["/home/me/reference-docs"], "allowPaths": ["/home/me/scratch"] }
```

Both sandboxes get these, as `--read` and `--allow`. They are not checked
against your home directory or `/`; granting either undoes most of the
sandbox.

**Other `nono run` flags**, such as a memory limit or rollback:

```json
{ "nonoArgs": ["--memory", "4G"] }
```

## What the plugin grants per launch

On top of the profile: `--allow <workspace root>` (the worktree checkout, the
workspace directory, or the git repository of the focused pane; never your
home directory or `/`, never `--allow-cwd`), `allowPaths`, `readPaths`,
`nonoArgs`, and the port flags above. `nonoArgs` cannot carry network flags
(`--allow-domain`, `--network-profile`, `--open-port`, `--listen-port`,
`--allow-connect-port`, `--upstream-proxy`, `--upstream-bypass`), `--profile`
or `--allow-cwd`; network access belongs in the profile, where `doctor`
checks it.

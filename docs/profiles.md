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
| Server | `allow_domain: [...]` (proxy mode) | `--listen-port <port>` | Accepts its client on `<port>`; egress only through nono's proxy; no direct TCP, no localhost, no UDP |
| Client | `block: true` | `--open-port <port>` | Reaches its server's port, nothing else |

nono honours `--listen-port` only in proxy mode and `--open-port` only in
blocked mode, so another network mode breaks the connection between client
and server. A server profile with open egress also makes localhost reachable;
the plugin then refuses to start while the OpenCode host service runs (see
[Security](security.md#the-opencode-host-service)).

## Common changes

**Allow only some domains.** In the server profile replace `"*"` in
`network.allow_domain`. OpenCode itself needs `opencode.ai`,
`models.opencode.ai` and your provider's API host:

```json
"network": { "allow_domain": ["opencode.ai", "models.opencode.ai", "api.anthropic.com", "github.com", "registry.npmjs.org"] }
```

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
`nonoArgs`, and the port flags above.

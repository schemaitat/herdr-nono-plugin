# Profiles

A nono profile is a JSON policy: which directories a sandbox may read and
write, which hosts it may reach, which sockets it may open. Each OpenCode agent
runs under two of them, one per sandbox:

| Sandbox | Runs | Config key | Default |
| --- | --- | --- | --- |
| Server | `opencode serve` and **every tool call** | `serverProfile` | [`profiles/herdr-opencode-server.json`](../profiles/herdr-opencode-server.json) |
| Client | The OpenCode TUI in the pane | `profile` | [`profiles/herdr-opencode-client.json`](../profiles/herdr-opencode-client.json) |

The server profile decides what the agent can do; the client only draws the
TUI. `open-shell` uses the server profile too, so a sandboxed shell behaves
like the agent's tools.

## See the active profiles

**In the overlay.** Press `prefix+shift+o`, then `i`. The profiles view shows,
for the selected agent (or for the next launch when no agent runs), each
sandbox's profile with its name, where it comes from, its file, what it
extends, its network, its directory grants and its description. Below that:
what the next launch uses, the `config.json` to change it in, and nono's user
profile directory. `i` again goes back to the sandboxes.

```text
┌─ Profiles · agent wQ:p2 ──────────────────────────────────────────────────────────────────────────────┐
│ server sandbox: opencode serve and every tool call · config key serverProfile                        │
│   Profile     herdr-opencode-server · shipped with the plugin                                        │
│   File        ~/.config/herdr/plugins/github/…/profiles/herdr-opencode-server.json                     │
│   Extends     nolabs-ai/opencode                                                                     │
│   Network     nono proxy to 5 hosts: models.opencode.ai, github.com, api.github.com, api.githubcopi… │
│   Files       11 read-write, 3 read-only directories, plus the workspace root (read-write)           │
│   Sockets     AF_UNIX pathname mediation: Herdr, D-Bus, SSH and GPG agents blocked                   │
│   About       OpenCode's server in a Herdr pane (herdr-nono-plugin). The server runs the model loo… │
│ client sandbox: the OpenCode TUI in the pane · config key profile                                    │
│   ...                                                                                                │
│ where to change them                                                                                 │
│   Next launch same profiles (server herdr-opencode-server · client herdr-opencode-client)            │
│   Config      ~/.config/herdr/plugins/config/nono.sandbox/config.json, keys "serverProfile" and "pr… │
│   nono        user profiles in ~/.config/nono/profiles · nono profile list · nono profile show <nam… │
└──────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

`Profile` says where a profile comes from:

| Source | Means | Referenced in `config.json` as |
| --- | --- | --- |
| shipped with the plugin | A file in the plugin's `profiles/` directory | `null` (the default) |
| your profile file | Any other JSON file | Its absolute path |
| nono user profile | A file in `~/.config/nono/profiles/` (`$XDG_CONFIG_HOME/nono/profiles/`) | Its name |
| built into nono or a nono package | `nono profile list` shows it under "Built-in" or "Packages" | Its name |

**From the command line:**

```bash
herdr plugin action invoke doctor --plugin nono.sandbox   # "plugin root", "config dir" and one line per profile
herdr plugin config-dir nono.sandbox                      # where config.json lives
nono profile list                                         # every profile nono knows by name, with its directory
nono profile show <name|path>                             # the fully resolved profile, extends applied
nono profile diff herdr-opencode-server my-opencode-server
```

The shipped files are in `<plugin root>/profiles/`. `info` on an agent's pane
prints the profiles that agent started with; running agents keep them until
they are relaunched.

## Change a profile

Two ways, both ending in `config.json`:

- **Extend** a shipped profile with a small profile of your own that adds
  only what you need. You keep the plugin's settings and get its future
  fixes. Use this for adding a host or a directory.
- **Copy** a shipped profile and edit the copy. You own every setting. Use
  this to remove something the shipped profile grants.

### Extend and register a profile (recommended)

nono finds a profile by name when its file is in its user profile directory.
`extends` takes names, and lists such as `network.allow_domain` and
`filesystem.read` are appended to the parent's.

1. Register the shipped profile under its own name, so yours can extend it:

    ```bash
    mkdir -p ~/.config/nono/profiles
    cp "<plugin root>/profiles/herdr-opencode-server.json" ~/.config/nono/profiles/
    ```

2. Write `~/.config/nono/profiles/my-opencode-server.json` with only the
   additions:

    ```json
    {
      "extends": "herdr-opencode-server",
      "meta": { "name": "my-opencode-server", "description": "Shipped server profile plus Anthropic" },
      "network": { "allow_domain": ["api.anthropic.com"] },
      "filesystem": { "read": ["$HOME/reference-docs"] }
    }
    ```

3. Check it: `nono profile validate my-opencode-server` and
   `nono profile diff herdr-opencode-server my-opencode-server`, which should
   list only your additions.
4. Point the plugin at it by name in `config.json` (see
   [Use it](#use-it)): `"serverProfile": "my-opencode-server"`.

The registered copy of the shipped profile does not update with the plugin:
after a plugin update, copy it again (the shipped file's `meta.version`
tells you whether it changed).

### Copy and edit a profile

```bash
cp "<plugin root>/profiles/herdr-opencode-server.json" ~/my-opencode-server.json
```

Edit the copy, then set `"serverProfile": "/home/me/my-opencode-server.json"`.
An absolute path works without registering anything. To refer to it by name
instead, put it in `~/.config/nono/profiles/` and change `meta.name`.

### Use it

1. In `config.json` (the directory `herdr plugin config-dir nono.sandbox`
   prints), set a nono profile name or an absolute path; `null` means the
   shipped one:

    ```json
    {
      "serverProfile": "my-opencode-server",
      "profile": null
    }
    ```

2. Run `doctor`. It resolves both profiles and probes the server profile from
   inside a real sandbox; every probe must stay `ok`.
3. Restart the agent: quit OpenCode and press `prefix+shift+b`. Until then,
   the overlay's profiles view shows the new profile under `Next launch`.

### Keep these settings

Both shipped profiles extend the `nolabs-ai/opencode` pack, which grants what
OpenCode needs (its state and config directories, toolchains, `/tmp`), and add:

| Setting | Why |
| --- | --- |
| `linux.af_unix_mediation: "pathname"` | Blocks Unix sockets the profile does not grant: Herdr's control socket, systemd, D-Bus, the SSH and GPG agents. Without it the agent can drive Herdr or start processes outside the sandbox. |
| `environment.deny_vars` | Keeps `HERDR_*`, `SSH_AUTH_SOCK`, `DBUS_SESSION_BUS_ADDRESS`, `TMUX` and similar out. The plugin also strips them itself. |
| `filesystem.suppress_save_prompt: ["/"]` | Skips nono's "review denied paths" prompt on exit, whose default grants every denied path. |

A profile that extends a shipped one inherits all three. The network mode of
each side is what joins client and server, so keep it:

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
`network.allow_domain` in your server profile, for example
`api.anthropic.com` for Anthropic, `opencode.ai` for OpenCode Zen, or
`registry.npmjs.org` for `npm install` (see
[Extend and register a profile](#extend-and-register-a-profile-recommended)).

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

# Change what the sandbox may touch

All of these go into `config.json` in the directory printed by
`herdr plugin config-dir nono.sandbox`. They take effect at the next
`start-agent` or `reconnect`. [Configuration](../reference/configuration.md)
lists every key.

## Grant another directory

Read-only, for example reference docs or a shared library checkout:

```json
{ "readPaths": ["/home/me/reference-docs"] }
```

Read-write:

```json
{ "allowPaths": ["/home/me/scratch"] }
```

Paths must be absolute. Unlike the workspace, these are not checked against
your home directory or `/`; granting either undoes most of the sandbox.

## Limit memory or turn on rollback

Pass extra `nono run` flags with `nonoArgs`:

```json
{ "nonoArgs": ["--memory", "4G"] }
```

```json
{ "nonoArgs": ["--rollback"] }
```

## Restrict egress to some domains

Copy [`profiles/herdr-opencode-server.json`](../../profiles/herdr-opencode-server.json)
to a path of your own, replace `"*"` in `network.allow_domain` with the
domains you want, and point the plugin at it:

```json
{ "serverProfile": "/home/me/.config/nono/herdr-opencode-server-strict.json" }
```

OpenCode itself needs `opencode.ai`, `models.opencode.ai` and your provider's
API host. Keep everything else in the profile as it is (the AF_UNIX
mediation and the denied variables are what keep the agent away from Herdr's
socket). Run `doctor` afterwards: every probe should still be `ok`.

## Use your own profile

Set `profile` (the client's sandbox) or `serverProfile` (where the tools run)
to a nono profile name from `nono profile list` or to an absolute path. Start
from the shipped profiles; [The profiles](../reference/profiles.md) lists what
each setting is for. Run `doctor`: it probes the server profile from inside a
real sandbox and fails if Herdr's socket, systemd, D-Bus or a localhost
service is reachable.

## Pass environment variables to the agent

```json
{ "agentEnv": ["RUST_LOG=info"] }
```

Keep secrets out of `agentEnv`; nono's credential proxy is the place for API
keys.

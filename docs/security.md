# What the sandbox stops, and what it does not

Tested 2026-09-29 and 2026-10-03 on Linux 7.0 with nono 0.78.0, OpenCode
2.0.18 and 2.0.20 (Bun 1.4.x inside), Herdr 0.9.1 and the `nolabs-ai/opencode`
pack 0.2.0. Every claim below comes from a command run on that host; the
commands are given so they can be repeated. Results depend on the nono
version and on the pack.

## Layout: a server sandbox and a client sandbox

OpenCode 2.x splits into a client (the TUI or `opencode run`) and a server
that owns sessions, plugins, permissions and **tool execution**. Plain
`opencode` connects to a background service on the host (`opencode serve
--service`, `127.0.0.1:4096`), so wrapping only the client in nono leaves
every tool call outside the sandbox. The plugin starts a private server per
pane in its own nono sandbox and the client in a second one:

```text
nono (supervisor)                                   client sandbox: network blocked, --open-port P
└─ opencode --server http://127.0.0.1:P             the TUI
nono (supervisor)                                   server sandbox: egress via nono's proxy, --listen-port P
└─ opencode serve --hostname 127.0.0.1 --port P     private server
   └─ /usr/bin/bash -c ...                          tool call
```

The plugin picks `P` on the host, generates a password per launch and passes
it to both as `OPENCODE_PASSWORD`; the server answers `401` without it.

Evidence: `verify-sandbox` during a real tool call listed the client, the
server and the tool processes, all with `no_new_privs` and `NONO_CAP_FILE`.
The client's `--server` argument is required (configuration cannot replace
it), and the verification fails when the client points elsewhere, when no
`serve ... --port P` process runs in the server sandbox, or when any process
in either tree lacks the marks.

### Why not `--standalone` in one sandbox

The first release ran `opencode --standalone`, which starts
`opencode serve --stdio --port 0` as a child, in one sandbox with open egress.
That keeps the server inside the sandbox, but open egress means open loopback:
the tools could reach the host service (below). Every nono mode that blocks
direct loopback connects breaks standalone mode:

- With a domain allowlist (proxy mode) the server's `bind(127.0.0.1:0)` is
  denied: `ServeError ... permission denied 127.0.0.1:0`. `--listen-port 0`
  allows the bind, but the client is then denied the connect back to the
  ephemeral port (`Transport: Was there a typo in the url or port?`), and
  `open_port_range: [[32768, 60999]]` does not change that.
- With `network.connect_port` (direct egress restricted to ports 53, 80 and
  443, no proxy) nono denies every `bind()`, even an explicit
  `--listen-port`, and its seccomp baseline denies UDP sockets, so DNS
  only works over TCP (`RES_OPTIONS=use-vc`).

Only `network.block: true` honours `--open-port` for both bind and connect,
and only the proxy mode honours `--listen-port` for an explicit port. Hence
the two sandboxes: the server in proxy mode with `--listen-port P`, the client
blocked with `--open-port P`. The nono-test launcher had found the same
layout for its allowlisted variant.

## Kernel requirement: Landlock ABI v4 (Linux 6.7+)

The network half of the layout relies on Landlock's TCP `bind`/`connect`
rules, which arrived with Landlock ABI v4 in Linux 6.7:

- In the server sandbox (proxy mode), these rules confine connects to nono's
  proxy, which is what denies direct TCP, including loopback and the host
  service. They also allow the one `--listen-port P` bind.
- In the client sandbox (`network.block: true`), these rules allow only the
  connect to `--open-port P`.

The rest does not need ABI v4:

- Filesystem confinement (the `--allow <root>` grant and the pack's paths)
  works with older Landlock ABIs.
- AF_UNIX pathname mediation goes through seccomp notifications, not Landlock.

Not tested: what nono 0.78 does on a kernel older than 6.7, whether it refuses
to start or runs without the network rules. The plugin does not check the
kernel or the Landlock ABI itself. On such a host, run `doctor` and confirm
that the escape probe passes and that it reports `opencodeServicePort: denied`
before you rely on the loopback lockdown. Everything in this document was
verified on Linux 7.0.

## Escape paths the shipped profiles close

The stock `nolabs-ai/opencode` pack, and a user profile extending it without
further settings, leaves pathname Unix sockets unmediated on Linux, leaves
direct TCP open, and nono passes the caller's environment through. From
inside such a sandbox:

| Target | Why it matters | Stock pack | Shipped profiles |
| --- | --- | --- | --- |
| `~/.config/herdr/herdr.sock` (and `HERDR_SOCKET_PATH`) | Herdr's API can split panes and type commands into them; those run outside the sandbox | connected | denied |
| `$XDG_RUNTIME_DIR/systemd/private` | `systemd-run --user` starts processes outside the sandbox | connected | denied |
| `$XDG_RUNTIME_DIR/bus` | the D-Bus session bus reaches the same systemd manager | connected | denied |
| `$XDG_RUNTIME_DIR/openssh_agent`, `gnupg/S.gpg-agent` | sign and decrypt with your keys | connected | denied |
| `127.0.0.1:4096` (OpenCode host service) | runs tools on the host | connected (`401`, `200` with the password from `service.json`) | denied, directly and through the proxy |
| `/var/run/docker.sock` | root-equivalent | denied | denied |

Evidence: a Python probe calling `connect()` on each socket inside
`nono run --profile nolabs-ai/opencode` connected to all but Docker; with
`"linux": { "af_unix_mediation": "pathname" }` every connect failed with
`EACCES`. For the port: from the stock pack's sandbox `curl` reached
`127.0.0.1:4096` and authenticated with the password read from
`~/.local/state/opencode/service.json` (`200`); from the server profile's
sandbox the same request returned nothing (`000`), with and without
`--noproxy '*'`, while GitHub, npm, PyPI, Wikipedia and opencode.ai returned
`200`. In a Herdr pane the agent itself ran `curl` against `127.0.0.1:4096`,
`herdr pane list` and `systemd-run --user`; all three failed.

`doctor` repeats the probe on every run with the server profile (the one the
tools run under) and fails with `unconfined` if Herdr's socket, systemd,
D-Bus, Docker or the host service's port is reachable. With the stock
`opencode` profile it reports `probe FAIL` for Herdr's socket, systemd,
D-Bus, the SSH and GPG agents and the leaked `HERDR_*` variables.

The profiles also remove `HERDR_*`, `SSH_AUTH_SOCK`,
`DBUS_SESSION_BUS_ADDRESS`, `TMUX` and similar variables
(`environment.deny_vars`), and the bridge strips them before it starts nono,
so a user profile without `deny_vars` does not leak them either.

Side effects: the X11 and Wayland sockets are pathname sockets too, so
clipboard helpers inside the sandboxes fail; SSH (`git@github.com:...`) and
other non-HTTP protocols cannot leave the server sandbox.

## nono rate-limits connections in proxy mode

With proxy mode and AF_UNIX mediation together, nono 0.78 checks connections
through seccomp notifications and rate-limits them; its log says `Rate
limited network seccomp notification, denying`. There is no setting for it.
Measured from the server profile's sandbox, 16 HTTPS requests:

| Pause between requests | Result |
| --- | --- |
| none | 10 succeed, the next 6 fail |
| 0.1 s | all succeed |
| 0.25 s, 0.5 s | all succeed |

A single Python process making 20 back-to-back requests lost 10;
`npm install lodash express` succeeded but took about a minute of retries.
Each new process also costs two denied connects to `/var/run/nscd/socket`
(glibc looks for the name service cache daemon), which count against the
same budget. Proxy mode without AF_UNIX mediation is not rate-limited, but
reopens the Herdr socket; the profile shipped in the first release (open
egress with AF_UNIX mediation) is not rate-limited either (40 of 40
requests), but leaves loopback open. This trade-off was chosen knowingly:
localhost locked down, bursts of connections slowed. It is worth reporting
to nono.

## What remains open

### The OpenCode host service's password

`~/.local/state/opencode/service.json` and `~/.config/opencode/service.json`
hold the host service's password, and the pack grants both directories
read-write because OpenCode needs them. Landlock cannot deny one file below a
granted directory; nono says so: `Landlock deny-overlap is not enforceable on
Linux ... deny '/home/dev/.local/state/opencode/service.json' overlaps
allowed parent`. With the shipped server profile the password is useless,
because the port is unreachable. With a server profile that leaves loopback
open, the plugin detects the service (the pid in `service.json` is alive,
runs `opencode`, and listens on the port) and by default refuses to start
agents (`hostServiceCheck`); `doctor` fails as well.

### Shared OpenCode state and config

Sessions (`~/.local/share/opencode/opencode.db`), auth and config are shared
between sandboxed and host OpenCode, and the pack grants
`~/.config/opencode`, `~/.claude` and `~/.codex` read-write. A sandboxed agent
can therefore add a plugin or an MCP server to `~/.config/opencode` that a
later **host-side** OpenCode loads, or change Claude Code's settings. Treat
these directories like the workspace: review changes before host tools use
them.

### The workspace

The agent writes the granted worktree, including `.git/hooks`, build scripts
and CI configuration. Anything you run from it on the host afterwards runs
unconfined.

### Egress

Every public host is reachable through the proxy. A domain allowlist is a
one-line change to the server profile (`allow_domain`), which this layout
supports; it was not the goal of this release. The domains OpenCode itself
needed in the nono-test investigation were `opencode.ai`,
`models.opencode.ai` and the provider's API host (for GitHub Copilot on that
account, `api.enterprise.githubcopilot.com`).

## Reproducing

```bash
sh scripts/run-action.sh doctor           # the escape probe, with the server profile
sh scripts/run-action.sh verify-sandbox   # both process trees of the running agent
nono profile show --json profiles/herdr-opencode-server.json
```

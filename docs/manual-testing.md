# Manual test plan

The automated tests exercise the plugin against fake `nono`, `herdr` and
`opencode` executables and fake `/proc` trees. This checklist covers what
only a real host can show: nono's actual confinement, Herdr's pane handling,
and OpenCode's behaviour inside the sandbox. Work through it top to bottom.

## Helper

```bash
nonorun() { sh /path/to/herdr-nono-plugin/scripts/run-action.sh "$@"; }
```

Pane-scoped actions use the focused pane, or the workspace's only mapping
when the focused pane has none.

## 0. Prerequisites

```bash
uname -r                       # Linux with Landlock; 6.7+ (ABI v4) for network rules, see docs/security.md
node --version                 # v20 or newer
herdr --version                # 0.9.0 or newer
nono --version                 # 0.78.0 or newer
nono list --installed          # nolabs-ai/opencode; otherwise: nono pull nolabs-ai/opencode
opencode --version             # 2.x, signed in to a provider
```

Create a throwaway repository to use as the workspace, outside the plugin
checkout:

```bash
mkdir -p ~/tmp/nono-plugin-demo && cd ~/tmp/nono-plugin-demo
git init -q && printf 'demo\n' > README.md && git add README.md && git commit -q -m init
```

## 1. Link the plugin

```bash
cd /path/to/herdr-nono-plugin && sh scripts/write-node-path.sh
herdr plugin link "$PWD"
herdr plugin action list --plugin nono.sandbox
```

Expect twelve actions (`doctor` through `forget-mapping`).

## 2. doctor

```bash
nonorun doctor
```

Expect `ok: true`, a client profile with egress `blocked` and a server
profile with egress `allowlist [*]`, both with `AF_UNIX mediation pathname`,
and every probe `ok` (including `opencodeServicePort: denied`) except
possibly `opencodeServicePassword` (a warning). If the OpenCode host service
runs, expect the line `OpenCode host service: pid ..., not reachable from the
agent's tools`.

Negative check: put `{ "serverProfile": "nolabs-ai/opencode" }` in the
config (`herdr plugin config-dir nono.sandbox`) and run `doctor`. Expect
`ok: false`, `errorKind: "unconfined"`, `probe FAIL herdrSocket: allowed` and
`probe FAIL opencodeServicePort: ...`. Remove the setting again.

## 3. Key bindings

```bash
nonorun install-keybindings
```

Expect four `bound` lines and `reloaded`, or `already bound` on a second run.

## 4. start-agent and verification

Open a Herdr workspace on the demo repository, focus a pane, press
`prefix+shift+a`. Expect a pane `nono opencode <id>`, nono's grant summary,
then the OpenCode TUI. Herdr should show the pane as an `opencode` agent.

```bash
nonorun info
```

Expect `mapping.lifecycleState: "running"`, a `mapping.port`, and a
`verification` with `ok: true`, a `client`
(`opencode --server http://127.0.0.1:<port>`) and a `server`
(`opencode serve --hostname 127.0.0.1 --port <port>`). `nono ps` lists two
sessions, `<session>` and `<session>-server`.

## 5. Tools run inside the server sandbox and cannot reach localhost

In the OpenCode pane, ask: *Use the bash tool to run exactly:
`curl -s -o /dev/null -w '%{http_code}\n' --max-time 3 http://127.0.0.1:4096/api/info; curl -s -o /dev/null -w '%{http_code}\n' https://github.com; herdr pane list; sleep 20`*.
While it sleeps:

```bash
nonorun verify-sandbox
```

Expect every process `confined`, with `bash -c ...` and `sleep 20` as `tool`
processes of the server sandbox. The agent's answer should show `000` for
port 4096, `200` for GitHub, and a failing `herdr`.

## 6. stop, reconnect, open-shell

```bash
nonorun stop          # the pane prints "OpenCode exited with code 130"; nono ps no longer lists <session>-server
nonorun reconnect     # mode "connect", argv ends with --continue; the old conversation is back
nonorun reconnect     # while it runs: errorKind "conflict"
nonorun forget-mapping   # while it runs: errorKind "conflict"
nonorun open-shell    # a pane below with a [nono:<id>] prompt
```

In the shell pane: `echo $NONO_CAP_FILE` prints a path, `echo $HERDR_SOCKET_PATH`
prints nothing, `touch ~/x` fails, `touch ./x` works. Leave it with `exit`;
no "review denied paths" prompt should appear.

## 7. Overlay and listing

Press `prefix+shift+o`. Expect a full-screen view with a title bar, an
Agents table (your agent `●` `running`, NONO `client+server`, VERIFIED
`✔ ok`), the flow line, a cyan `client · no network but :<port>` box with the
TUI process and a magenta `server · on :<port>, proxy egress` box with
`opencode serve` (while a tool runs, its processes appear under it as
`tool`), and a Details panel for the selected row. Move with `j`/`k`, press `v`
(status line: `confined: ...`), `r`, then `q`. `nonorun list-sandboxes` shows
the same mappings.

## 8. Pane closed under a running agent

Close the agent pane with Herdr. Expect `nono ps` to no longer list the
session within a few seconds. Open the overlay: the row shows `✗` after its
pane and the title bar `1 stale`. Press `p`, answer `y`: the status line says
`pruned 1 mapping` and the row disappears. (`nonorun prune-mappings` does the
same from a terminal.)

## 9. Worktree hook

Create a worktree with Herdr, start an agent in it, then remove the worktree
with Herdr. Expect the agent to stop and its mapping to disappear
(`nonorun list-sandboxes`), with a toast.

## 10. Host service refusal (open-egress server profile only)

With the shipped profiles a running host service is unreachable and no
reason to refuse. To see the refusal, start the host service
(`opencode service start`), set `"serverProfile": "nolabs-ai/opencode"` and
run `start-agent`. Expect a framed `nono: the agent was NOT started` block in
the pane with the service's pid and `opencode service stop`, a toast, and no
nono session (`nono ps`). Remove the setting.

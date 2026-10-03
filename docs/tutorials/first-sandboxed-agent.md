# Your first sandboxed agent

In this tutorial you install the plugin, start OpenCode in a Herdr pane inside
two nono sandboxes, and watch one of its tools fail to reach a service on your
own machine. It takes about fifteen minutes.

You need a Linux machine with [Herdr](https://herdr.dev) 0.9 or newer,
[nono](https://nono.sh) 0.78 or newer, Node.js 20 or newer and
[OpenCode](https://opencode.ai) 2.x, signed in to a model provider. Each
command below shows what you should see; if you see something else, stop and
look it up in [Troubleshoot](../how-to/troubleshoot.md).

## 1. Install the plugin

Fetch the nono profile pack the plugin builds on, then install the plugin:

```bash
nono pull nolabs-ai/opencode
herdr plugin install schemaitat/herdr-nono-plugin
```

Herdr asks for confirmation and then records where `node` lives. Check that
Herdr registered the plugin's actions:

```bash
herdr plugin action list --plugin nono.sandbox
```

You see twelve actions, from `doctor` to `forget-mapping`.

## 2. Check the host

```bash
herdr plugin action invoke doctor --plugin nono.sandbox
herdr plugin log list --plugin nono.sandbox --limit 1
```

The first line of the log entry starts with `HERDR_SANDBOX_RESULT:` and
contains `"ok":true`. Further down you see two profiles, the client's with
egress `blocked` and the server's with egress `allowlist [*]`, and a list of
probes that are all `ok`, among them `opencodeServicePort: denied`.

`doctor` has just started a throwaway sandbox and tried to break out of it.
Every `ok` is an escape that failed.

## 3. Add the key bindings

```bash
herdr plugin action invoke install-keybindings --plugin nono.sandbox
```

Herdr reloads its config. From now on `prefix+shift+a` starts an agent.

## 4. Make a practice repository

Use a throwaway repository so nothing you care about is at stake:

```bash
mkdir -p ~/tmp/nono-demo && cd ~/tmp/nono-demo
git init -q && printf 'demo\n' > README.md && git add README.md && git commit -q -m init
```

Open Herdr in this directory.

## 5. Start the agent

Press `ctrl+b`, release, then `shift+a`.

A new pane named `nono opencode <id>` opens on the right. nono prints a short
summary of what the client sandbox may touch, and then the OpenCode TUI
starts. Herdr's sidebar shows the pane as an `opencode` agent.

## 6. See the sandboxes

Press `ctrl+b`, then `shift+o`. A full-screen overview opens. Your agent's row
shows `● running`, NONO `client+server` and VERIFIED `✔ ok`. Below it are two
boxes: the client sandbox (no network except one port) with the TUI, and the
server sandbox (egress through nono's proxy) with `opencode serve`. Every
process carries a `✔`.

Press `q` to close it.

## 7. Ask the agent to break out

Go back to the OpenCode pane and send this prompt:

```text
Use the bash tool to run exactly:
curl -s -o /dev/null -w '%{http_code}\n' --max-time 3 http://127.0.0.1:4096/api/info
curl -s -o /dev/null -w '%{http_code}\n' https://github.com
herdr pane list
sleep 20
```

While it sleeps, press `ctrl+b`, `shift+o` again. In the server box you now
see `bash -c ...` and `sleep 20` marked as `tool`, each with a `✔`: the tool
runs inside the sandbox.

When the agent answers, it reports:

- `000` for `127.0.0.1:4096`: the tool could not reach a service on your own
  machine (OpenCode's unsandboxed host service listens there).
- `200` for GitHub: the internet is reachable through nono's proxy.
- an error from `herdr pane list`: the tool could not reach Herdr's control
  socket, so it cannot type commands into your other panes.

## 8. Leave and come back

Quit OpenCode. The pane prints the exit code and how to resume. Press
`ctrl+b`, `shift+b` in that pane. OpenCode starts again in fresh sandboxes and
continues the same conversation.

## What you did

You ran an AI coding agent whose every tool call was confined by nono, saw
the plugin verify that from outside, and saw a tool fail to reach your
machine's local services while the internet stayed usable.

Next:

- [Manage running agents](../how-to/manage-agents.md): stop, reconnect, open a
  sandboxed shell, clean up.
- [Change what the sandbox may touch](../how-to/grant-access.md).
- [How the plugin is built](../explanation/design.md) and
  [what the sandbox stops](../explanation/security.md).

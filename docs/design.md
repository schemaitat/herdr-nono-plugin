# Design

## Two plugin processes

Herdr runs plugin actions as short-lived processes without a terminal, so
anything interactive lives in a pane:

- **The action** (`herdr-nono action`, started by Herdr through `bin/run.sh`)
  resolves the context, splits the pane, stores a mapping and prints the
  result line.
- **The bridge** (`herdr-nono bridge`) is typed into the new pane. It starts
  both sandboxes, verifies them, and records the exit.

Both are subcommands of the one binary the plugin installs as `bin/herdr-nono`;
so are the overlay (`herdr-nono pane`) and the `worktree.removed` hook
(`herdr-nono events`).

## Launch sequence

1. Pick the workspace root and refuse home and `/`.
2. Split a pane, store a `provisional` mapping, type the bridge command.
3. The bridge stops leftover servers of this mapping, picks a free port `P`
   and a password.
4. Server, detached from the pane, output to a log file:
   `nono run --profile <server> --allow <root> --listen-port P -- opencode serve --hostname 127.0.0.1 --port P`.
   Wait until it answers.
5. Client in the pane:
   `nono run --profile <client> --allow <root> --open-port P -- opencode --server http://127.0.0.1:P`.
   Both get `OPENCODE_PASSWORD`; `HERDR_*` and socket variables are removed.
6. Verify both process trees until they pass (up to twenty seconds); stop on
   failure.
7. When the client exits, stop the server and record the exit code.

## Choices

- **A private server per pane.** OpenCode's server runs the tools, so it must
  run in a sandbox the plugin started; the host service never does.
- **Two sandboxes, not one.** `opencode --standalone` in one sandbox needs
  open loopback, which makes the host service reachable. Every nono mode that
  closes loopback breaks standalone mode, but proxy mode accepts an explicit
  `--listen-port` and blocked mode an explicit `--open-port`. So the server
  uses proxy mode, the client blocked mode, and the plugin owns the port and
  password between them. The cost is nono's rate limit in proxy mode.
- **`--standalone` is stripped.** The pack profile `nolabs-ai/opencode` sets
  `command_args: ["--standalone"]`, which nono appends to every command run
  under a profile that extends it, and a profile cannot clear it. OpenCode
  2.0.22 rejects the flag on `opencode serve` and next to `--server`, and a
  shell rejects it too. So every sandboxed command runs through a small
  `sh -c` wrapper that drops it and `exec`s the real command.
- **Profiles on top of the pack.** `nolabs-ai/opencode` describes what
  OpenCode needs; the plugin adds only what a Herdr pane needs and passes its
  profiles by path, installing nothing.
- **Verification from outside.** `/proc` shows what processes actually carry,
  and checking the tree catches a client talking to a server outside the
  sandbox. It fails closed by default.
- **`--allow <root>`, never `--allow-cwd`.** The grant must not follow `cd`.
- **Ownership by process record.** The bridge and shells record pid and start
  time, so actions never touch a mapping whose agent still runs.
- **No persistent sandbox.** A nono sandbox is a process; there is nothing to
  create ahead of time or delete. This is why herdr-sbx-plugin's
  `fetch-changes`, `open-port` and `replace-sandbox` have no counterpart.

## State

One JSON file per pane under `panes/` in the plugin's state directory, written
atomically under a lock. It ties the pane to a session name, the granted
directory, the last launch (profiles, port, supervisor pids, exit code), the
last verification and a lifecycle state: `provisional`, `starting`,
`running`, `exited` or `failed`.

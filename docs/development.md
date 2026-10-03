# Development

[Link a checkout](getting-started.md#link-a-checkout-instead). There are no
dependencies and no build step beyond `scripts/write-node-path.sh`, which
records the node path for the shim `bin/run.sh`.

## Tests

```bash
npm run check                                            # syntax check, then all tests
node --test test/actions.test.mjs                        # one file
node --test --test-name-pattern="stop" "test/*.test.mjs" # by name
```

The tests run the real scripts against fake `nono`, `herdr` and `opencode`
in `test/fakes/` and fake `/proc` trees, so they need neither nono nor Herdr.

## Docs

```bash
just docs        # live preview on http://127.0.0.1:8000/
just docs-build  # strict build, as CI runs it
```

Pages live flat in `docs/` and are listed in `nav` in `mkdocs.yml`; the README
is the home page. Keep pages short and link instead of repeating.

## On a real host

What the fakes cannot show. Use a throwaway repository and
`nonorun() { sh <plugin dir>/scripts/run-action.sh "$@"; }`.

1. `herdr plugin action list --plugin nono.sandbox` lists twelve actions.
2. `nonorun doctor`: client egress `blocked`, server `allowlist [*]`, every
   probe `ok`. With `"serverProfile": "nolabs-ai/opencode"` it must fail with
   `unconfined`.
3. `nonorun install-keybindings`: four bindings, then `already bound`.
4. `prefix+shift+a`: the TUI starts; `nonorun info` shows `running` and a
   passing verification; `nono ps` lists `<session>` and `<session>-server`.
5. Ask the agent to `curl` `127.0.0.1:4096` (`000`) and GitHub (`200`) and
   run `herdr pane list` (fails), then `sleep 20`. Meanwhile
   `nonorun verify-sandbox` lists the tools as confined.
6. `prefix+shift+o`: both boxes, tools under the server; `v`, `x`, `p` work.
7. `nonorun stop` takes the server down too; `prefix+shift+b` resumes;
   `reconnect` and `forget-mapping` refuse while it runs.
8. `prefix+shift+s`: `touch ~/x` fails, `touch ./x` works.
9. Close the agent pane: the overlay marks it `✗`; `p` prunes it.
10. Remove a worktree with an agent in it: the agent stops, with a toast.

## Source layout

| Path | Purpose |
| --- | --- |
| `herdr-plugin.toml` | Manifest: actions, hook, overlay pane |
| `profiles/` | `herdr-opencode-server.json`, `herdr-opencode-client.json` |
| `bin/run.sh`, `scripts/write-node-path.sh` | Node shim and build step |
| `scripts/install-keybindings.sh` | Key binding installer |
| `scripts/run-action.sh` | Run an action and wait for its result |
| `src/action.mjs`, `src/action-main.mjs` | Action entry point and handlers |
| `src/bridge.mjs`, `src/bridge-main.mjs` | In-pane launcher |
| `src/events.mjs`, `src/events-main.mjs` | `worktree.removed` hook |
| `src/sandboxes-pane.mjs`, `src/sandboxes-pane-main.mjs` | Overlay |
| `src/lifecycle.mjs` | Launch, shell, stop, verify, describe |
| `src/verify.mjs`, `src/procfs.mjs` | Verification over `/proc` |
| `src/hostservice.mjs` | OpenCode host service detection |
| `src/probes.mjs` | `doctor`'s escape probe |
| `src/nono.mjs`, `src/herdr.mjs` | CLI wrappers |
| `src/agents.mjs` | Built-in and custom agents |
| `src/context.mjs` | Herdr context, workspace root |
| `src/state.mjs`, `src/config.mjs` | Mapping store, config |
| `src/result.mjs`, `src/errors.mjs` | Result line, error kinds |
| `src/naming.mjs`, `src/shell.mjs`, `src/constants.mjs` | Names, quoting, constants |

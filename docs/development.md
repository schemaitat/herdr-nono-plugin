# Development

[Link a checkout](getting-started.md#link-a-checkout-instead). The plugin is one
Rust crate that builds one binary, `herdr-nono`; `bin/run.sh` is a shell shim
that Herdr starts, which runs `bin/herdr-nono`. For development point the shim
at your build instead of installing a release:

```bash
cargo build                                          # target/debug/herdr-nono
export HERDR_NONO_BINARY="$PWD/target/debug/herdr-nono"   # for scripts/run-action.sh and tests
```

## Tests

```bash
just check                                               # fmt, clippy, cargo test, then the black-box tests
cargo test                                               # the crate's unit and pseudo-terminal tests
npm run check                                            # build, syntax check, then the black-box tests
node --test test/actions.test.mjs                        # one black-box file
node --test --test-name-pattern="stop" "test/*.test.mjs" # by name
```

`cargo test` covers the modules (including the overlay's screen and keys on
ratatui's test backend, and a pseudo-terminal run of the overlay). The
black-box tests in `test/` run the built binary against fake `nono`, `herdr`
and `opencode` in `test/fakes/`; they need neither nono nor Herdr. Node is
only their test runner. On a host with nono and the `nolabs-ai/opencode` pack,
`npm test` also runs `test/integration-egress.test.mjs` and
`test/integration-sandbox.test.mjs` against real sandboxes
([what they check](security.md#how-it-is-checked));
`HERDR_NONO_INTEGRATION=0` skips them.

## Releases

[release-please](https://github.com/googleapis/release-please) does the releasing,
from the Conventional Commits on `main` (`.github/workflows/release.yml`,
configured in `release-please-config.json`). It keeps a release pull request
open that bumps the version in `Cargo.toml`, `Cargo.lock`, `herdr-plugin.toml`
and `package.json` together and updates `CHANGELOG.md`; before 1.0 a breaking
change bumps the minor version. Merging that pull request creates the `v<version>`
tag and the GitHub release, and the same workflow then builds static (musl)
binaries for x86_64 and aarch64 Linux with `cargo-zigbuild`, writes `SHA256SUMS`
and attaches both to the release, where `scripts/install-binary.sh` finds them.
The build fails if the tag, `Cargo.toml` and `herdr-plugin.toml` disagree. A manual
run builds the files as workflow artifacts without a release.

The repository needs "Allow GitHub Actions to create and approve pull requests"
(Settings, Actions, General) for release-please to open its pull request. Pull
requests opened with the default token do not start `ci.yml`; close and reopen
the release pull request, or use a token of your own, if checks are required.

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
2. `nonorun doctor`: client egress `blocked`, server `allowlist [...]` with
   the Copilot hosts and `localhost unreachable`, every probe `ok`
   (`loopbackViaProxy: denied`). With `"serverProfile": "nolabs-ai/opencode"`,
   or a copy of the server profile with `"*"`, it must fail with `unconfined`.
3. `nonorun install-keybindings`: four bindings, then `already bound`.
4. `prefix+shift+a`: the TUI starts; `nonorun info` shows `running` and a
   passing verification; `nono ps` lists `<session>` and `<session>-server`.
5. Ask the agent to `curl` `127.0.0.1:4096` (`000`), the same through the
   proxy, `curl -x "$HTTPS_PROXY" --noproxy '' http://127.0.0.1:4096` (`403`),
   `https://api.githubcopilot.com` (any status: it got through) and
   `https://www.wikipedia.org` (`000`: the proxy refuses it), and to run `herdr pane list` (fails), then `sleep 20`. Meanwhile
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
| `herdr-plugin.toml` | Manifest: actions, hook, overlay pane, build step |
| `profiles/` | `herdr-opencode-server.json`, `herdr-opencode-client.json` |
| `bin/run.sh` | The shim Herdr starts; runs `bin/herdr-nono` |
| `scripts/install-binary.sh` | Build step: download and verify the release binary, or build from source |
| `scripts/install-keybindings.sh` | Key binding installer |
| `scripts/run-action.sh` | Run an action and wait for its result (`herdr-nono run-action`) |
| `rust/src/main.rs` | Subcommands: `action`, `bridge`, `events`, `pane`, `run-action` and hidden helpers |
| `rust/src/action.rs` | Action dispatcher and the twelve actions |
| `rust/src/bridge.rs`, `rust/src/signals.rs` | In-pane launcher and its signal handling |
| `rust/src/events.rs` | `worktree.removed` hook |
| `rust/src/lifecycle.rs` | Launch, shell, stop, verify, describe, prune |
| `rust/src/pane/mod.rs`, `rust/src/pane/app.rs` | Overlay: entry point, event loop, key state machine |
| `rust/src/pane/ui.rs`, `rust/src/pane/model.rs`, `rust/src/pane/worker.rs` | Overlay: ratatui screen, data, background worker |
| `rust/src/verify.rs`, `rust/src/procfs.rs` | Verification over `/proc` |
| `rust/src/hostservice.rs` | OpenCode host service detection |
| `rust/src/probes.rs` | `doctor`'s escape probe (also the in-sandbox `probe` subcommand) |
| `rust/src/nono.rs`, `rust/src/herdr.rs`, `rust/src/exec.rs` | CLI wrappers and the command runner |
| `rust/src/agents.rs` | Built-in and custom agents |
| `rust/src/context.rs` | Herdr context, workspace root |
| `rust/src/state.rs`, `rust/src/config.rs` | Mapping store, config |
| `rust/src/result.rs`, `rust/src/errors.rs`, `rust/src/describe.rs` | Result line, error kinds, `describe` |
| `rust/src/naming.rs`, `rust/src/shell.rs`, `rust/src/constants.rs`, `rust/src/util.rs` | Names, quoting, constants, helpers |
| `rust/src/run_action.rs` | `run-action` |
| `test/` | Black-box tests, fakes in `test/fakes/`, helpers in `test/support/` |

# Changelog

## Unreleased

- **The plugin is now a single Rust binary** (`herdr-nono`) instead of Node scripts: actions, the
  in-pane bridge, the `worktree.removed` hook and the overlay are its subcommands, and Node.js is no
  longer needed at runtime. The build step, `scripts/install-binary.sh`, downloads the release
  binary that matches the plugin version (x86_64 or aarch64 Linux, static), checks its sha256
  against the release's `SHA256SUMS`, and builds from source with `cargo` when there is no release.
  `scripts/write-node-path.sh` and `HERDR_NONO_NODE` are gone; `HERDR_NONO_BINARY` points the shim
  `bin/run.sh` at another binary (for example a `cargo build`).
- Upgrading keeps running agents: the mapping files and session names are unchanged, and a bridge
  that the Node version started is still recognised, stopped and cleaned up by the new binary.
- `doctor`'s escape probe runs as `herdr-nono probe` from a copy of the binary inside its throwaway
  sandbox, so the profile no longer has to grant Node. Its result line reports `binary` and
  `binaryVersion` where it reported `node`.
- The overlay is rebuilt on [ratatui](https://ratatui.rs): the same information and keys. Quitting
  cancels the `nono` and `herdr` calls in flight instead of waiting for them, and a startup error
  stays on screen until a key is pressed.
- `scripts/run-action.sh` is a wrapper around `herdr-nono run-action`.
- Releases are driven by release-please: it keeps a release pull request open that bumps the version
  everywhere and updates this file, and merging it creates the tag and release, after which the same
  workflow (`.github/workflows/release.yml`) builds the binaries and attaches them with `SHA256SUMS`.
- Tests: the black-box suite in `test/` drives the built binary; the modules have Rust unit tests,
  and the overlay has screen, key and pseudo-terminal tests.

- Sandboxes overlay: `i` toggles a profiles view showing, for the selected agent (or the next launch),
  each sandbox's profile, where it comes from (shipped, a file, a nono user profile, built into
  nono), its file, `extends`, network, directory grants, socket mediation and description, plus
  the profiles the next launch uses and where to change them.
- Docs: [Profiles](docs/profiles.md) covers seeing the active profiles, extending a shipped profile
  with a registered nono user profile, and copying one; the README has a "Check the profiles" step.
- Docs: [Security](docs/security.md) explains, with a reproducible check, why `herdr pane run` and
  the other Herdr socket commands cannot reach the host from either sandbox.

- **Security:** the server sandbox no longer allows every domain. nono's proxy forwards to any host
  the allowlist matches, localhost included, so `allow_domain: ["*"]` let the agent's tools reach
  the unsandboxed OpenCode host service on `127.0.0.1:4096`. The shipped server profile (0.3.0)
  now allows GitHub Copilot (`github.com`, `api.github.com`, `api.githubcopilot.com`,
  `*.githubcopilot.com`) and `models.opencode.ai` only.
- `start-agent` and `reconnect` refuse a server profile whose allowlist covers localhost (`"*"`,
  `localhost`, IP addresses, single-label and `.local`/`.internal` names, wildcard DNS services
  such as `nip.io`) even while no host service runs; `doctor` fails on them, and the overlay marks
  them `+ LOCALHOST`.
- `doctor`'s escape probe listens on the host's `127.0.0.1` and tries that port directly and through
  nono's proxy under six loopback names (`loopbackCanary`, `loopbackViaProxy`).
- `nonoArgs` rejects network flags, `--profile` and `--allow-cwd`; `agentEnv` rejects the `NONO_*`
  variables nono reads as flags, and the plugin drops them from the environment it gives nono.
- `test/integration-egress.test.mjs`: integration tests against the real nono (skipped without it).
- `test/integration-sandbox.test.mjs`: real-nono tests for the rest of the confinement: host Unix
  sockets and `herdr pane run`, leaked variables, `doctor`'s probe for both profiles, home
  directory writes, and the client/server port join, each with a control that removes the
  protection.

- Documentation site on GitHub Pages (MkDocs Material) with the page index in a left sidebar:
  eleven short pages, with dedicated pages for key bindings, the sandboxes overlay, changing the
  server and client profiles, and OpenCode's security model. The README links the site first and
  says early that the plugin is built for OpenCode.
- `justfile` with `just docs` (live preview) and `just docs-build` (strict build, as in CI).

## [0.3.0](https://github.com/schemaitat/herdr-nono-plugin/compare/v0.2.0...v0.3.0) (2026-10-06)


### Bug Fixes

* **rust:** strip the --standalone flag the OpenCode pack appends ([#9](https://github.com/schemaitat/herdr-nono-plugin/issues/9)) ([d7b4fa8](https://github.com/schemaitat/herdr-nono-plugin/commit/d7b4fa8c5a1760da63c0ff5c8c604a611e17dec4))

## [0.2.0](https://github.com/schemaitat/herdr-nono-plugin/compare/v0.1.0...v0.2.0) (2026-10-06)


### ⚠ BREAKING CHANGES

* **rust:** HERDR_NONO_NODE and bin/node-path are gone; HERDR_NONO_BINARY points the shim at another binary. doctor reports binary and binaryVersion instead of node.

### Features

* herdr plugin that runs OpenCode in nono sandboxes ([c5ad6a2](https://github.com/schemaitat/herdr-nono-plugin/commit/c5ad6a2756c117c3d5b513b942c3a647940419b4))
* interactive sandboxes overlay with client/server process view ([e232515](https://github.com/schemaitat/herdr-nono-plugin/commit/e232515e857376b55af6e3cd2849ccdfcc9e009d))
* profiles view in the overlay, profile docs, and real-nono socket confinement tests ([#4](https://github.com/schemaitat/herdr-nono-plugin/issues/4)) ([1a2228c](https://github.com/schemaitat/herdr-nono-plugin/commit/1a2228ce2b7286425f1ecdba502152e1c1253810))
* **rust:** ship the plugin as a single Rust binary ([#5](https://github.com/schemaitat/herdr-nono-plugin/issues/5)) ([4a0bbb6](https://github.com/schemaitat/herdr-nono-plugin/commit/4a0bbb6ecc119f1fee05e970c290966b7c23d8bf))


### Bug Fixes

* restrict server egress to GitHub Copilot so sandboxes cannot reach localhost ([#3](https://github.com/schemaitat/herdr-nono-plugin/issues/3)) ([30fcca5](https://github.com/schemaitat/herdr-nono-plugin/commit/30fcca58f01607d7b2c70fb8abaec9c928645ae9))

## 0.1.0 - 2026-09-29

Initial release.

- Actions: `doctor`, `install-keybindings`, `start-agent`, `reconnect`, `open-shell`, `stop`, `info`,
  `verify-sandbox`, `prune-mappings`, `sandboxes`, `list-sandboxes`, `forget-mapping`.
- OpenCode adapter that runs a private server per pane in one nono sandbox and the TUI client in
  another, joined by a port and a password the plugin picks per launch, so every tool call runs
  sandboxed and never in the host service; `reconnect` resumes with `--continue`.
- `profiles/herdr-opencode-server.json` (egress only through nono's proxy, so no direct connects to
  localhost services such as the OpenCode host service) and `profiles/herdr-opencode-client.json`
  (network blocked): the `nolabs-ai/opencode` pack plus pathname AF_UNIX mediation, stripped Herdr
  and socket variables, and no post-exit "grant denied paths" prompt.
- Process-tree verification of every launch through `/proc`, stopping sessions that fail it
  (`onVerificationFailure`), and on demand with `verify-sandbox`.
- An escape probe in `doctor` that runs inside a sandbox with the configured profile.
- Detection of the unsandboxed OpenCode host service. When a server profile leaves localhost
  open, agents are not started while it runs (`hostServiceCheck: "refuse"`), with a framed message
  in the pane and a toast, and `doctor` fails; `warn` and `off` are opt-outs.
- An escape probe check that the tools cannot connect to the host service's port.
- Custom agents with their own command, nono profile and server pattern.
- `worktree.removed` hook that stops the agents of a removed worktree and forgets their mappings.
- An interactive `sandboxes` overlay (selectable table, details panel, keys to verify, stop and
  prune with confirmation), result marker line and stable error kinds for orchestration,
  `scripts/run-action.sh` matching its invocation by `log_id`.

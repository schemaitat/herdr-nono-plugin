---
id: 261006-pwmvec
slug: rust-rewrite
status: In Progress
created: 2026-10-06
updated: 2026-10-06
areas: [runtime, overlay, distribution, tests]
summary: Rewrite the plugin from Node modules into one Rust binary (ratatui overlay, prebuilt releases), keeping state, output and test contracts intact.
files_touched: [Cargo.toml, rust/, herdr-plugin.toml, bin/run.sh, scripts/install-binary.sh, scripts/write-node-path.sh, src/, test/helpers.mjs, test/docs.test.mjs, .github/workflows/ci.yml, .github/workflows/release.yml, justfile, package.json, docs/, README.md, CHANGELOG.md]
---

# Rewrite the plugin as a single Rust binary

## Context

The plugin is about 5,400 lines of dependency-free Node (`src/*.mjs`): actions,
the in-pane bridge, the `worktree.removed` event hook, and an interactive
overlay. Herdr starts plugin commands with the server's `PATH`, which often has
no `node`, so the plugin carries a workaround: `scripts/write-node-path.sh`
records a Node binary and `bin/run.sh` launches it. Even then, `doctor` runs
its escape probe with `node -e` inside the sandbox, so the agent's profile has
to grant Node. A static binary removes that whole dependency chain. It also
gives the plugin a type system to enforce the state, result and config schemas
that the JS only checks by hand.

Constraints the rewrite must respect:

- **CON-001: State compatibility.** `state.json` entries written by the JS
  version must load unchanged. Session names and state file names come from
  `sha256` digests (`naming.mjs`, `state.mjs`) and must hash byte for byte the
  same, so an upgrade never orphans a running agent.
- **CON-002: Output contract.** The `HERDR_SANDBOX_RESULT: {...}` line
  (schemaVersion 1, `errorKind` values) and the action ids are a public
  interface that `scripts/run-action.sh`, the docs and users' scripts rely on.
  Field names and semantics stay the same.
- **CON-003: Config compatibility.** `config.json` keys, defaults and
  validation messages stay the same.
- **CON-004: Linux only.** Same as `platforms = ["linux"]` today. Target
  x86_64 and aarch64.
- **SEC-001: Probe coverage.** The escape probe must keep every check it runs
  today, and its results must stay comparable.
- **REQ-001: Overlay.** The overlay must keep its keys (verify, stop, prune,
  `i` for profiles, `q`) and its cancellable background refresh. It is built
  with **ratatui** (user decision).

## Decision

Build one crate at the repo root that produces one binary, `herdr-nono`. It
has the subcommands `action`, `bridge <mode>`, `events` and `pane`, plus two
hidden ones: `probe` (runs inside the sandbox) and `describe` (prints action
ids and constants as JSON for the docs test). The port goes bottom-up, module
by module, keeping the JS module boundaries so each Rust module can be
reviewed against the file it replaces.

The runtime is synchronous `std` plus threads, not tokio. Every external call
is a short CLI run, and only the overlay needs cancellation, which a worker
thread, a channel and `Child::kill` handle.

The overlay uses `ratatui` with `crossterm`.

The escape probe re-executes the plugin binary itself (`herdr-nono probe`).
The binary is copied into the probe's throwaway workspace, which nono already
`--allow`s, so the profile doesn't need to grant the plugin directory or Node.

Users install prebuilt static (musl) binaries from GitHub releases. The
`[[build]]` step downloads the one matching the manifest version and checks
its sha256, and falls back to `cargo build --release` when no release
matches and cargo is present.

The existing JS black-box tests switch to the binary through one helper
toggle, so they serve as the parity suite throughout. Node stays a dev and CI
dependency for those tests only, never a runtime one.

## Alternatives Considered

| Option | Why rejected |
|--------|-------------|
| ALT-001: Keep Node, fix the PATH problem only | The node-path shim already does that. What's left (probes that need Node inside the sandbox, schemas checked by hand) is the reason to move. |
| ALT-002: tokio async runtime | It adds a large dependency and colored functions for a program that makes short sequential CLI calls. The overlay's one cancellation need fits a thread and `Child::kill`. |
| ALT-003: Hand-rolled ANSI overlay (port the JS directly) | The user chose ratatui. It also removes the hand-written layout, fitting and clipping code, which is the most fragile part of `sandboxes-pane-main.mjs`. |
| ALT-004: `cargo build` at install as the only path | Every user would need a Rust toolchain, which is worse than needing Node. It stays only as a fallback. |
| ALT-005: Keep the probe as a Node script | It keeps a Node dependency inside the sandbox, which is the dependency the rewrite exists to remove. |
| ALT-006: Rewrite all tests in Rust first | It throws away the black-box suite that proves parity. The black-box tests only need a new spawn target. Unit tests that import modules are ported as `#[cfg(test)]` per module. |
| ALT-007: Big-bang rewrite in one PR | It can't be reviewed against 5k lines of JS, and nothing could be verified until the end. |

## Consequences

- Runtime needs no Node. `bin/node-path` and `scripts/write-node-path.sh`
  go away, and the `node was not found` failure class disappears.
- Releases now need a CI workflow that builds and attaches binaries. A
  manifest version without a release silently falls back to cargo.
  **RISK-001**: the Herdr build step may not have network access. The
  cargo fallback and a clear startup error cover that.
- **RISK-002: Process-ownership matching.** `processOwns` matches
  `bridge.mjs` in `/proc/<pid>/cmdline`. Bridges launched by the JS version
  must still be recognised after the upgrade, so the matcher accepts both
  forms until a later cleanup.
- **RISK-003: Hash drift.** If the digest inputs for naming or state differ
  by a single byte (newlines, hex case, slice length), lookups break. Golden
  tests pin them.
- **RISK-004: The probe copy is denied.** Some profile could forbid
  executing files from the probe workspace. Then `doctor` reports a clear
  `startup` error that names the cause instead of a false "confined".
- **ASSUMPTION-001**: Herdr runs `[[build]]` commands with network access
  (verify against Herdr ≥ 0.9 docs or by trying it).
- **ASSUMPTION-002**: A static musl binary runs under every nono profile the
  plugin ships, since it needs no dynamic loader paths.
- **DEP-001**: crates `serde`, `serde_json`, `sha2`, `getrandom`, `ratatui`,
  `crossterm`, `thiserror`, and `clap` for argv. Avoid anything heavier.
- **DEP-002**: a GitHub release workflow with `cross` or `cargo-zigbuild`
  for the aarch64-musl target.
- **DEP-003**: The repo admin must allow the release workflow to create
  releases (`contents: write`).

## Phases

| # | Phase | File | Status |
|---|-------|------|--------|
| 1 | Crate scaffold and pure foundations | [phase-01.md](phase-01.md) | Done |
| 2 | External clients, verification and the probe | [phase-02.md](phase-02.md) | Proposed |
| 3 | Lifecycle, bridge and event hook | [phase-03.md](phase-03.md) | Proposed |
| 4 | Actions | [phase-04.md](phase-04.md) | Proposed |
| 5 | ratatui overlay | [phase-05.md](phase-05.md) | Proposed |
| 6 | Distribution and cut-over | [phase-06.md](phase-06.md) | Proposed |

## Affected Files

- FILE-001: `Cargo.toml`, `Cargo.lock`: new crate, binary `herdr-nono`.
- FILE-002: `rust/src/**`: one module per JS module (`errors`, `constants`,
  `result`, `naming`, `shell`, `context`, `config`, `state`, `procfs`,
  `herdr`, `nono`, `agents`, `hostservice`, `verify`, `probes`,
  `lifecycle`, `bridge`, `events`, `action`, `pane`) and `main.rs`.
- FILE-003: `test/helpers.mjs`: spawn target switchable between
  `node src/*.mjs` and the Rust binary (`HERDR_NONO_IMPL=rust`).
- FILE-004: `test/docs.test.mjs`: read action ids and constants from
  `herdr-nono describe` instead of importing JS modules.
- FILE-005: `.github/workflows/ci.yml`: add `cargo fmt --check`,
  `clippy -D warnings`, `cargo test`, and the JS suite against the binary.
- FILE-006: `.github/workflows/release.yml`: new; builds musl binaries for
  x86_64 and aarch64 and attaches them with sha256 sums.
- FILE-007: `scripts/install-binary.sh`: new `[[build]]` step (download,
  verify, cargo fallback).
- FILE-008: `herdr-plugin.toml`: commands point at the binary; build step
  swapped.
- FILE-009: `bin/run.sh`: reduced to a builtins-only shim that execs
  `bin/herdr-nono` or prints the startup result line.
- FILE-010: `src/*.mjs`, `scripts/write-node-path.sh`: removed at
  cut-over.
- FILE-011: `justfile`, `package.json`: recipes for cargo build, test and
  lint. The package keeps test scripts only.
- FILE-012: `docs/`, `README.md`, `CHANGELOG.md`: drop Node requirements,
  document install and the probe change.

## Open Questions

- ASSUMPTION-001: Does Herdr's `[[build]]` step have network access? It
  decides whether the download is the primary path or the cargo fallback is.
- ASSUMPTION-002: Is a musl binary executable under the shipped OpenCode
  server and client profiles? Phase 2's probe work proves or refutes it.

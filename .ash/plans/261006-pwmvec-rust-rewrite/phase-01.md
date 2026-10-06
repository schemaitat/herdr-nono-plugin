---
id: 261006-pwmvec
slug: rust-rewrite
phase: 1
status: Done
---

# Phase 1 — Crate scaffold and pure foundations

## Goal

GOAL-001: Stand up the crate, CI and the modules with no external process
calls (errors, constants, result line, naming, shell quoting, plugin context,
config, state, procfs), each with its JS unit tests ported. Every later
module builds on these types.

## Why this phase exists

These modules are pure or touch only the filesystem, so they port mechanically
and can be checked line for line against their JS counterparts. Getting the
compatibility-critical pieces right here (hash inputs, state schema, result
JSON) before anything calls nono means later phases inherit proven types and
never re-litigate them. This is the first phase, so it has no predecessor.

## Steps

- [x] TASK-001: `Cargo.toml`, `rust/src/main.rs`: create the crate (binary
  `herdr-nono`, edition 2021, `[profile.release]` with `lto`, `strip`,
  `panic = "abort"`) and a `clap` skeleton with the subcommands `action`,
  `bridge`, `events`, `pane`, plus hidden `probe` and `describe`, each
  returning "not implemented".
- [x] TASK-002: `.github/workflows/ci.yml`, `justfile`: add a Rust job
  (`cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`) next to
  the Node matrix, and add `just rs-check`.
- [x] TASK-003: `rust/src/errors.rs`, `rust/src/constants.rs`: a
  `PluginError` enum (thiserror) with the same `errorKind` strings, and
  every constant from `constants.mjs`.
- [x] TASK-004: `rust/src/result.rs`: serialize the `HERDR_SANDBOX_RESULT`
  line with serde. Port `result.test.mjs`. Why: CON-002. Field order and
  null handling must match what `run-action.sh` and the docs parse.
- [x] TASK-005: `rust/src/naming.rs`, `rust/src/shell.rs`: session names,
  digests and shell quoting. Port `naming-shell.test.mjs` and add golden
  vectors captured from the JS for fixed inputs. Why: RISK-003.
- [x] TASK-006: `rust/src/context.rs`, `rust/src/config.rs`: Herdr plugin
  environment and `config.json` loading, defaults and validation, with the
  same messages. Port `context.test.mjs` and `config.test.mjs`.
- [x] TASK-007: `rust/src/state.rs`: the state file schema (serde, unknown
  fields preserved), atomic temp-file writes, revision tokens, and per-pane
  file naming. Port `state.test.mjs` and add a fixture of real JS-written
  state that must round-trip. Why: CON-001.
- [x] TASK-008: `rust/src/procfs.rs`: `/proc` readers (process start token,
  descendants, TCP sockets, fd inodes). Port the procfs parts of
  `verify.test.mjs` against the same fake `/proc` trees from
  `test/helpers.mjs`.
- [x] TASK-009: `rust/src/main.rs`: implement `describe` (action ids and
  doc-relevant constants as JSON) and add a test that compares its output with
  a snapshot generated from the JS modules.

## Trade-offs & risks

- RISK-003 is handled here with golden vectors. A mismatch found later would
  be far costlier.
- State keeps unknown fields (`#[serde(flatten)]` extra map), so a newer or
  older writer never loses data. That costs some typing strictness.

## Done criteria

- TEST-001: `cargo fmt --check`, `cargo clippy -D warnings` and
  `cargo test` are green in CI, next to the unchanged Node matrix.
- TEST-002: Golden naming and state digests equal the JS output for every
  fixture. A JS-written `state.json` loads, saves and diffs clean.
- TEST-003: `herdr-nono describe` matches the JS-derived snapshot.

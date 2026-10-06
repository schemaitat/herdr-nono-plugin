---
id: 261006-pwmvec
slug: rust-rewrite
phase: 3
status: Done
---

# Phase 3 — Lifecycle, bridge and event hook

## Goal

GOAL-003: Port `lifecycle.mjs` and ship working `bridge` and `events`
subcommands, verified by the existing black-box bridge and event tests running
against the binary. The agent can then run end to end under the Rust bridge.

## Why this phase exists

Lifecycle is the largest module (946 lines) and the heart of the plugin:
launch order of server and client sandboxes, ports, waits, signals, shells.
It gets its own phase so its review isn't diluted by action plumbing. The
bridge and the event hook are thin entry points over it and come for free.
Actions (Phase 4) spawn the bridge, so the bridge must exist first. Depends
only on Phase 2.

## Steps

- [x] TASK-017: `test/helpers.mjs`: add the `HERDR_NONO_IMPL=rust` toggle.
  When set, `runBridge`, `runEvents` and `runAction` spawn
  `target/debug/herdr-nono <subcommand>` instead of `node src/*.mjs`. Why:
  ALT-006. One switch turns the existing suite into the parity suite.
- [x] TASK-018: `rust/src/lifecycle.rs`: port process ownership
  (`processOwns`, which accepts both `bridge.mjs` and `herdr-nono bridge`
  command lines, per RISK-002), path assertions, `sandboxEnv`,
  `findExecutable`, `pickFreePort`, shell launch, and `createLifecycle`
  (start, connect, shell, stop, wait), keeping the log lines.
- [x] TASK-019: `rust/src/bridge.rs`: argv parsing identical to
  `parseBridgeArgs`, mode dispatch, signal forwarding (SIGINT, SIGTERM,
  SIGHUP to the nono child), and exit codes.
- [x] TASK-020: `rust/src/events.rs`: the `worktree.removed` handler (stop
  agents in the removed worktree, forget mappings).
- [x] TASK-021: CI: run `bridge.test.mjs` and `events.test.mjs` with
  `HERDR_NONO_IMPL=rust` and fix divergences until both implementations pass
  the same assertions.
- [x] TASK-022: Manual check with real Herdr and nono. Start an agent with
  the JS action, which launches the JS bridge, and confirm that a Rust
  `stop` and `verify-sandbox` (invoked directly) recognise and act on it.
  Why: RISK-002 proven on a live upgrade path.

## Trade-offs & risks

- RISK-002: dual-form ownership matching is kept until a follow-up removes
  the `bridge.mjs` form, one release after cut-over.
- Signal semantics differ slightly between Node and Rust (no default SIGINT
  handler in Rust). They're tested explicitly rather than assumed.

## Done criteria

- TEST-006: `bridge.test.mjs` and `events.test.mjs` pass under both
  `HERDR_NONO_IMPL` values in CI.
- TEST-007: A JS-launched bridge is stopped and verified by the Rust
  binary on a real host (TASK-022 notes recorded in the run log).

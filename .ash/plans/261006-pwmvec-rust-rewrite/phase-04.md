---
id: 261006-pwmvec
slug: rust-rewrite
phase: 4
status: Done
---

# Phase 4 — Actions

## Goal

GOAL-004: Port `action-main.mjs` so that every action id in
`herdr-plugin.toml` runs from the `action` subcommand with the same result
lines, proven by the black-box action tests. After this phase every
user-facing command except the overlay works in Rust.

## Why this phase exists

Actions are dispatch and presentation over lifecycle. Keeping them apart from
Phase 3 means a failing action test points at action code, not at launch
orchestration. The overlay comes after this phase because it calls the same
verify, stop and prune paths that actions expose. Depends only on Phase 3.

## Steps

- [x] TASK-023: `rust/src/action.rs`: dispatch on `HERDR_PLUGIN_ACTION_ID`,
  focused-mapping resolution (focused pane, then the workspace's single
  mapping, then a single orphan), and `bridgeCommand`, which now types
  `<plugin-root>/bin/herdr-nono bridge <mode> ...` into the pane.
- [x] TASK-024: `rust/src/action.rs`: `start-agent`, `reconnect`,
  `open-shell` and `stop`, including the bridge start timeout and the
  launch-id handshake.
- [x] TASK-025: `rust/src/action.rs`: `info`, `list-sandboxes`,
  `verify-sandbox`, `prune-mappings` and `forget-mapping`.
- [x] TASK-026: `rust/src/action.rs`: `doctor` (nono version warning,
  profile resolution, Rust probe from Phase 2). The `node:` line in its
  output becomes `binary: <path> (<version>)`.
- [x] TASK-027: `rust/src/action.rs`: `install-keybindings` (parse the
  keybinding report, skip existing bindings, reload) and `sandboxes`, which
  opens the overlay pane and is still backed by the JS pane until Phase 5.
- [x] TASK-028: CI: run `actions.test.mjs` with `HERDR_NONO_IMPL=rust` and
  adjust only the assertions on intentional changes (the doctor `node` line),
  each with a comment.

## Trade-offs & risks

- TASK-026 changes doctor's output on purpose. It's documented in Phase 6
  docs and CHANGELOG.
- `sandboxes` keeps launching the JS overlay for one phase. That's
  acceptable because the manifest still points at JS until Phase 6.

## Done criteria

- TEST-008: `actions.test.mjs` passes under both implementations, with only
  the documented doctor-line difference.
- TEST-009: On a real host, `scripts/run-action.sh` returns `ok` for
  `doctor`, `start-agent`, `verify-sandbox` and `stop` with the binary
  wired in by hand (a temporary local manifest edit, not committed).

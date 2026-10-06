---
id: 261006-pwmvec
slug: rust-rewrite
phase: 5
status: Done
---

# Phase 5 — ratatui overlay

## Goal

GOAL-005: Replace `sandboxes-pane-main.mjs` with a `pane` subcommand built
on ratatui and crossterm. It keeps the list, the details panel, the profiles
view, the keys and the cancellable refresh. After this phase the last Node
entry point has a Rust replacement.

## Why this phase exists

The overlay is the one module whose port isn't mechanical: it is a redesign on
ratatui (REQ-001, ALT-003). It is isolated so the UI work doesn't hold up the
headless parity work. It comes after actions because it reuses their verify,
stop and prune code paths. Depends only on Phase 4.

## Steps

- [x] TASK-029: `rust/src/pane/model.rs`: a view model built from state,
  the nono session list, Herdr panes and verification. It's pure and unit
  tested, with no terminal involved. Why: it keeps logic testable apart from
  rendering, which ratatui's `TestBackend` covers next.
- [x] TASK-030: `rust/src/pane/worker.rs`: a refresh worker thread that
  sends snapshots over a channel every 3 s. Keypress-triggered jobs (verify,
  stop, prune) use the Phase 2 cancellation handle, so `q` and Ctrl-C kill
  in-flight nono and herdr children right away.
- [x] TASK-031: `rust/src/pane/ui.rs`: the ratatui layout (sandbox table,
  details panel, the `i` profiles view, status and error line, empty and
  startup-error states held for `holdMs`), with colors that match the
  current palette.
- [x] TASK-032: `rust/src/pane/mod.rs`: the event loop (crossterm raw
  mode with a guaranteed restore on panic or exit, resize, key map), plus a
  `--once` mode that renders one frame to stdout for tests.
- [x] TASK-033: `rust/src/pane/` tests: `TestBackend` snapshot tests for
  the list, details, profiles, empty and error states. Move
  `overlay.test.mjs`'s black-box cases onto `herdr-nono pane --once` under
  the toggle.
- [x] TASK-034: `rust/src/action.rs`: `sandboxes` opens
  `bin/herdr-nono pane`.

## Trade-offs & risks

- Snapshots pin ratatui's rendering, so a ratatui upgrade may need snapshot
  refreshes. That's accepted in exchange for having layout tests at all.
- Exact visual parity with the hand-drawn JS overlay is not a goal.
  Functional parity is: the same information and the same keys.

## Done criteria

- TEST-010: Model and `TestBackend` snapshot tests pass. `overlay.test.mjs`
  passes against `pane --once`.
- TEST-011: Manually, in a real Herdr overlay (prefix+shift+o) on a remote
  terminal, list, details, `i`, verify, stop, prune and `q` all work. `q`
  during a slow verify exits within 200 ms, and the terminal is restored
  afterwards.

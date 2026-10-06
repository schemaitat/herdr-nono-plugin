---
id: 261006-pwmvec
slug: rust-rewrite
phase: 6
status: Proposed
---

# Phase 6 — Distribution and cut-over

## Goal

GOAL-006: Ship the binary to users (release workflow plus a `[[build]]`
download step with a cargo fallback), point the manifest at it, delete the
Node runtime code, and update the docs. Users stop needing Node.

## Why this phase exists

The cut-over is the only user-visible switch, so it waits until every entry
point has a tested Rust replacement (Phases 1–5). Distribution sits in the
same phase because the manifest can't point at a binary that users have no
way to get. Depends only on Phase 5.

## Steps

- [ ] TASK-035: `.github/workflows/release.yml`: on tag `v*`, build
  `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` with
  `cargo-zigbuild` and attach `herdr-nono-<version>-<target>` plus
  `SHA256SUMS` to the release. This waits on DEP-003 (an admin grants
  `contents: write`). Until then, build artifacts on a workflow dispatch
  and continue.
- [ ] TASK-036: `scripts/install-binary.sh`: builtins-plus-`curl`/`wget`
  only. Detect the arch, download the release for the manifest `version`,
  verify the sha256 and install to `bin/herdr-nono`. If that fails and
  `cargo` exists, run `cargo build --release` and copy the result. Otherwise
  print an actionable error. Add a shell test with a local fake release
  directory. Why: RISK-001 and ASSUMPTION-001.
- [ ] TASK-037: `herdr-plugin.toml`, `bin/run.sh`: the build step runs
  `install-binary.sh`. Every action, event and pane command runs
  `sh bin/run.sh <subcommand>`, and the shim now only execs
  `bin/herdr-nono` or prints the `startup` result line. Update
  `shim.test.mjs`.
- [ ] TASK-038: `test/helpers.mjs`, `test/docs.test.mjs`: make the Rust
  binary the only target, drop the toggle, and read docs facts from
  `herdr-nono describe`. Port any remaining unit tests that import
  `src/*.mjs` into Rust.
- [ ] TASK-039: `src/`, `scripts/write-node-path.sh`,
  `package.json`, `justfile`, `.github/workflows/ci.yml`: delete the JS
  runtime and node-path script. `package.json` keeps only the test runner,
  CI runs one Node version for the black-box suite, and `just check` runs
  cargo then the black-box tests.
- [ ] TASK-040: `docs/`, `README.md`, `CHANGELOG.md`: replace Node
  requirements with the install story (prebuilt binary, cargo fallback),
  and document the probe change, the doctor output change and the
  dual-form bridge matching. `just docs-build` must be strict-clean.
- [ ] TASK-041: Upgrade check on a real host: with an agent running under
  the JS plugin, update the plugin to this branch, and confirm `info`,
  `verify-sandbox`, `stop` and the overlay all handle the existing mapping
  (CON-001, RISK-002).

## Trade-offs & risks

- RISK-001 and ASSUMPTION-001: if Herdr's build step has no network, users
  without cargo can't install. The error message points them at a manual
  download.
- If DEP-003 isn't granted yet, fall back by order: finish TASK-036 through
  TASK-040 against artifacts from a dispatch run, and leave the first tagged
  release to the confirm-after-landing half below.
- The `bridge.mjs` ownership form is removed in a follow-up plan one release
  later, not here.

## Done criteria

**Provable in the tree:**

- TEST-012: `just check` is green with no `src/*.mjs` present. The CI
  Rust job and the black-box suite pass. `install-binary.sh` tests pass
  against a fake release.
- TEST-013: `just docs-build` is strict-clean and `docs.test.mjs` passes
  against `describe`.
- TEST-014: The upgrade check (TASK-041) passes on a real host with the
  binary built locally.

**Confirmed after landing (needs DEP-003, set by an admin, and a tag):**

- TEST-015: The first `v*` tag publishes both musl binaries with
  `SHA256SUMS`, and a fresh `herdr plugin install` on x86_64 downloads and
  runs it with no Node on `PATH`.

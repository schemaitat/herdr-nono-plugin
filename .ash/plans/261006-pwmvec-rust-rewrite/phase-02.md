---
id: 261006-pwmvec
slug: rust-rewrite
phase: 2
status: Proposed
---

# Phase 2 — External clients, verification and the probe

## Goal

GOAL-002: Port everything that talks to `herdr`, `nono` or inspects running
processes (herdr client, nono client, agent adapters, host-service detection,
confinement verification, escape probes). The lifecycle can then be built on
tested clients.

## Why this phase exists

Phase 1 provides the types these clients return. Lifecycle (Phase 3) is mostly
orchestration over these clients, so porting them first lets lifecycle bugs be
told apart from parsing bugs. The probe redesign (ALT-005) also lands here
because it's the riskiest unknown (RISK-004, ASSUMPTION-002) and should fail
early. Depends only on Phase 1.

## Steps

- [ ] TASK-010: `rust/src/exec.rs`: one helper that runs a CLI with a
  timeout, captures stdout and stderr as UTF-8, kills the whole process group
  on timeout, and takes an optional cancellation handle (used by the overlay
  in Phase 5). Why: three JS copies of this exist (`runCli`, `runCaptured`,
  `spawnSync` calls). One tested helper replaces them.
- [ ] TASK-011: `rust/src/herdr.rs`: the Herdr CLI client (panes, splits,
  typing the command, reload, keybindings). Port its behaviour against
  `test/fakes/herdr.mjs`.
- [ ] TASK-012: `rust/src/nono.rs`: the nono client (run, stop, session list
  normalisation, `classifyFailure`, profile summary, version parsing). Port
  `nono.test.mjs` against `test/fakes/nono.mjs`.
- [ ] TASK-013: `rust/src/agents.rs`: agent adapters (OpenCode default,
  resume args, profiles, env). Port `agents.test.mjs`.
- [ ] TASK-014: `rust/src/hostservice.rs`, `rust/src/verify.rs`: host
  service detection (reads URL, pid and port, never the password) and
  confinement verification (`looksConfined`, summaries). Port the rest of
  `verify.test.mjs`.
- [ ] TASK-015: `rust/src/probes.rs` and the `probe` subcommand: the
  in-sandbox checks re-implemented in Rust (socket connects, loopback canary
  direct and via proxy, home secrets existence, env leakage, host-service
  password readability), with the same `HERDR_NONO_PROBE` JSON line. The
  runner copies `current_exe()` into the probe workspace and runs it under
  `nono run --allow <workspace>`. Port `probes.test.mjs`. Why: SEC-001 and
  ALT-005. It also proves ASSUMPTION-002.
- [ ] TASK-016: `test/integration-sandbox.test.mjs`,
  `test/integration-egress.test.mjs`: run against real nono with the Rust
  probe (JS harness, binary target) and compare the check-by-check results
  with the Node probe on the same machine.

## Trade-offs & risks

- RISK-004: if a profile forbids executing from the workspace, the runner
  reports a `startup` error that names the cause. No fallback to Node is
  added, so the probe never silently passes.
- The JS fakes stay Node scripts. Node is a test-only dependency from here on.

## Done criteria

- TEST-004: All ported unit tests pass. Client behaviour against the fake
  herdr and nono matches the JS tests' expectations.
- TEST-005: On a host with real nono, the Rust probe reports the same
  verdict for every check as the Node probe, under the shipped server and
  client profiles.

---
id: 261006-pwmvec
slug: rust-rewrite
updated: 2026-10-06
areas: [runtime, overlay, distribution, tests]
issue_count: 8
---

# Learnings — Rewrite the plugin as a single Rust binary (261006-pwmvec-rust-rewrite)

## Source
- Plan: `./README.md`
- Basis: session context, cross-checked against the run log
- Logs consulted: `logs/20261006T184148Z-claude-opus-5-5.log`

## Summary
All six phases were implemented and committed: a Rust crate with the actions, bridge, event hook and a
ratatui overlay, a download-and-verify install step with a cargo fallback, a release workflow, and the Node
runtime deleted. The existing black-box tests, pointed at the binary, were the parity suite and found few
real differences. The criteria that needed a real Herdr or a tagged release could not be done as written
(ISSUE-006, ISSUE-007); TEST-015 is open until the first tag. The headline lesson: the bugs that cost time
were in test and shell plumbing (races, shared shell variables), not in the port.

## Issues

### ISSUE-001: Parallel tests that write and run fake scripts fail with "Text file busy"
**What happened:** a unit test that creates a fake `herdr` script and runs it failed about 1 run in 80.
**Root cause:** another test thread forked while this one still had the script open for writing; exec of a
file open for writing returns ETXTBSY until that child execs.
**Fix applied:** the command runner retries a spawn on ETXTBSY (with a deterministic test that holds a write
handle open), which also hardens real use.
**Recommendation:** when tests create executables in parallel, expect ETXTBSY and retry in the spawn helper
rather than adding sleeps; find such flakes by running the suite 50-100 times, not 3.
**Skill:** none
**Distilled:** promoted — LESSON-001

### ISSUE-002: Tests that expect "connection refused" on a just-closed port are racy
**What happened:** two probe tests failed about 3 times in 80 runs.
**Root cause:** the port freed by one test was handed to a listener in a parallel test before the refusal check.
**Fix applied:** the tests retry with fresh ports and pass if any attempt sees the refusal.
**Recommendation:** never assert on a specific ephemeral port after closing it; assert the property over a few fresh ports.
**Skill:** none
**Distilled:** promoted — LESSON-001

### ISSUE-003: A direct crossterm dependency does not match ratatui's
**What happened:** `cargo add ratatui crossterm` resolved crossterm 0.29 while ratatui 0.29 uses 0.28.
**Root cause:** ratatui pins its own crossterm; two versions mean two incompatible sets of types.
**Fix applied:** dropped the direct dependency and used `ratatui::crossterm`.
**Recommendation:** use the backend crate a UI library re-exports instead of adding it separately.
**Skill:** none
**Distilled:** declined — specific to this dependency pair, and cargo would show it at compile time.

### ISSUE-004: A shell function overwrote its caller's variable
**What happened:** `install-binary.sh` reported "checksum of SHA256SUMS does not match SHA256SUMS".
**Root cause:** POSIX sh has no function-local variables; `fetch` assigned `asset`, which `download` was using.
**Fix applied:** unique variable names in the helper, with a comment; the new tests found it at once.
**Recommendation:** treat every variable in a POSIX shell function as global (prefix them), and test shell
scripts before relying on them as a build step.
**Skill:** none
**Distilled:** promoted — LESSON-002

### ISSUE-005: The plan missed a consumer of the code being removed
**What happened:** `scripts/run-action.sh` parsed Herdr's JSON by running Node through the shim, so deleting
the Node runtime would have broken it. Found while planning phase 6 work, not in the plan.
**Root cause:** the plan's file list came from the module survey, not from a search for everything that
invokes the runtime.
**Fix applied:** a `run-action` subcommand, with the script reduced to a wrapper.
**Recommendation:** when a plan removes something, grep the whole repo (scripts, workflows, docs, tests, the
manifest) for every consumer before writing the phases.
**Skill:** plan-write
**Distilled:** promoted — LESSON-003

### ISSUE-006: Fakes keyed to the old implementation's details
**What happened:** pointing the black-box tests at the binary broke three reconnect tests and doctor; the
fake herdr acknowledged a typed command only if it contained `bridge.mjs`, and the fake nono recognised the
probe by the Node script's text.
**Root cause:** the fakes encoded how the old implementation worked rather than what it did.
**Fix applied:** the fakes accept the new forms; assertions about the typed command go through one helper.
**Recommendation:** key fakes on behaviour (subcommand names), and expect a handful of such adaptations when
swapping an implementation behind a black-box suite; the suite still caught the real differences.
**Skill:** none
**Distilled:** declined — specific to these fakes.

### ISSUE-007: "Real host" criteria assumed a real Herdr and an installed OpenCode
**What happened:** TASK-022, TASK-034, TEST-009 and TEST-011 were written as runs through a real Herdr
overlay and OpenCode. Doing that here would have split panes in the user's live Herdr session, and OpenCode
is not installed.
**Root cause:** the plan did not say what a safe real-host check is on a machine that is also the user's
working environment.
**Fix applied:** the same flows ran against the real nono with a `sleep` agent, the real read-only
`herdr pane list`, a fake herdr elsewhere, and a pseudo-terminal for the overlay; the deviations are logged.
**Recommendation:** when a criterion needs a live system, write the safe form into the plan (what may be
touched, what may not) and keep a pseudo-terminal or loopback harness as the repeatable version.
**Skill:** plan-write
**Distilled:** promoted — LESSON-004

### ISSUE-008: spawnSync blocks an in-process HTTP server
**What happened:** a test that ran the install script (curl) against a Node HTTP server in the same process
hung until the test runner gave up.
**Root cause:** `spawnSync` blocks the event loop, so the server could not answer the child.
**Fix applied:** an async spawn for any child that talks to an in-process server.
**Recommendation:** use the async spawn whenever the child needs something the test process serves.
**Skill:** none
**Distilled:** declined — a well-known Node pitfall, cheap to recognise on first hang.

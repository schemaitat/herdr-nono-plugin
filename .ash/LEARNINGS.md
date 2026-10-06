# Distilled learnings

Lessons that generalize beyond the plan that produced them. Read this
before planning new work.

### LESSON-001: Run a suite many times before calling it stable, and expect races in tests that share the machine
**Lesson:** after adding tests that create executables, bind ports or fork in parallel, run the suite 50-100
times; fix ETXTBSY with a retry in the spawn helper and never assert on a specific ephemeral port after
closing it.
**Why:** both races fail 1-4% of runs, so three green runs prove nothing, and they surface only under the
parallelism the real suite uses.
**Status:** prose
**Seen in:** 261006-pwmvec-rust-rewrite (ISSUE-001, ISSUE-002)

### LESSON-002: Treat every variable in a POSIX shell function as global, and test shell scripts that act as build steps
**Lesson:** prefix helper-function variables with the function name, and give shell scripts that install or
build things their own tests before depending on them.
**Why:** POSIX sh has no `local`, so a helper silently overwrites its caller's variables, and a build step
that misbehaves is found by users first.
**Status:** prose
**Seen in:** 261006-pwmvec-rust-rewrite (ISSUE-004)

### LESSON-003: When a plan removes something, search the whole repo for its consumers first
**Lesson:** before writing the phases, grep scripts, workflows, docs, tests and the manifest for every use of
what is being removed, and list each hit as a file in the plan.
**Why:** a module survey finds the code's own files, not the shell wrappers and tooling that call it.
**Status:** prose
**Seen in:** 261006-pwmvec-rust-rewrite (ISSUE-005)

### LESSON-004: Say in the plan what a safe real-host check is
**Lesson:** for any criterion that needs a live system the user also works in, write down what may be
touched and what may not, and keep a repeatable harness (a pseudo-terminal, a loopback fake) beside it.
**Why:** otherwise the choice is made under pressure at the boundary, and the check either alters the user's
session or is quietly skipped.
**Status:** prose
**Seen in:** 261006-pwmvec-rust-rewrite (ISSUE-007)

import assert from "node:assert/strict";
import { test } from "node:test";
import { createFixture, fakeBridgeProcess, fakeShellProcess, mappingFor, runEvent } from "./helpers.mjs";

test("worktree.removed forgets the idle mappings of that worktree and nothing else", () => {
  const f = createFixture({
    panes: ({ worktree, root }) => ({
      "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1" }),
      "w1:p2": mappingFor({ worktree: root }, { paneId: "w1:p2", sessionName: "herdr-opencode-000000000002", localPath: root, workdir: root }),
    }),
  });
  const { status, stdout } = runEvent(f, "worktree.removed", { data: { worktree: { path: f.worktree } } });
  assert.equal(status, 0, stdout);
  assert.match(stdout, /forgot mapping for pane w1:p1/);
  assert.deepEqual(Object.keys(f.mappings().panes), ["w1:p2"]);
  assert.ok(f.herdrCalls().some((call) => call[0] === "notification"));
  f.cleanup();
});

test("worktree.removed stops a running agent of that worktree before forgetting it", () => {
  const bridge = fakeBridgeProcess("w1:p1");
  const f = createFixture({
    sessions: [{ session_id: "s1", name: "herdr-opencode-abc123def456", supervisor_pid: bridge.pid, status: "running" }],
    panes: ({ worktree }) => ({ "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1", bridgePid: bridge.pid, lifecycleState: "running" }) }),
  });
  // The fake nono's stop kills the recorded supervisor, which here is the fake bridge itself.
  const { status, stdout } = runEvent(f, "worktree.removed", { data: { worktree: { path: f.worktree } } });
  bridge.stop();
  assert.equal(status, 0, stdout);
  assert.match(stdout, /stopped herdr-opencode-abc123def456/);
  assert.deepEqual(f.nonoCalls().filter((call) => call.argv[0] === "stop").map((call) => call.argv), [["stop", "s1"]]);
  assert.deepEqual(Object.keys(f.mappings().panes), []);
  f.cleanup();
});

test("worktree.removed keeps a mapping whose shell still runs, and respects cleanupOnWorktreeRemoved", () => {
  const shell = fakeShellProcess("w1:p1");
  const f = createFixture({ panes: ({ worktree }) => ({ "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1", shellPids: [{ pid: shell.pid, paneId: "w1:p1" }] }) }) });
  const { stdout } = runEvent(f, "worktree.removed", { data: { worktree: { path: f.worktree } } });
  shell.stop();
  assert.match(stdout, /kept mapping for pane w1:p1/);
  const off = createFixture({ config: { cleanupOnWorktreeRemoved: false }, panes: ({ worktree }) => ({ "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1" }) }) });
  assert.match(runEvent(off, "worktree.removed", { data: { worktree: { path: off.worktree } } }).stdout, /cleanupOnWorktreeRemoved is false/);
  assert.deepEqual(Object.keys(off.mappings().panes), ["w1:p1"]);
  assert.match(runEvent(off, "workspace.closed", {}).stdout, /ignoring event workspace\.closed/);
  f.cleanup();
  off.cleanup();
});

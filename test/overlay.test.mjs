import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import path from "node:path";
import { test } from "node:test";
import { collectSandboxes, renderSandboxes } from "../src/sandboxes-pane-main.mjs";
import { ROOT, createFixture, mappingFor } from "./helpers.mjs";

const NAME = "herdr-opencode-abc123def456";

test("collectSandboxes merges mappings, live sessions and panes with one call each", async () => {
  const f = createFixture({
    panes: ({ worktree }) => ({
      "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1", lifecycleState: "running", verification: { ok: false, supported: true, processes: [], problems: ["Process 7 is not confined"] } }),
      "w1:p2": mappingFor({ worktree }, { paneId: "w1:p2", sessionName: "herdr-opencode-000000000002" }),
    }),
  });
  let nonoCalls = 0;
  let herdrCalls = 0;
  const nono = { async listSessions() { nonoCalls += 1; return [{ sessionId: "s1", name: NAME, supervisorPid: 5, status: "running" }, { sessionId: "s2", name: `${NAME}-shell`, status: "running" }, { sessionId: "s3", name: "herdr-opencode-000000000002", status: "exited" }]; } };
  const herdr = { async listPaneIds() { herdrCalls += 1; return ["w1:p1"]; } };
  const { rows, sessionError } = await collectSandboxes({ stateDir: f.stateDir, nono, herdr });
  assert.equal(sessionError, null);
  assert.equal(nonoCalls, 1);
  assert.equal(herdrCalls, 1);
  const byPane = Object.fromEntries(rows.map((row) => [row.paneId, row]));
  assert.equal(byPane["w1:p1"].running, true);
  assert.equal(byPane["w1:p1"].shells, 1);
  assert.equal(byPane["w1:p1"].verified, false);
  assert.equal(byPane["w1:p2"].running, false, "an exited session does not count");
  assert.equal(byPane["w1:p2"].paneExists, false);
  const lines = renderSandboxes({ rows, sessionError }, { at: new Date(0) });
  const text = lines.join("\n");
  assert.match(text, /running \+1sh/);
  assert.match(text, /FAILED/);
  assert.match(text, /herdr-opencode-abc123def456: FAILED: Process 7 is not confined/);
  assert.match(text, /w1:p2 \(gone\)/);
  assert.match(text, /1 stale mapping \(pane gone, nothing running\): run prune-mappings/);
  f.cleanup();
});

test("the overlay reports a failing nono and an empty state", async () => {
  const f = createFixture();
  const nono = { async listSessions() { throw new Error("nono ps failed (exit 1)"); } };
  const herdr = { async listPaneIds() { return []; } };
  const lines = renderSandboxes(await collectSandboxes({ stateDir: f.stateDir, nono, herdr }));
  assert.match(lines.join("\n"), /No sandboxed agents are mapped/);
  assert.match(lines.join("\n"), /nono ps failed: nono ps failed \(exit 1\)/);
  f.cleanup();
});

test("the overlay entry point renders one frame with --once through the real clients", () => {
  const f = createFixture({
    sessions: [{ session_id: "s1", name: NAME, supervisor_pid: 5, status: "running" }],
    panes: ({ worktree }) => ({ "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1", lifecycleState: "running" }) }),
  });
  const result = spawnSync(process.execPath, [path.join(ROOT, "src", "sandboxes-pane.mjs"), "--once"], { cwd: ROOT, encoding: "utf8", env: f.env() });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /nono sandboxes {2}1 mapping/);
  assert.match(result.stdout, /w1:p1 +herdr-opencode-abc123def456 +opencode +running +running/);
  f.cleanup();
});

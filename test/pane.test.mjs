import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { test } from "node:test";
import { createFixture, mappingFor, paneCommand } from "./helpers.mjs";

const NAME = "herdr-opencode-abc123def456";
const ESC = String.fromCharCode(27);

test("the overlay entry point renders one plain frame with --once through the real clients", () => {
  const f = createFixture({
    sessions: [{ session_id: "s1", name: NAME, supervisor_pid: 5, status: "running" }, { session_id: "s2", name: `${NAME}-server`, supervisor_pid: 6, status: "running" }],
    panes: ({ worktree }) => ({ "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1", lifecycleState: "running" }) }),
  });
  const [command, args] = paneCommand(["--once"]);
  const result = spawnSync(command, args, { cwd: f.root, encoding: "utf8", env: f.env() });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /nono sandboxes .*1 agent · 1 running/);
  assert.match(result.stdout, /┌─ client · /);
  assert.match(result.stdout, /▶● {2}w1:p1 +…abc123def456 +opencode +running +client\+server/);
  assert.doesNotMatch(result.stdout, new RegExp(`${ESC}\\[`), "--once prints no escape codes");
  f.cleanup();
});

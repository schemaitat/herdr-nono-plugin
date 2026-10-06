/**
 * Upgrade path: an agent started by the Node version's bridge is still
 * recognised, stopped and cleaned up by the Rust binary. The bridge here is
 * the Node script, the cleanup is the Rust `events` hook; the test is skipped
 * when the Rust binary has not been built (cargo build).
 */
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { test } from "node:test";
import { FAKE_HERDR, ROOT, RUST_BIN, createFixture, mappingFor } from "./helpers.mjs";

const SKIP = existsSync(RUST_BIN) ? false : `no Rust binary at ${RUST_BIN}; run cargo build`;

async function until(check, what, timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = check();
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

test("the Rust events hook recognises, stops and forgets an agent that the Node bridge started", { skip: SKIP }, async () => {
  const f = createFixture({
    config: { verifyAfterStart: false },
    panes: ({ worktree }) => ({ "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1", lifecycleState: "provisional", launchCount: 0 }) }),
  });
  const args = [path.join(ROOT, "src", "bridge.mjs"), "start", "--state-dir", f.stateDir, "--config-dir", f.configDir, "--pane-id", "w1:p1", "--plugin-root", ROOT, "--herdr-bin", FAKE_HERDR];
  const bridge = spawn(process.execPath, args, { cwd: ROOT, env: f.env({ FAKE_AGENT_SLEEP_MS: "60000" }), stdio: "ignore" });
  const exited = new Promise((resolve) => bridge.once("exit", (code, signal) => resolve({ code, signal })));
  try {
    const running = await until(() => {
      const entry = f.mappings().panes["w1:p1"];
      return entry?.bridgePid === bridge.pid && entry.lifecycleState === "running" && entry.supervisorPid ? entry : null;
    }, "the Node bridge to start the agent");
    assert.match(running.bridgeToken, /^linux:\d+$/, "the Node bridge records its start token in the format the Rust binary reads");

    const result = spawnSync(RUST_BIN, ["events"], {
      cwd: ROOT,
      encoding: "utf8",
      env: f.env({ HERDR_PLUGIN_EVENT: "worktree.removed", HERDR_PLUGIN_EVENT_JSON: JSON.stringify({ data: { worktree: { path: f.worktree } } }) }),
    });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.match(result.stdout, /stopped herdr-opencode-abc123def456/);
    assert.match(result.stdout, /forgot mapping for pane w1:p1/);
    const timer = setTimeout(() => bridge.kill("SIGKILL"), 15_000);
    const { signal } = await exited;
    clearTimeout(timer);
    assert.notEqual(signal, "SIGKILL", "the Node bridge exits on its own after the stop");
    assert.deepEqual(Object.keys(f.mappings().panes), [], "the mapping is gone");
    assert.ok(f.nonoCalls().some((call) => call.argv[0] === "stop"), "the session was stopped through nono");
  } finally {
    bridge.kill("SIGKILL");
    f.cleanup();
  }
});

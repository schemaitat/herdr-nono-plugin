import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { BINARY, ROOT, createFixture, runAction } from "./helpers.mjs";

const SHIM = path.join(ROOT, "bin", "run.sh");

function executable(file, body) {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, `#!/bin/sh\n${body}\n`);
  chmodSync(file, 0o755);
  return file;
}

test("the shim runs bin/herdr-nono next to it with the arguments it was given", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "herdr-nono-shim-"));
  copyFileSync(SHIM, path.join(dir, "run.sh"));
  executable(path.join(dir, "herdr-nono"), 'printf "ran:%s:%s" "$1" "$2"');
  const result = spawnSync("sh", [path.join(dir, "run.sh"), "bridge", "a b"], { encoding: "utf8", env: { PATH: "/usr/bin:/bin" } });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, "ran:bridge:a b");
});

test("HERDR_NONO_BINARY overrides where the shim looks", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "herdr-nono-shim-"));
  copyFileSync(SHIM, path.join(dir, "run.sh"));
  executable(path.join(dir, "herdr-nono"), "echo next-to-the-shim");
  const other = executable(path.join(dir, "other"), "echo override");
  const result = spawnSync("sh", [path.join(dir, "run.sh")], { encoding: "utf8", env: { PATH: "/usr/bin:/bin", HERDR_NONO_BINARY: other } });
  assert.equal(result.stdout.trim(), "override");
});

test("the shim needs nothing but shell builtins, so an empty PATH cannot break it", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "herdr-nono-shim-"));
  copyFileSync(SHIM, path.join(dir, "run.sh"));
  executable(path.join(dir, "herdr-nono"), "echo built-ins-only");
  const result = spawnSync("/bin/sh", [path.join(dir, "run.sh")], { encoding: "utf8", env: { PATH: "/nonexistent" } });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout.trim(), "built-ins-only");
  assert.equal(result.stderr, "", "no missing-command noise");
});

test("a missing binary is explained, with the result marker for actions", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "herdr-nono-shim-"));
  copyFileSync(SHIM, path.join(dir, "run.sh"));
  const action = spawnSync("/bin/sh", [path.join(dir, "run.sh"), "action"], { encoding: "utf8", env: { PATH: "/nonexistent", HERDR_PLUGIN_ACTION_ID: "doctor" } });
  assert.equal(action.status, 127);
  assert.match(action.stderr, /install-binary\.sh/);
  assert.ok(action.stdout.startsWith('HERDR_SANDBOX_RESULT: {"schemaVersion":1,"plugin":"nono.sandbox","action":"doctor","ok":false,"errorKind":"startup"'), action.stdout);
  assert.match(JSON.parse(action.stdout.slice("HERDR_SANDBOX_RESULT: ".length)).message, /is missing; run sh scripts\/install-binary\.sh/);
  const pane = spawnSync("/bin/sh", [path.join(dir, "run.sh"), "pane"], { encoding: "utf8", env: { PATH: "/nonexistent" } });
  assert.equal(pane.status, 127);
  assert.equal(pane.stdout, "", "only actions print a result line");
  assert.match(pane.stderr, /HERDR_NONO_BINARY/);
});

test("actions run end to end through the shim as the manifest declares", () => {
  const f = createFixture();
  const result = spawnSync("sh", [SHIM, "action"], { cwd: ROOT, encoding: "utf8", env: f.env({ HERDR_PLUGIN_ACTION_ID: "doctor", HERDR_PLUGIN_CONTEXT_JSON: "{}", HERDR_NONO_BINARY: BINARY }) });
  assert.equal(result.status, 0, result.stderr);
  assert.ok(result.stdout.startsWith("HERDR_SANDBOX_RESULT:"));
  assert.equal(runAction(f, "doctor").result.ok, true);
  f.cleanup();
});

test("run-action.sh invokes an action and waits for the log entry of that very invocation", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "herdr-nono-run-action-"));
  const logs = path.join(dir, "logs.json");
  const env = { PATH: `${path.dirname(process.execPath)}:/usr/bin:/bin`, HERDR_NONO_BINARY: BINARY, HERDR_BIN_PATH: path.join(ROOT, "test", "fakes", "herdr.mjs"), FAKE_HERDR_ACTION_LOGS: logs, FAKE_HERDR_LOG: path.join(dir, "herdr.log"), FAKE_HERDR_LOG_ID: "plugin-log-7" };
  writeFileSync(logs, JSON.stringify([
    { action_id: "info", log_id: "plugin-log-7", status: "succeeded", stdout: 'HERDR_SANDBOX_RESULT: {"schemaVersion":1,"plugin":"nono.sandbox","action":"info","ok":true,"paneId":"p1"}\nmore output\n', stderr: "pane p0 has no sandboxed agent; using the workspace's only mapping\n" },
    { action_id: "info", log_id: "plugin-log-6", status: "succeeded", stdout: 'HERDR_SANDBOX_RESULT: {"ok":true,"old":true}\n', stderr: "" },
  ]));
  const ok = spawnSync("sh", [path.join(ROOT, "scripts", "run-action.sh"), "info", "5"], { encoding: "utf8", env });
  assert.equal(ok.status, 0, ok.stderr);
  assert.equal(ok.stdout, 'HERDR_SANDBOX_RESULT: {"schemaVersion":1,"plugin":"nono.sandbox","action":"info","ok":true,"paneId":"p1"}\n');
  assert.match(ok.stderr, /using the workspace's only mapping/);
  const calls = readFileSync(path.join(dir, "herdr.log"), "utf8").trim().split("\n").map((line) => JSON.parse(line).argv.slice(0, 3).join(" "));
  assert.equal(calls[0], "plugin action invoke");
  // An older finished run of the same action must not be mistaken for this one.
  writeFileSync(logs, JSON.stringify([{ action_id: "reconnect", log_id: "plugin-log-6", status: "succeeded", stdout: 'HERDR_SANDBOX_RESULT: {"ok":true}\n', stderr: "" }]));
  const stale = spawnSync("sh", [path.join(ROOT, "scripts", "run-action.sh"), "reconnect", "3"], { encoding: "utf8", env });
  assert.equal(stale.status, 2, "waits for plugin-log-7 and times out");
  assert.match(stale.stderr, /did not finish within 3 seconds/);
  writeFileSync(logs, JSON.stringify([{ action_id: "stop", log_id: "plugin-log-7", status: "failed", stdout: 'HERDR_SANDBOX_RESULT: {"ok":false,"errorKind":"conflict","message":"busy"}\n', stderr: "" }]));
  const failed = spawnSync("sh", [path.join(ROOT, "scripts", "run-action.sh"), "stop", "5"], { encoding: "utf8", env });
  assert.equal(failed.status, 1, "a result with ok:false exits 1");
  assert.match(failed.stdout, /"errorKind":"conflict"/);
  writeFileSync(logs, JSON.stringify([{ action_id: "doctor", log_id: "plugin-log-7", status: "running", stdout: "", stderr: "" }]));
  const timedOut = spawnSync("sh", [path.join(ROOT, "scripts", "run-action.sh"), "doctor", "6"], { encoding: "utf8", env });
  assert.equal(timedOut.status, 2);
  assert.match(timedOut.stderr, /still running/);
  assert.match(timedOut.stderr, /did not finish within 6 seconds/);
  assert.equal(spawnSync("sh", [path.join(ROOT, "scripts", "run-action.sh")], { encoding: "utf8", env }).status, 2, "usage error");
});

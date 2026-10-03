import assert from "node:assert/strict";
import { test } from "node:test";
import { buildRunArgs, classifyFailure, createNonoClient, normalizeSessionList } from "../src/nono.mjs";
import { FAKE_NONO } from "./helpers.mjs";

test("buildRunArgs grants the workspace root explicitly and puts the agent after --", () => {
  assert.deepEqual(
    buildRunArgs({ profile: "/p.json", sessionName: "herdr-opencode-1", workspaceRoot: "/repo", allowPaths: ["/data"], readPaths: ["/ref"], silent: true, extraArgs: ["--memory", "2G"], argv: ["opencode", "--standalone"] }),
    ["run", "--silent", "--profile", "/p.json", "--name", "herdr-opencode-1", "--allow", "/repo", "--allow", "/data", "--read", "/ref", "--memory", "2G", "--", "opencode", "--standalone"],
  );
  const plain = buildRunArgs({ profile: "opencode", sessionName: "s1", workspaceRoot: "/repo", argv: ["bash"] });
  assert.ok(!plain.includes("--allow-cwd"), "the pane's directory is never granted implicitly");
  assert.throws(() => buildRunArgs({ profile: "p", sessionName: "s", workspaceRoot: "/r", argv: [] }), /needs the command/);
});

test("classifyFailure maps nono's wording", () => {
  assert.equal(classifyFailure("nono: Session not found: abc"), "not-found");
  assert.equal(classifyFailure("nono: Profile not found: x"), "not-found");
  assert.equal(classifyFailure("Permission denied"), "permission");
  assert.equal(classifyFailure("error: unexpected argument '--x' found"), "config");
  assert.equal(classifyFailure("something else"), "unknown");
});

test("normalizeSessionList accepts nono's array and refuses other shapes", () => {
  const [session] = normalizeSessionList([{ session_id: "ab12", name: "herdr-opencode-1", supervisor_pid: 10, child_pid: 11, status: "running", command: ["opencode", "--standalone"], profile: "/p.json", workdir: "/repo" }]);
  assert.equal(session.sessionId, "ab12");
  assert.equal(session.supervisorPid, 10);
  assert.deepEqual(session.command, ["opencode", "--standalone"]);
  assert.deepEqual(normalizeSessionList(null), []);
  assert.deepEqual(normalizeSessionList({ sessions: [] }), []);
  assert.throws(() => normalizeSessionList({ weird: true }), /unexpected shape/);
  assert.throws(() => normalizeSessionList([1, 2]), /unexpected shape/);
});

test("the client reads the version, lists sessions and classifies failures", () => {
  const nono = createNonoClient({ bin: FAKE_NONO, env: { ...process.env, FAKE_NONO_VERSION: "0.79.1" } });
  assert.equal(nono.version().version, "0.79.1");
  assert.deepEqual(nono.listSessions(), []);
  assert.throws(() => nono.stop("nothing"), (error) => error.errorKind === "not-found");
  const failing = createNonoClient({ bin: FAKE_NONO, env: { ...process.env, FAKE_NONO_FAIL: "profile:not-found" } });
  assert.throws(() => failing.showProfile("missing"), (error) => error.errorKind === "not-found" && /Profile not found/.test(error.output));
  const missing = createNonoClient({ bin: "/definitely/not/nono" });
  assert.throws(() => missing.version(), (error) => error.errorKind === "startup" && /Install nono/.test(error.message));
});

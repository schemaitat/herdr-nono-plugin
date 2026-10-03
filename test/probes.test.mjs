import assert from "node:assert/strict";
import { test } from "node:test";
import { PROBE_EXPECTATIONS, evaluateProbes, probeTargets, runProbes } from "../src/probes.mjs";
import { FAKE_NONO } from "./helpers.mjs";

test("probeTargets aims at Herdr's socket, the user's buses and agents, and the OpenCode service file", () => {
  const targets = probeTargets({ HOME: "/home/u", XDG_RUNTIME_DIR: "/run/user/1000", HERDR_SOCKET_PATH: "/home/u/.config/herdr/herdr.sock" });
  assert.equal(targets.sockets.herdrSocket, "/home/u/.config/herdr/herdr.sock");
  assert.equal(targets.sockets.systemdUser, "/run/user/1000/systemd/private");
  assert.equal(targets.sockets.sessionBus, "/run/user/1000/bus");
  assert.equal(targets.sockets.sshAgent, "/run/user/1000/openssh_agent");
  assert.equal(targets.files.opencodeServicePassword, "/home/u/.local/state/opencode/service.json");
  assert.equal(probeTargets({ HOME: "/home/u" }).sockets.herdrSocket, "/home/u/.config/herdr/herdr.sock", "falls back to Herdr's default socket");
});

test("evaluateProbes flags reachable sockets and leaked variables with their severity", () => {
  const checks = evaluateProbes({ herdrSocket: "allowed", sessionBus: "denied", sshKeys: "denied", homeDirectory: "allowed", opencodeServicePassword: "allowed", env: ["HERDR_SOCKET_PATH"], marker: true });
  const byName = Object.fromEntries(checks.map((check) => [check.check, check]));
  assert.equal(byName.herdrSocket.ok, false);
  assert.equal(byName.herdrSocket.severity, "critical");
  assert.equal(byName.sessionBus.ok, true);
  assert.equal(byName.homeDirectory.ok, false);
  assert.equal(byName.opencodeServicePassword.severity, "warning");
  assert.equal(byName.env.ok, false);
  assert.equal(byName.env.result, "HERDR_SOCKET_PATH");
  assert.equal(byName.marker.ok, true);
  assert.equal(evaluateProbes({ marker: false }).find((check) => check.check === "marker").ok, false);
  assert.ok(Object.keys(PROBE_EXPECTATIONS).includes("dockerSocket"));
});

test("runProbes goes through nono run with the profile and parses the probe line", () => {
  const { checks } = runProbes({ nonoBin: FAKE_NONO, profile: "/p.json", env: { ...process.env, HOME: "/nonexistent" } });
  assert.ok(checks.every((check) => check.check === "opencodeServicePassword" || check.ok), JSON.stringify(checks));
  assert.throws(() => runProbes({ nonoBin: FAKE_NONO, profile: "/p.json", env: { ...process.env, FAKE_NONO_FAIL: "run" } }), /did not report/);
});

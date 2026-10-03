import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { resolveAgent } from "../src/agents.mjs";
import { detectOpencodeService, hostServiceWarning } from "../src/hostservice.mjs";
import { decodeTcpAddress, descendants, parentFromStat, readProcess, tcpConnections } from "../src/procfs.mjs";
import { findClient, summarizeVerification, verifySession } from "../src/verify.mjs";
import { writeFakeProc } from "./helpers.mjs";

const OPENCODE = resolveAgent({ agentKind: "opencode" }, { pluginRoot: "/plugin" });

function tcpLine(local, remote, state, inode) {
  return `   0: ${local} ${remote} ${state} 00000000:00000000 00:00000000 00000000  1000        0 ${inode} 1 0000000000000000 20 4 30 10 -1`;
}

const PORT = 4242;

/** A client sandbox (100 -> 101) and a server sandbox (110 -> 111 -> 112), plus the host service (200). */
function procTree(extra = [], options = {}) {
  const root = mkdtempSync(path.join(tmpdir(), "herdr-nono-proc-"));
  writeFakeProc(root, [
    { pid: 100, ppid: 1, argv: ["nono", "run", "--profile", "client", "--", "opencode", "--server", `http://127.0.0.1:${PORT}`], noNewPrivs: false, capFile: false },
    { pid: 101, ppid: 100, argv: ["/home/u/.opencode/bin/opencode", "--server", `http://127.0.0.1:${PORT}`] },
    { pid: 110, ppid: 1, argv: ["nono", "run", "--profile", "server", "--", "opencode", "serve"], noNewPrivs: false, capFile: false },
    { pid: 111, ppid: 110, argv: ["/home/u/.opencode/bin/opencode", "serve", "--hostname", "127.0.0.1", "--port", String(PORT)] },
    { pid: 112, ppid: 111, argv: ["/usr/bin/bash", "-c", "echo hi"] },
    { pid: 200, ppid: 1, argv: ["/home/u/.opencode/bin/opencode", "serve", "--service"], noNewPrivs: false, capFile: false },
    ...extra,
  ], options);
  return root;
}

const verifyTree = (root, overrides = {}) => verifySession({ supervisorPid: 100, serverSupervisorPid: 110, port: PORT, agent: OPENCODE, procRoot: root, ...overrides });

test("parentFromStat copes with parentheses and spaces in the command name", () => {
  assert.equal(parentFromStat("42 (my (odd) name) S 7 42 42 0"), 7);
});

test("readProcess and descendants walk the fake tree", () => {
  const root = procTree();
  const info = readProcess(111, { root });
  assert.deepEqual(info.argv, ["/home/u/.opencode/bin/opencode", "serve", "--hostname", "127.0.0.1", "--port", String(PORT)]);
  assert.equal(info.noNewPrivs, true);
  assert.equal(info.nonoCapFile, true);
  assert.equal(readProcess(999, { root }), null);
  assert.deepEqual(descendants(110, { root }).map((item) => item.pid), [111, 112]);
});

test("decodeTcpAddress reads IPv4 and IPv4-mapped IPv6 addresses", () => {
  assert.deepEqual(decodeTcpAddress("0100007F:1000"), { address: "127.0.0.1", port: 4096 });
  assert.deepEqual(decodeTcpAddress("0000000000000000FFFF00000100007F:1000"), { address: "127.0.0.1", port: 4096 });
  assert.deepEqual(decodeTcpAddress("00000000000000000000000001000000:0050"), { address: "::1", port: 80 });
});

test("a client and its private server, each in their own sandbox, with the tools under the server, verify", () => {
  const report = verifyTree(procTree());
  assert.equal(report.ok, true, report.problems.join("\n"));
  assert.deepEqual(report.processes.map((item) => [item.pid, item.sandbox, item.role]), [[101, "client", "client"], [111, "server", "server"], [112, "server", "tool"]]);
  assert.equal(report.server.pid, 111);
  assert.deepEqual(report.client.requiredArgs, ["--server", `http://127.0.0.1:${PORT}`]);
  assert.deepEqual(report.client.missingArgs, []);
  assert.match(summarizeVerification(report), /^confined: 3 processes, server pid 111$/);
});

test("a client pointed elsewhere, or a missing server sandbox, fails the verification", () => {
  const root = mkdtempSync(path.join(tmpdir(), "herdr-nono-proc-"));
  writeFakeProc(root, [
    { pid: 100, ppid: 1, argv: ["nono"], noNewPrivs: false },
    { pid: 101, ppid: 100, argv: ["/home/u/.opencode/bin/opencode", "--server", "http://127.0.0.1:4096"] },
    { pid: 200, ppid: 1, argv: ["/home/u/.opencode/bin/opencode", "serve", "--service"], noNewPrivs: false, capFile: false },
  ]);
  const report = verifySession({ supervisorPid: 100, serverSupervisorPid: null, port: PORT, agent: OPENCODE, procRoot: root });
  assert.equal(report.ok, false);
  assert.ok(report.problems.some((problem) => /started without http:\/\/127\.0\.0\.1:4242/.test(problem)), report.problems.join("\n"));
  assert.ok(report.problems.some((problem) => /server sandbox is not running/.test(problem)));
  assert.ok(report.problems.some((problem) => /agent's own server/.test(problem)), "the host service never counts as the server");
  assert.match(summarizeVerification(report), /^FAILED: /);
  const wrongPort = verifyTree(procTree(), { port: 4243 });
  assert.equal(wrongPort.ok, false, "a server on another port is not this launch's server");
});

test("an unconfined process in either sandbox fails the verification", () => {
  const report = verifyTree(procTree([{ pid: 113, ppid: 111, argv: ["/usr/bin/escape"], noNewPrivs: false }]));
  assert.equal(report.ok, false);
  assert.match(report.problems.join("\n"), /Process 113 \(\/usr\/bin\/escape\) under the server sandbox's supervisor is not confined/);
  const noMarker = verifyTree(procTree([{ pid: 105, ppid: 101, argv: ["x"], capFile: false }]));
  assert.equal(noMarker.ok, false, "a readable environment without NONO_CAP_FILE counts against the process");
  const unreadable = verifyTree(procTree([{ pid: 105, ppid: 101, argv: ["x"], environ: false }]));
  assert.equal(unreadable.ok, true, "an unreadable environment is judged by no_new_privs alone");
});

test("a sandboxed process connected to the host service port fails the verification", () => {
  const root = procTree([], { tcp: [tcpLine("0100007F:A000", "0100007F:1000", "01", "7777"), tcpLine("0100007F:1000", "00000000:0000", "0A", "8888")] });
  mkdirSync(path.join(root, "103", "fd"), { recursive: true });
  const connected = procTree([{ pid: 113, ppid: 111, argv: ["curl", "http://127.0.0.1:4096/api/info"], sockets: ["7777"] }], { tcp: [tcpLine("0100007F:A000", "0100007F:1000", "01", "7777")] });
  const service = { kind: "opencode", running: true, pid: 200, url: "http://127.0.0.1:4096", port: 4096, listening: true, sandboxed: false, stateFile: "/s", configFile: "/c" };
  const report = verifyTree(connected, { hostService: service });
  assert.equal(report.ok, false);
  assert.deepEqual(report.hostConnections, [{ pid: 113, remote: "127.0.0.1:4096" }]);
  assert.equal(report.warnings.length, 1, "a host service the tools can reach is also a warning");
  const unreachable = verifyTree(procTree(), { hostService: service, hostServiceReachable: false });
  assert.equal(unreachable.ok, true);
  assert.deepEqual(unreachable.warnings, [], "behind the proxy the running service is no warning");
  assert.equal(unreachable.hostService.reachable, false);
  assert.equal(tcpConnections({ root }).get("8888").state, "0A");
});

test("a session that is not running, or not Linux, is reported as such", () => {
  const root = procTree();
  assert.equal(verifyTree(root, { supervisorPid: 4242 }).ok, false);
  const none = verifyTree(path.join(root, "absent"));
  assert.equal(none.supported, false);
  assert.equal(none.ok, null);
  assert.match(summarizeVerification(none), /unsupported/);
  assert.equal(summarizeVerification(null), "not verified");
});

test("findClient matches the executable or the script an interpreter runs", () => {
  assert.equal(findClient([{ pid: 1, argv: ["/usr/bin/node", "/x/bin/opencode"] }], "opencode").pid, 1);
  assert.equal(findClient([{ pid: 1, argv: ["/usr/bin/node", "/x/other"] }], "opencode"), null);
});

test("detectOpencodeService reads the service file and checks the process and listener", () => {
  const home = mkdtempSync(path.join(tmpdir(), "herdr-nono-home-"));
  const env = { HOME: home };
  const proc = procTree([], { tcp: [tcpLine("0100007F:1000", "00000000:0000", "0A", "8888")] });
  assert.equal(detectOpencodeService({ env, procRoot: proc }).running, false, "no service file, no service");
  mkdirSync(path.join(home, ".local", "state", "opencode"), { recursive: true });
  writeFileSync(path.join(home, ".local", "state", "opencode", "service.json"), JSON.stringify({ url: "http://127.0.0.1:4096", pid: process.pid, password: "secret" }));
  // The fake /proc has no entry for this pid, so only liveness (the real process) and the listener are known.
  const service = detectOpencodeService({ env, procRoot: proc });
  assert.equal(service.running, true);
  assert.equal(service.port, 4096);
  assert.equal(service.listening, true);
  assert.equal(JSON.stringify(service).includes("secret"), false, "the password is never read into the result");
  assert.match(hostServiceWarning(service), /opencode service stop/);
  assert.equal(hostServiceWarning({ ...service, sandboxed: true }), null, "a service inside a sandbox is no way out");
  writeFileSync(path.join(home, ".local", "state", "opencode", "service.json"), JSON.stringify({ url: "http://127.0.0.1:4097", pid: process.pid }));
  assert.equal(detectOpencodeService({ env, procRoot: proc }).running, false, "a live pid without a listener on the port is a stale file");
});

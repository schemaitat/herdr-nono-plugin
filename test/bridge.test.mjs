import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import path from "node:path";
import { test } from "node:test";
import { parseBridgeArgs } from "../src/bridge-main.mjs";
import { sandboxEnv, shellLaunch } from "../src/lifecycle.mjs";
import { ROOT, createFixture, fakeBridgeProcess, mappingFor, runBridge } from "./helpers.mjs";

const CLIENT_PROFILE = path.join(ROOT, "profiles", "herdr-opencode-client.json");
const SERVER_PROFILE = path.join(ROOT, "profiles", "herdr-opencode-server.json");

function startedFixture(config = {}, overrides = {}) {
  return createFixture({ config: { verifyAfterStart: false, ...config }, panes: ({ worktree }) => ({ "pane-1": mappingFor({ worktree }, { lifecycleState: "provisional", launchCount: 0, ...overrides }) }) });
}

test("start runs the server and the client in two nono sandboxes joined by one port and one password", () => {
  const f = startedFixture({ agentEnv: ["FAKE_AGENT_MARK=hello"] });
  const started = Date.now();
  const { status, stdout } = runBridge(f, "start", "pane-1");
  assert.equal(status, 0, stdout);
  assert.ok(Date.now() - started < 4000, `the server is stopped promptly (${Date.now() - started} ms)`);
  const [server, client] = f.nonoRuns();
  const port = server.argv[server.argv.indexOf("--listen-port") + 1];
  assert.match(port, /^\d+$/);
  assert.deepEqual(server.argv, ["run", "--silent", "--profile", SERVER_PROFILE, "--name", "herdr-opencode-abc123def456-server", "--allow", f.worktree, "--listen-port", port, "--", "opencode", "serve", "--hostname", "127.0.0.1", "--port", port]);
  assert.deepEqual(client.argv, ["run", "--profile", CLIENT_PROFILE, "--name", "herdr-opencode-abc123def456", "--allow", f.worktree, "--open-port", port, "--", "opencode", "--server", `http://127.0.0.1:${port}`]);
  assert.equal(client.cwd, f.worktree);
  for (const run of [server, client]) {
    assert.deepEqual(Object.keys(run.env).filter((key) => key.startsWith("HERDR_")), [], "no HERDR_* variable reaches nono");
  }
  const agents = f.agentRuns();
  const serverRun = agents.find((item) => item.mode === "server");
  const clientRun = agents.find((item) => item.mode === "client");
  assert.equal(serverRun.hasPassword, true);
  assert.equal(clientRun.serverStatus, 200, "the client authenticated against its server with the shared password");
  assert.deepEqual(clientRun.herdr, []);
  assert.equal(clientRun.mark, "hello", "agentEnv reaches the agent");
  assert.ok(agents.some((item) => item.mode === "server-stopped"), "the server is stopped when the client exits");
  const entry = f.mappings().panes["pane-1"];
  assert.equal(entry.lifecycleState, "exited");
  assert.equal(entry.lastExitCode, 0);
  assert.equal(entry.launchCount, 1);
  assert.equal(entry.port, Number(port));
  assert.equal(entry.serverProfile, SERVER_PROFILE);
  assert.equal(entry.bridgePid, null, "the bridge gives the mapping back on exit");
  assert.equal(entry.supervisorPid, null);
  assert.equal(entry.serverSupervisorPid, null);
  assert.ok(entry.serverLog.endsWith("herdr-opencode-abc123def456-server.log"));
  assert.match(readFileSync(entry.serverLog, "utf8"), /fake server listening/);
  assert.match(stdout, /Starting OpenCode's server in nono sandbox herdr-opencode-abc123def456-server/);
  assert.match(stdout, /Launching OpenCode in nono sandbox herdr-opencode-abc123def456/);
  assert.match(stdout, /OpenCode exited with code 0\. Use reconnect/);
  f.cleanup();
});

test("a server that does not come up stops the launch with its log", () => {
  const f = startedFixture();
  const { status, stdout } = runBridge(f, "start", "pane-1", { env: { FAKE_SERVER_FAIL: "1" } });
  assert.equal(status, 1);
  assert.match(stdout, /error: OpenCode's server exited before it answered\. Its log is .*-server\.log/);
  assert.match(stdout, /fake server: cannot start/);
  assert.equal(f.nonoRuns().length, 1, "no client is started");
  assert.equal(f.mappings().panes["pane-1"].lifecycleState, "failed");
  assert.equal(f.mappings().panes["pane-1"].lastError.kind, "startup");
  f.cleanup();
});

test("connect resumes with the adapter's resume arguments and extra grants and flags are passed on", () => {
  const f = startedFixture({ allowPaths: ["/data"], readPaths: ["/ref"], nonoArgs: ["--memory", "2G"], silent: true });
  assert.equal(runBridge(f, "connect", "pane-1").status, 0);
  const [server, client] = f.nonoRuns();
  const port = server.argv[server.argv.indexOf("--listen-port") + 1];
  assert.deepEqual(client.argv.slice(client.argv.indexOf("--") + 1), ["opencode", "--server", `http://127.0.0.1:${port}`, "--continue"]);
  assert.ok(client.argv.includes("--silent"));
  assert.deepEqual(client.argv.slice(client.argv.indexOf("--allow", 7), client.argv.indexOf("--")), ["--allow", "/data", "--read", "/ref", "--memory", "2G", "--open-port", port]);
  assert.deepEqual(server.argv.slice(server.argv.indexOf("--allow", 7), server.argv.indexOf("--")), ["--allow", "/data", "--read", "/ref", "--memory", "2G", "--listen-port", port]);
  f.cleanup();
});

test("the agent's exit code is recorded and shown", () => {
  const f = startedFixture();
  const { status, stdout } = runBridge(f, "start", "pane-1", { env: { FAKE_AGENT_EXIT: "3" } });
  assert.equal(status, 3);
  assert.equal(f.mappings().panes["pane-1"].lastExitCode, 3);
  assert.match(stdout, /exited with code 3/);
  f.cleanup();
});

test("a failed verification stops the session and marks the mapping failed", () => {
  // The fake nono does not confine anything, so the verification must fail.
  const f = startedFixture({ verifyAfterStart: true });
  const started = Date.now();
  const { status, stdout } = runBridge(f, "start", "pane-1", { env: { FAKE_AGENT_SLEEP_MS: "15000", FAKE_AGENT_SERVER: "1" } });
  assert.ok(Date.now() - started < 12_000, "the agent was stopped long before it would have exited");
  assert.notEqual(status, 0);
  assert.match(stdout, /The sandbox verification failed, so OpenCode was stopped: Process \d+ .* is not confined/);
  const entry = f.mappings().panes["pane-1"];
  assert.equal(entry.lifecycleState, "failed");
  assert.equal(entry.lastError.kind, "unconfined");
  assert.equal(entry.verification.ok, false);
  assert.ok(entry.verification.server.pid, "the fake server was found in the tree");
  assert.ok(f.herdrCalls().some((call) => call[0] === "notification" && /FAILED/.test(call[2]) && /stopped/.test(call[4])));
  f.cleanup();
});

test("with onVerificationFailure warn the session keeps running and the failed report is kept", () => {
  const f = startedFixture({ verifyAfterStart: true, onVerificationFailure: "warn" });
  const { status } = runBridge(f, "start", "pane-1", { env: { FAKE_AGENT_SLEEP_MS: "2500" } });
  assert.equal(status, 0);
  const entry = f.mappings().panes["pane-1"];
  assert.equal(entry.lifecycleState, "exited");
  assert.equal(entry.verification.ok, false);
  f.cleanup();
});

test("a missing agent binary fails before nono runs", () => {
  const f = startedFixture();
  const { status, stdout } = runBridge(f, "start", "pane-1", { env: { PATH: `${path.dirname(process.execPath)}:/usr/bin:/bin` } });
  assert.equal(status, 1);
  assert.match(stdout, /error: The agent command "opencode" was not found on PATH/);
  assert.equal(f.nonoRuns().length, 0);
  assert.equal(f.mappings().panes["pane-1"].lifecycleState, "failed");
  assert.equal(f.mappings().panes["pane-1"].lastError.kind, "config");
  f.cleanup();
});

async function withHostService(f, fn) {
  const server = createServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = /** @type {import("node:net").AddressInfo} */ (server.address());
  const service = spawn(process.execPath, ["-e", "setTimeout(() => {}, 60000)", "opencode", "serve", "--service"], { stdio: "ignore" });
  try {
    mkdirSync(path.join(f.home, ".local", "state", "opencode"), { recursive: true });
    writeFileSync(path.join(f.home, ".local", "state", "opencode", "service.json"), JSON.stringify({ url: `http://127.0.0.1:${port}`, pid: service.pid, password: "not-read" }));
    return await fn();
  } finally {
    service.kill("SIGKILL");
    server.close();
  }
}

test("a running host service is no reason to refuse when the tools' sandbox cannot reach localhost", async () => {
  const f = startedFixture();
  await withHostService(f, () => {
    const { status, stdout } = runBridge(f, "start", "pane-1");
    assert.equal(status, 0, stdout);
    assert.doesNotMatch(stdout, /background service|NOT started/);
    assert.equal(f.nonoRuns().length, 2);
  });
  f.cleanup();
});

test("with a server profile that leaves localhost open the agent is not started while the host service runs", async () => {
  const f = startedFixture({ serverProfile: "/etc/open-egress.json" });
  await withHostService(f, () => {
    const { status, stdout } = runBridge(f, "start", "pane-1");
    assert.equal(status, 1);
    assert.match(stdout, /nono: the agent was NOT started/);
    assert.match(stdout, /An OpenCode background service runs OUTSIDE any sandbox:\n {2}pid \d+, http:\/\/127\.0\.0\.1:\d+/);
    assert.match(stdout, /~\/\.local\/state\/opencode\/service\.json/, "the path is shown relative to home");
    assert.match(stdout, /\n {2}opencode service stop\n/);
    assert.match(stdout, /error: Not started: an unsandboxed OpenCode service runs \(pid \d+\); hostServiceCheck is "refuse"\./);
    assert.equal(f.nonoRuns().length, 0);
    const entry = f.mappings().panes["pane-1"];
    assert.equal(entry.lifecycleState, "failed");
    assert.equal(entry.lastError.kind, "unconfined");
    assert.ok(f.herdrCalls().some((call) => call[0] === "notification" && /NOT started/.test(call[2]) && /opencode service stop/.test(call[4])));
  });
  f.cleanup();
});

test("hostServiceCheck warn starts an agent whose tools can reach the host service, with a warning", async () => {
  const f = startedFixture({ serverProfile: "/etc/open-egress.json", hostServiceCheck: "warn" });
  await withHostService(f, () => {
    const { status, stdout } = runBridge(f, "start", "pane-1");
    assert.equal(status, 0);
    assert.match(stdout, /warning: An OpenCode background service/);
    assert.doesNotMatch(stdout, /NOT started/);
    assert.equal(f.nonoRuns().length, 2);
    assert.ok(f.herdrCalls().some((call) => call[0] === "notification" && /host service/.test(call[2])));
  });
  f.cleanup();
});

test("hostServiceCheck off does not look for the service", async () => {
  const f = startedFixture({ hostServiceCheck: "off", serverProfile: "/etc/open-egress.json" });
  await withHostService(f, () => {
    const { stdout } = runBridge(f, "start", "pane-1");
    assert.doesNotMatch(stdout, /background service/);
  });
  f.cleanup();
});

test("shell opens the configured shell without start-up files in a sandbox named after the agent", () => {
  const f = startedFixture({ shell: "/bin/true" });
  const { status, stdout } = runBridge(f, "shell", "pane-1");
  assert.equal(status, 0, stdout);
  const [run] = f.nonoRuns();
  assert.equal(run.argv[run.argv.indexOf("--name") + 1], "herdr-opencode-abc123def456-shell");
  assert.deepEqual(run.argv.slice(run.argv.indexOf("--") + 1), ["/bin/true"]);
  assert.equal(run.env.PS1, "[nono:abc123] $ ");
  assert.deepEqual(f.mappings().panes["pane-1"].shellPids, [], "the shell record is released on exit");
  assert.equal(f.mappings().panes["pane-1"].lifecycleState, "provisional", "a shell does not touch the agent's state");
  f.cleanup();
});

test("shellLaunch knows bash, zsh and fish", () => {
  assert.deepEqual(shellLaunch("bash", "herdr-opencode-abc123def456"), { argv: ["bash", "--noprofile", "--norc"], env: { PS1: "[nono:abc123] \\w \\$ " } });
  assert.deepEqual(shellLaunch("/usr/bin/zsh", "s-1").argv, ["/usr/bin/zsh", "--no-rcs"]);
  assert.deepEqual(shellLaunch("fish", "s-1"), { argv: ["fish", "--no-config"], env: {} });
});

test("a second bridge refuses while the first still runs", () => {
  const busy = fakeBridgeProcess("pane-1");
  const f = startedFixture({}, { bridgePid: busy.pid });
  const { status, stdout } = runBridge(f, "start", "pane-1");
  busy.stop();
  assert.equal(status, 1);
  assert.match(stdout, /already runs a bridge/);
  assert.equal(f.nonoRuns().length, 0);
  f.cleanup();
});

test("agents Herdr cannot detect are reported for the time they run", () => {
  const f = createFixture({
    config: { verifyAfterStart: false, agentKind: "tool", customAgents: { tool: { title: "Tool", command: ["opencode"], profile: "opencode" } } },
    panes: ({ worktree }) => ({ "pane-1": mappingFor({ worktree }, { agentKind: "tool", launchCount: 0 }) }),
  });
  assert.equal(runBridge(f, "start", "pane-1").status, 0);
  const calls = f.herdrCalls();
  const report = calls.findIndex((call) => call[1] === "report-agent");
  const release = calls.findIndex((call) => call[1] === "release-agent");
  assert.ok(report !== -1 && release > report, JSON.stringify(calls));
  assert.deepEqual(calls[report].slice(2, 9), ["pane-1", "--source", "nono.sandbox", "--agent", "tool", "--state", "unknown"]);
  assert.deepEqual(f.nonoRuns()[0].argv.slice(0, 3), ["run", "--profile", "opencode"]);
  f.cleanup();
});

test("sandboxEnv strips Herdr's and the host agents' socket variables and adds agentEnv", () => {
  const env = sandboxEnv({ PATH: "/bin", HERDR_SOCKET_PATH: "/s", HERDR_PANE_ID: "p", SSH_AUTH_SOCK: "/a", DBUS_SESSION_BUS_ADDRESS: "unix:x", TMUX: "t", KEEP: "1" }, ["EXTRA=a=b"]);
  assert.deepEqual(env, { PATH: "/bin", KEEP: "1", EXTRA: "a=b" });
});

test("parseBridgeArgs requires the plugin root and rejects unknown options", () => {
  assert.equal(parseBridgeArgs(["start", "--state-dir", "s", "--config-dir", "c", "--pane-id", "p", "--plugin-root", "r"]).pluginRoot, "r");
  assert.throws(() => parseBridgeArgs(["start", "--state-dir", "s", "--config-dir", "c", "--pane-id", "p"]), /pluginRoot/);
  assert.throws(() => parseBridgeArgs(["start", "--sbx-bin", "x"]), /Unexpected bridge argument/);
  assert.throws(() => parseBridgeArgs(["attach"]), /Unknown bridge mode/);
});

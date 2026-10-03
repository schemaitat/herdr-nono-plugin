import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import path from "node:path";
import { test } from "node:test";
import { bridgeStartTimeout, entryCwd, nonoVersionWarning, parseKeybindingReport } from "../src/action-main.mjs";
import { summarizeProfile } from "../src/nono.mjs";
import { FAKE_NONO, ROOT, createFixture, fakeBridgeProcess, fakeShellProcess, mappingFor, runAction } from "./helpers.mjs";

const NAME = "herdr-opencode-abc123def456";
const PROFILE = path.join(ROOT, "profiles", "herdr-opencode-client.json");
const SERVER_PROFILE = path.join(ROOT, "profiles", "herdr-opencode-server.json");

function mapped(overrides = {}, config = {}) {
  return createFixture({ config, panes: ({ worktree }) => ({ "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1", ...overrides }) }) });
}

test("doctor reports nono, the resolved profile and a clean escape probe with the marker first", () => {
  const f = createFixture();
  const { status, result, stdout } = runAction(f, "doctor");
  assert.equal(status, 0, stdout);
  assert.ok(stdout.startsWith("HERDR_SANDBOX_RESULT: "));
  assert.equal(result.ok, true);
  assert.equal(result.nonoVersion, "0.78.0");
  assert.equal(result.agentKind, "opencode");
  assert.deepEqual(result.launchArgv, ["opencode", "--server", "http://127.0.0.1:{port}"]);
  assert.deepEqual(result.serverArgv, ["opencode", "serve", "--hostname", "127.0.0.1", "--port", "{port}"]);
  assert.equal(result.agentBinary, path.join(ROOT, "test", "fakes", "bin", "opencode"));
  assert.equal(result.profile.ref, PROFILE);
  assert.equal(result.profile.egress, "blocked");
  assert.equal(result.serverProfile.ref, SERVER_PROFILE);
  assert.equal(result.serverProfile.egress, "allowlist");
  assert.deepEqual(result.serverProfile.allowDomains, ["*"]);
  assert.equal(result.serverProfile.afUnixMediation, "pathname");
  assert.equal(result.hostService.running, false);
  assert.ok(result.probes.every((check) => check.ok));
  assert.deepEqual(result.warnings, []);
  assert.match(stdout, /probe ok {3}herdrSocket: denied/);
  assert.match(stdout, /probe ok {3}opencodeServicePort: denied/);
  const run = f.nonoRuns()[0];
  assert.deepEqual(run.argv.slice(0, 6), ["run", "--silent", "--profile", SERVER_PROFILE, "--name", "herdr-nono-probe"], "the probe runs under the profile the tools run under");
  assert.ok(run.env.HERDR_SOCKET_PATH, "the probe keeps HERDR_* so it shows whether the profile strips them");
  f.cleanup();
});

test("doctor fails with unconfined when the probe reaches a critical socket", () => {
  const f = createFixture();
  const probe = { herdrSocket: "allowed", sessionBus: "denied", env: ["HERDR_SOCKET_PATH"], marker: true };
  const { status, result, stderr } = runAction(f, "doctor", { env: { FAKE_NONO_PROBE: JSON.stringify(probe) } });
  assert.equal(status, 1);
  assert.equal(result.ok, false);
  assert.equal(result.errorKind, "unconfined");
  assert.match(result.message, /herdrSocket allowed/);
  assert.ok(Array.isArray(result.probes), "the failure keeps the probe results");
  assert.match(stderr, /probe FAIL herdrSocket/);
  f.cleanup();
});

test("doctor warns about profiles without AF_UNIX mediation, a client with network, an old nono and a missing agent", () => {
  const f = createFixture();
  const { result } = runAction(f, "doctor", { env: { FAKE_NONO_PROFILE_JSON: JSON.stringify({ name: "opencode", extends: ["nolabs-ai/opencode"], network: { allow_domain: ["opencode.ai"] } }), FAKE_NONO_VERSION: "0.70.1", PATH: `${path.dirname(process.execPath)}:/usr/bin:/bin` } });
  assert.equal(result.ok, true);
  assert.equal(result.profile.egress, "allowlist");
  assert.deepEqual(result.profile.allowDomains, ["opencode.ai"]);
  assert.equal(result.profile.afUnixMediation, "off");
  assert.equal(result.agentBinary, null);
  assert.equal(result.warnings.length, 5, JSON.stringify(result.warnings));
  assert.match(result.warnings.join("\n"), /does not block the network/);
  assert.match(result.versionWarning, /0\.70\.1 is older than 0\.78\.0/);
  f.cleanup();
});

test("doctor explains a profile nono cannot resolve and a missing nono", () => {
  const f = createFixture();
  const missing = runAction(f, "doctor", { env: { FAKE_NONO_FAIL: "profile:not-found" } });
  assert.equal(missing.result.errorKind, "config");
  assert.match(missing.result.message, /nono cannot resolve the profile/);
  const f2 = createFixture({ config: { nonoBin: "/nope/nono" } });
  assert.equal(runAction(f2, "doctor").result.errorKind, "startup");
  f.cleanup();
  f2.cleanup();
});

test("doctor reports a host service the tools cannot reach, and fails when a server profile leaves it reachable", async () => {
  const f = createFixture();
  const server = createServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const service = spawn(process.execPath, ["-e", "setTimeout(() => {}, 60000)", "opencode", "serve", "--service"], { stdio: "ignore" });
  try {
    mkdirSync(path.join(f.home, ".local", "state", "opencode"), { recursive: true });
    writeFileSync(path.join(f.home, ".local", "state", "opencode", "service.json"), JSON.stringify({ url: `http://127.0.0.1:${/** @type {any} */ (server.address()).port}`, pid: service.pid }));
    const locked = runAction(f, "doctor");
    assert.equal(locked.result.ok, true, JSON.stringify(locked.result));
    assert.equal(locked.result.hostService.running, true);
    assert.equal(locked.result.hostService.reachable, false);
    assert.deepEqual(locked.result.warnings, []);
    assert.match(locked.stdout, /OpenCode host service: pid \d+, .*not reachable from the agent's tools/);
    writeFileSync(path.join(f.configDir, "config.json"), JSON.stringify({ serverProfile: "/etc/open-egress.json" }));
    const open = runAction(f, "doctor", { env: { FAKE_NONO_PROBE: JSON.stringify({ herdrSocket: "denied", opencodeServicePort: "refused", marker: true, env: [] }) } });
    assert.equal(open.status, 1);
    assert.equal(open.result.errorKind, "unconfined");
    assert.match(open.result.message, /start-agent and reconnect will refuse to run/);
    assert.equal(open.result.hostService.reachable, true, "the failure keeps the payload");
    assert.match(open.stderr, /REACHABLE from the agent's tools/);
    writeFileSync(path.join(f.configDir, "config.json"), JSON.stringify({ serverProfile: "/etc/open-egress.json", hostServiceCheck: "warn" }));
    const warned = runAction(f, "doctor", { env: { FAKE_NONO_PROBE: JSON.stringify({ herdrSocket: "denied", opencodeServicePort: "refused", marker: true, env: [] }) } });
    assert.equal(warned.result.errorKind, "unconfined", "the port probe still fails: localhost is open");
    assert.match(warned.result.message, /opencodeServicePort refused/);
  } finally {
    service.kill("SIGKILL");
    server.close();
    f.cleanup();
  }
});

test("start-agent splits a pane, records the mapping and types the bridge command", () => {
  const f = createFixture();
  mkdirSync(path.join(f.worktree, "src"));
  const { status, result, stdout } = runAction(f, "start-agent", { context: { focused_pane_id: "w1:p1", workspace_id: "w1", focused_pane_cwd: path.join(f.worktree, "src") } });
  assert.equal(status, 0, stdout);
  assert.equal(result.paneId, "pane-new-1");
  assert.equal(result.localPath, f.worktree, "the repository root is granted");
  assert.equal(result.workdir, path.join(f.worktree, "src"), "the agent starts where the pane was");
  assert.match(result.sessionName, /^herdr-opencode-[a-f0-9]{12}$/);
  assert.equal(result.profile, PROFILE);
  assert.equal(result.serverProfile, SERVER_PROFILE);
  const calls = f.herdrCalls();
  assert.deepEqual(calls[0], ["pane", "split", "w1:p1", "--direction", "right", "--ratio", "0.5", "--cwd", path.join(f.worktree, "src"), "--focus"]);
  assert.deepEqual(calls[1], ["pane", "rename", "pane-new-1", `nono opencode ${result.sessionName.split("-").pop().slice(0, 6)}`]);
  const typed = calls.find((call) => call[1] === "run");
  assert.equal(typed[2], "pane-new-1");
  assert.ok(typed[3].startsWith(`env HERDR_AGENT=opencode ${process.execPath} ${path.join(ROOT, "src", "bridge.mjs")} start --state-dir ${f.stateDir}`), typed[3]);
  assert.ok(typed[3].includes(`--plugin-root ${ROOT}`));
  assert.ok(typed[3].includes(`--nono-bin ${FAKE_NONO}`));
  assert.ok(calls.some((call) => call[0] === "notification"));
  const entry = f.mappings().panes["pane-new-1"];
  assert.equal(entry.lifecycleState, "provisional");
  assert.equal(entry.workspaceId, "w1");
  assert.equal(entry.launchCount, 0);
  f.cleanup();
});

test("start-agent opens a tab when openIn is tab", () => {
  const f = createFixture({ config: { openIn: "tab" } });
  const { result } = runAction(f, "start-agent", { context: { focused_pane_id: "w1:p1", workspace_id: "w1", workspace_cwd: f.worktree } });
  assert.equal(result.ok, true);
  assert.deepEqual(f.herdrCalls()[0].slice(0, 6), ["tab", "create", "--workspace", "w1", "--cwd", f.worktree]);
  f.cleanup();
});

test("start-agent refuses the home directory and a context without a directory", () => {
  const f = createFixture();
  const home = runAction(f, "start-agent", { context: { focused_pane_id: "w1:p1", workspace_cwd: f.home } });
  assert.equal(home.result.errorKind, "target");
  assert.match(home.result.message, /Refusing to grant/);
  assert.equal(runAction(f, "start-agent", { context: {} }).result.errorKind, "target");
  assert.equal(f.herdrCalls().length, 0, "nothing was opened");
  f.cleanup();
});

test("start-agent fails before touching Herdr when nono is missing", () => {
  const f = createFixture({ config: { nonoBin: "/nope/nono" } });
  const { result } = runAction(f, "start-agent", { context: { focused_pane_id: "w1:p1", workspace_cwd: f.worktree } });
  assert.equal(result.errorKind, "startup");
  assert.equal(f.herdrCalls().length, 0);
  f.cleanup();
});

test("start-agent refuses a reused pane id whose agent still runs", () => {
  const busy = fakeBridgeProcess("pane-new-1");
  const f = createFixture({ panes: ({ worktree }) => ({ "pane-new-1": mappingFor({ worktree }, { paneId: "pane-new-1", bridgePid: busy.pid }) }) });
  const { result } = runAction(f, "start-agent", { context: { focused_pane_id: "w1:p1", workspace_cwd: f.worktree } });
  busy.stop();
  assert.equal(result.errorKind, "conflict");
  f.cleanup();
});

test("reconnect resumes the agent in its pane", () => {
  const f = mapped();
  const { result } = runAction(f, "reconnect", { context: { focused_pane_id: "w1:p1", workspace_id: "w1" } });
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.equal(result.mode, "connect");
  assert.deepEqual(result.argv, ["opencode", "--server", "http://127.0.0.1:{port}", "--continue"]);
  assert.equal(result.movedTo, null);
  const typed = f.herdrCalls().find((call) => call[1] === "run");
  assert.equal(typed[2], "w1:p1");
  assert.match(typed[3], /bridge\.mjs connect /);
  f.cleanup();
});

test("reconnect starts afresh when the agent never ran", () => {
  const f = mapped({ launchCount: 0, lifecycleState: "failed" });
  const { result } = runAction(f, "reconnect", { context: { focused_pane_id: "w1:p1" } });
  assert.equal(result.mode, "start");
  assert.deepEqual(result.argv, ["opencode", "--server", "http://127.0.0.1:{port}"]);
  f.cleanup();
});

test("reconnect refuses while the agent runs, per bridge process or per Herdr", () => {
  const busy = fakeBridgeProcess("w1:p1");
  const f = mapped({ bridgePid: busy.pid, bridgeStartedAt: "now" });
  const running = runAction(f, "reconnect", { context: { focused_pane_id: "w1:p1" } });
  busy.stop();
  assert.equal(running.result.errorKind, "conflict");
  assert.match(running.result.message, /still runs herdr-opencode-abc123def456/);
  const f2 = mapped();
  const herdrSees = runAction(f2, "reconnect", { context: { focused_pane_id: "w1:p1", focused_pane_agent: "opencode" } });
  assert.equal(herdrSees.result.errorKind, "conflict");
  assert.match(herdrSees.result.message, /herdr pane release-agent w1:p1 --source nono\.sandbox --agent opencode/);
  f.cleanup();
  f2.cleanup();
});

test("reconnect from the pane next to the agent uses the workspace's only mapping", () => {
  const f = mapped();
  const { result, stderr } = runAction(f, "reconnect", { context: { focused_pane_id: "w1:p9", workspace_id: "w1" } });
  assert.equal(result.ok, true);
  assert.equal(result.paneId, "w1:p1");
  assert.match(stderr, /using the workspace's only mapping/);
  f.cleanup();
});

test("reconnect gives a mapping whose pane is gone a new pane", () => {
  const f = mapped();
  const { result } = runAction(f, "reconnect", { context: { focused_pane_id: "w2:p1", workspace_id: "w2" }, env: { FAKE_HERDR_MISSING_PANES: "w1:p1" } });
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.equal(result.adoptedFrom, "w1:p1");
  assert.equal(result.paneId, "pane-new-1");
  const panes = f.mappings().panes;
  assert.ok(!panes["w1:p1"], "the old mapping moved");
  assert.equal(panes["pane-new-1"].sessionName, NAME);
  assert.equal(panes["pane-new-1"].workspaceId, "w2");
  f.cleanup();
});

test("reconnect moves to a fresh pane when the typed command is swallowed", () => {
  const f = mapped();
  const { result } = runAction(f, "reconnect", { context: { focused_pane_id: "w1:p1" }, env: { FAKE_HERDR_BRIDGE_STARTS: "0" } });
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.equal(result.movedTo, "pane-new-1");
  const calls = f.herdrCalls();
  assert.ok(calls.some((call) => call[1] === "rename" && call[2] === "w1:p1" && /moved to pane-new-1/.test(call[3])));
  const runs = calls.filter((call) => call[1] === "run");
  assert.equal(runs.at(-1)[2], "pane-new-1");
  assert.match(runs.at(-1)[3], /bridge\.mjs connect --state-dir .* --pane-id pane-new-1/);
  f.cleanup();
});

test("open-shell splits below the focused pane and runs the shell bridge for the mapping", () => {
  const f = mapped();
  const { result } = runAction(f, "open-shell", { context: { focused_pane_id: "w1:p1" } });
  assert.equal(result.ok, true);
  assert.equal(result.mappedPaneId, "w1:p1");
  const calls = f.herdrCalls();
  assert.deepEqual(calls[0].slice(0, 5), ["pane", "split", "w1:p1", "--direction", "down"]);
  const typed = calls.find((call) => call[1] === "run");
  assert.match(typed[3], /bridge\.mjs shell --state-dir .* --pane-id w1:p1 /);
  assert.doesNotMatch(typed[3], /HERDR_AGENT/);
  f.cleanup();
});

test("stop ends the mapped nono session and waits for the bridge", () => {
  const f = createFixture({
    sessions: [{ session_id: "0badc0ffee", name: NAME, supervisor_pid: 999999, status: "running", command: ["opencode", "--server", "http://127.0.0.1:4242"] }, { session_id: "other", name: "someone-else", supervisor_pid: 999998, status: "running" }],
    panes: ({ worktree }) => ({ "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1", lifecycleState: "running" }) }),
  });
  const { result } = runAction(f, "stop", { context: { focused_pane_id: "w1:p1" } });
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.equal(result.sessionId, "0badc0ffee");
  assert.deepEqual(f.nonoCalls().find((call) => call.argv[0] === "stop").argv, ["stop", "0badc0ffee"]);
  assert.equal(f.sessions().find((session) => session.session_id === "other").status, "running", "other sessions are left alone");
  const nothing = runAction(f, "stop", { context: { focused_pane_id: "w1:p1" } });
  assert.equal(nothing.result.errorKind, "not-found");
  f.cleanup();
});

test("info prints the mapping, the resolved agent and the live sessions", () => {
  const f = createFixture({
    sessions: [{ session_id: "s1", name: NAME, supervisor_pid: 999999, status: "running" }, { session_id: "s2", name: `${NAME}-shell`, supervisor_pid: 999997, status: "running" }],
    panes: ({ worktree }) => ({ "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1" }) }),
  });
  const { result, stdout } = runAction(f, "info", { context: { focused_pane_id: "w1:p1" } });
  assert.equal(result.ok, true);
  assert.equal(result.mapping.sessionName, NAME);
  assert.equal(result.agent.profile, PROFILE);
  assert.deepEqual(result.agent.resumeArgv, ["opencode", "--server", "http://127.0.0.1:{port}", "--continue"]);
  assert.equal(result.agent.serverProfile, SERVER_PROFILE);
  assert.equal(result.sessions.agent.sessionId, "s1");
  assert.equal(result.sessions.shells.length, 1);
  assert.match(stdout, /"sessionName": "herdr-opencode-abc123def456"/);
  f.cleanup();
});

test("verify-sandbox needs a running session and fails with the report when it is not confined", () => {
  const f = mapped();
  const idle = runAction(f, "verify-sandbox", { context: { focused_pane_id: "w1:p1" } });
  assert.equal(idle.result.errorKind, "not-found");
  // A live but unconfined process tree: a fake supervisor whose child is not sandboxed.
  const supervisor = spawn(process.execPath, ["-e", `require("node:child_process").spawn(process.execPath, ["-e", "setTimeout(() => {}, 20000)", "opencode", "--server", "http://127.0.0.1:4242"], { stdio: "ignore" }); setTimeout(() => {}, 20000)`], { stdio: "ignore" });
  try {
    const f2 = mapped({ supervisorPid: supervisor.pid, lifecycleState: "running" });
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 400);
    const { status, result, stderr } = runAction(f2, "verify-sandbox", { context: { focused_pane_id: "w1:p1" } });
    assert.equal(status, 1);
    assert.equal(result.errorKind, "unconfined");
    assert.equal(result.verified, false);
    assert.equal(result.report.processes.length, 1);
    assert.equal(result.report.processes[0].confined, false);
    assert.match(stderr, /UNCONFINED/);
    assert.equal(f2.mappings().panes["w1:p1"].verification.ok, false, "the report is recorded");
    f2.cleanup();
  } finally {
    supervisor.kill("SIGKILL");
  }
  f.cleanup();
});

test("list-sandboxes shows every mapping with its session and pane state", () => {
  const f = createFixture({
    sessions: [{ session_id: "s1", name: NAME, supervisor_pid: 999999, status: "running" }],
    panes: ({ worktree }) => ({
      "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1", lifecycleState: "running", verification: { ok: true, supported: true, processes: [{}, {}], server: { pid: 5 }, problems: [] } }),
      "w1:p2": mappingFor({ worktree }, { paneId: "w1:p2", sessionName: "herdr-opencode-000000000002" }),
    }),
  });
  const { result, stdout } = runAction(f, "list-sandboxes", { env: { FAKE_HERDR_MISSING_PANES: "w1:p2" } });
  const byPane = Object.fromEntries(result.mappings.map((item) => [item.paneId, item]));
  assert.equal(byPane["w1:p1"].running, true);
  assert.equal(byPane["w1:p1"].verified, true);
  assert.equal(byPane["w1:p2"].running, false);
  assert.equal(byPane["w1:p2"].paneExists, false);
  assert.match(stdout, /w1:p2 \(pane gone\)/);
  assert.match(stdout, /confined: 2 processes, server pid 5/);
  f.cleanup();
});

test("prune-mappings drops mappings whose pane is gone unless their agent or shell still runs", () => {
  const shell = fakeShellProcess("w1:p3");
  const f = createFixture({
    panes: ({ worktree }) => ({
      "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1" }),
      "w1:p2": mappingFor({ worktree }, { paneId: "w1:p2", sessionName: "herdr-opencode-000000000002" }),
      "w1:p3": mappingFor({ worktree }, { paneId: "w1:p3", sessionName: "herdr-opencode-000000000003", shellPids: [{ pid: shell.pid, paneId: "w1:p3" }] }),
    }),
  });
  const { result } = runAction(f, "prune-mappings", { env: { FAKE_HERDR_MISSING_PANES: "w1:p2,w1:p3" } });
  shell.stop();
  assert.deepEqual(result.pruned.map((item) => item.paneId), ["w1:p2"]);
  assert.deepEqual(result.kept.map((item) => item.paneId), ["w1:p3"]);
  assert.deepEqual(Object.keys(f.mappings().panes).sort(), ["w1:p1", "w1:p3"]);
  f.cleanup();
});

test("forget-mapping drops an idle mapping and refuses while a shell runs", () => {
  const shell = fakeShellProcess("w1:p1");
  const f = mapped({ shellPids: [{ pid: shell.pid, paneId: "w1:p1" }] });
  const refused = runAction(f, "forget-mapping", { context: { focused_pane_id: "w1:p1" } });
  shell.stop();
  assert.equal(refused.result.errorKind, "conflict");
  assert.match(refused.result.message, /open-shell session/);
  const { result } = runAction(f, "forget-mapping", { context: { focused_pane_id: "w1:p1" } });
  assert.equal(result.ok, true);
  assert.equal(result.removed, true);
  assert.deepEqual(Object.keys(f.mappings().panes), []);
  f.cleanup();
});

test("sandboxes opens the overlay pane", () => {
  const f = createFixture();
  const { result } = runAction(f, "sandboxes");
  assert.equal(result.entrypoint, "sandboxes");
  assert.deepEqual(f.herdrCalls()[0], ["plugin", "pane", "open", "--plugin", "nono.sandbox", "--entrypoint", "sandboxes", "--focus"]);
  f.cleanup();
});

test("install-keybindings adds the four bindings once and reloads Herdr", () => {
  const f = createFixture();
  const configPath = path.join(f.root, "herdr", "config.toml");
  const first = runAction(f, "install-keybindings", { env: { HERDR_CONFIG_PATH: configPath } });
  assert.equal(first.result.ok, true, JSON.stringify(first.result));
  assert.deepEqual(first.result.added.map((item) => item.action), ["start-agent", "reconnect", "open-shell", "sandboxes"]);
  assert.equal(first.result.reloaded, true);
  const text = readFileSync(configPath, "utf8");
  assert.match(text, /key = "prefix\+shift\+a"\ntype = "plugin_action"\ncommand = "nono\.sandbox\.start-agent"/);
  const second = runAction(f, "install-keybindings", { env: { HERDR_CONFIG_PATH: configPath } });
  assert.deepEqual(second.result.added, []);
  assert.equal(second.result.existing.length, 4);
  assert.equal(readFileSync(configPath, "utf8"), text, "nothing is appended twice");
  f.cleanup();
});

test("install-keybindings restores the config when Herdr rejects it and leaves taken chords alone", () => {
  const f = createFixture();
  const configPath = path.join(f.root, "config.toml");
  writeFileSync(configPath, '[[keys.command]]\nkey = "prefix+shift+o"\ntype = "shell"\ncommand = "htop"\n');
  const before = readFileSync(configPath, "utf8");
  const rejected = runAction(f, "install-keybindings", { env: { HERDR_CONFIG_PATH: configPath, FAKE_HERDR_CONFIG_CHECK: "fail" } });
  assert.equal(rejected.result.errorKind, "config");
  assert.match(rejected.result.message, /restored to its previous content/);
  assert.equal(readFileSync(configPath, "utf8"), before);
  const { result } = runAction(f, "install-keybindings", { env: { HERDR_CONFIG_PATH: configPath } });
  assert.deepEqual(result.added.map((item) => item.action), ["start-agent", "reconnect", "open-shell"]);
  assert.match(result.warnings.join("\n"), /prefix\+shift\+o is already bound to htop/);
  f.cleanup();
});

test("an unknown action and missing plugin directories are reported, not thrown", () => {
  const f = createFixture();
  assert.equal(runAction(f, "fetch-changes").result.errorKind, "target");
  const noState = runAction(f, "doctor", { env: { HERDR_PLUGIN_STATE_DIR: "" } });
  assert.equal(noState.result.errorKind, "startup");
  f.cleanup();
});

test("small helpers", () => {
  assert.equal(bridgeStartTimeout({}), 4000);
  assert.equal(bridgeStartTimeout({ HERDR_NONO_BRIDGE_START_TIMEOUT_MS: "25" }), 25);
  assert.equal(bridgeStartTimeout({ HERDR_NONO_BRIDGE_START_TIMEOUT_MS: "-1" }), 4000);
  assert.equal(entryCwd({ workdir: "/definitely/gone", localPath: "/" }), "/");
  assert.equal(entryCwd({ workdir: "/gone", localPath: "/also/gone" }), null);
  assert.equal(nonoVersionWarning("0.78.0"), null);
  assert.equal(nonoVersionWarning("1.0.0"), null);
  assert.match(nonoVersionWarning("0.77.9"), /older/);
  assert.equal(summarizeProfile({ network: { block: true } }).egress, "blocked");
  assert.deepEqual(parseKeybindingReport("config: /c\nbound prefix+shift+a -> nono.sandbox.start-agent\nalready bound: nono.sandbox.reconnect (prefix+shift+b)\nwarning: w\nreloaded\n"), { configPath: "/c", added: [{ key: "prefix+shift+a", action: "start-agent" }], existing: [{ key: "prefix+shift+b", action: "reconnect" }], warnings: ["w"], reloaded: true });
});

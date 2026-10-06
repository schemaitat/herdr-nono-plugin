import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { EventEmitter } from "node:events";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { test } from "node:test";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { collectSandboxes, fit, isStale, parseKeys, profileSource, renderSandboxes, renderTui, runSandboxesPane, sandboxTree } from "../src/sandboxes-pane-main.mjs";
import { createFixture, fakeBridgeProcess, mappingFor, paneCommand, writeFakeProc } from "./helpers.mjs";

const NAME = "herdr-opencode-abc123def456";
const ESC = String.fromCharCode(27);
const strip = (text) => text.replace(new RegExp(`${ESC}\\[[0-9;?]*[A-Za-z]`, "g"), "");

test("collectSandboxes merges mappings, live sessions and panes with one call each", async () => {
  const f = createFixture({
    panes: ({ worktree }) => ({
      "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1", lifecycleState: "running", port: 4242, serverProfile: "/p/server.json", verification: { ok: false, supported: true, processes: [], problems: ["Process 7 is not confined"] } }),
      "w1:p2": mappingFor({ worktree }, { paneId: "w1:p2", sessionName: "herdr-opencode-000000000002" }),
    }),
  });
  let nonoCalls = 0;
  let herdrCalls = 0;
  let profileCalls = 0;
  const nono = {
    async listSessions() { nonoCalls += 1; return [{ sessionId: "s1", name: NAME, supervisorPid: 5, status: "running" }, { sessionId: "s2", name: `${NAME}-shell`, status: "running" }, { sessionId: "s4", name: `${NAME}-server`, status: "running" }, { sessionId: "s3", name: "herdr-opencode-000000000002", status: "exited" }]; },
    async showProfile(ref) { profileCalls += 1; return { name: ref, network: /client/.test(ref) ? { block: true } : { allow_domain: ["*"] } }; },
  };
  const herdr = { async listPaneIds() { herdrCalls += 1; return ["w1:p1"]; } };
  const profileCache = new Map();
  const data = await collectSandboxes({ stateDir: f.stateDir, nono, herdr, profileCache, procRoot: path.join(f.root, "no-proc") });
  await collectSandboxes({ stateDir: f.stateDir, nono, herdr, profileCache, procRoot: path.join(f.root, "no-proc") });
  assert.equal(data.sessionError, null);
  assert.deepEqual(data.paneIds, ["w1:p1"]);
  assert.equal(nonoCalls, 2, "one nono ps per frame");
  assert.equal(herdrCalls, 2, "one pane list per frame");
  const byPane = Object.fromEntries(data.rows.map((row) => [row.paneId, row]));
  assert.equal(byPane["w1:p1"].running, true);
  assert.equal(byPane["w1:p1"].serverRunning, true);
  assert.equal(byPane["w1:p1"].shells, 1);
  assert.equal(byPane["w1:p1"].verified, false);
  assert.equal(byPane["w1:p2"].running, false, "an exited session does not count");
  assert.equal(byPane["w1:p2"].paneExists, false);
  assert.equal(isStale(byPane["w1:p2"]), true);
  assert.equal(isStale(byPane["w1:p1"]), false);
  assert.equal(byPane["w1:p1"].network.client.egress, "blocked");
  assert.equal(byPane["w1:p1"].network.server.egress, "allowlist");
  assert.equal(profileCalls, 3, "each distinct profile is resolved once and cached across frames");
  assert.deepEqual(byPane["w1:p1"].trees, { client: [], server: [] }, "no /proc, no trees");
  f.cleanup();
});

const SAMPLE_ROWS = () => [
  {
    paneId: "w1:p1", paneExists: true, sessionName: NAME, agentKind: "opencode", lifecycleState: "running", running: true, serverRunning: true, shells: 1, sessionId: "s1", verified: false, verification: "FAILED: Process 7 is not confined", localPath: "/home/u/projects/app",
    trees: {
      client: [{ pid: 101, depth: 0, confined: true, argv: ["/home/u/.opencode/bin/opencode", "--server", "http://127.0.0.1:4242"] }],
      server: [
        { pid: 111, depth: 0, confined: true, argv: ["/home/u/.opencode/bin/opencode", "serve", "--hostname", "127.0.0.1", "--port", "4242"] },
        { pid: 112, depth: 1, confined: true, argv: ["/usr/bin/bash", "-c", "npm test"] },
        { pid: 113, depth: 2, confined: false, argv: ["/usr/bin/node", "/home/u/projects/app/node_modules/.bin/vitest", "run"] },
      ],
    },
    network: { client: { egress: "blocked", allowDomains: [] }, server: { egress: "allowlist", allowDomains: ["*"] } },
    entry: { workspaceId: "w1", port: 4242, profile: "/p/profiles/herdr-opencode-client.json", serverProfile: "/p/profiles/herdr-opencode-server.json", verification: { ok: false, checkedAt: "2026-10-03T07:00:00Z", problems: ["Process 7 is not confined"] }, launchCount: 2, lastLaunchAt: "2026-10-03T06:59:00Z", lastExitCode: null },
  },
  { paneId: "w1:p2", paneExists: false, sessionName: "herdr-opencode-000000000002", agentKind: "opencode", lifecycleState: "exited", running: false, serverRunning: false, shells: 0, verified: true, verification: "confined: 2 processes", localPath: "/tmp/x", trees: { client: [], server: [] }, network: {}, entry: {} },
];

test("renderTui draws the agents table, the two sandboxes of the selected agent with their processes, and its details", () => {
  const rows = SAMPLE_ROWS();
  const lines = renderTui({ rows, sessionError: null, selected: 0, status: { text: "pruned 1 mapping", kind: "ok" } }, { width: 110, height: 32, color: false, at: new Date(0), home: "/home/u" });
  assert.equal(lines.length, 32);
  assert.ok(lines.every((line) => line.length <= 110), "nothing is wider than the terminal");
  const text = lines.join("\n");
  assert.match(lines[0], /^ nono sandboxes .*2 agents · 1 running · 1 FAILED · 1 stale/);
  assert.match(text, /┌─ Agents ─/);
  assert.match(text, /│ ▶● {2}w1:p1 +…abc123def456 +opencode +running +client\+server\+1 sh +✖ FAILED +~\/projects\/app/);
  assert.match(text, /│ {2}○ {2}w1:p2 ✗ +…000000000002 +opencode +exited +- +✔ ok +\/tmp\/x/);
  assert.match(text, / client ──:4242──▶ server ──▶ nono proxy ──▶ internet {2}✖ localhost ✖ Herdr ✖ systemd ✖ ssh-agent/);
  assert.match(text, /┌─ client · no network but :4242 ─+┐┌─ server · on :4242, proxy egress ─+┐/);
  assert.match(text, /│ ✔ {5}101 tui {4}opencode --server http:\/\/127\S* +││ ✔ {5}111 server opencode serve --hostname 1\S* +│/);
  assert.match(text, /││ ✔ {5}112 tool {3}└ bash -c npm test +│/);
  assert.match(text, /││ ✖ {5}113 tool {5}└ vitest run +│/, "an unconfined process is marked, interpreters show their script");
  assert.match(text, /┌─ Details ─/);
  assert.match(text, /Session +herdr-opencode-abc123def456/);
  assert.match(text, /Profiles +client herdr-opencode-client · server herdr-opencode-server/);
  assert.match(text, /Verified +FAILED: Process 7 is not confined/);
  assert.match(text, /✔ pruned 1 mapping/);
  assert.match(lines.at(-1), /↑↓\/jk +select +v +verify +x +stop +p +prune +i +profiles +r +refresh +q +quit/);
  const prompt = renderTui({ rows, sessionError: null, selected: 1, prompt: "Forget 1 mapping? [y/N]" }, { width: 110, height: 32, color: false });
  const promptText = prompt.join("\n");
  assert.match(promptText, /\? Forget 1 mapping\? \[y\/N\]/);
  assert.match(prompt.at(-1), /y +confirm +n\/esc +cancel/);
  assert.match(promptText, /▶○ {2}w1:p2/, "the selection follows the index");
  assert.match(promptText, /sandbox · network unknown.*not running/s, "an idle agent's sandbox says so");
  const open = SAMPLE_ROWS();
  open[0].network.server = { egress: "open", allowDomains: [] };
  assert.match(renderTui({ rows: open, sessionError: null, selected: 0 }, { width: 110, height: 32, color: false }).join("\n"), /server · on :4242, OPEN egress \+ localhost/);
  const colored = renderTui({ rows, sessionError: null, selected: 0 }, { width: 110, height: 32, color: true });
  assert.ok(colored.join("\n").includes(`${ESC}[35`), "the server side is magenta");
  assert.deepEqual(colored.map(strip).map((line) => line.trimEnd()), renderTui({ rows, sessionError: null, selected: 0 }, { width: 110, height: 32, color: false }).map((line) => line.trimEnd()), "colour changes nothing but the codes");
  const short = renderTui({ rows, sessionError: null, selected: 0 }, { width: 110, height: 20, color: false }).join("\n");
  assert.match(short, /┌─ server/, "on a short screen the sandboxes stay");
  assert.doesNotMatch(short, /┌─ Details/, "and the details panel goes");
});

test("collectSandboxes summarizes the profiles the next launch uses", async () => {
  const f = createFixture({ panes: () => ({}) });
  const nono = {
    async listSessions() { return []; },
    async showProfile(ref) { return { name: path.basename(ref, ".json"), description: `about ${ref}`, extends: ["nolabs-ai/opencode"], network: /client/.test(ref) ? { block: true } : { allow_domain: ["api.githubcopilot.com"] }, filesystem: { allow: ["$HOME/.opencode", "$TMPDIR"], read: ["$HOME/.agents"] }, linux: { af_unix_mediation: "pathname" } }; },
  };
  const herdr = { async listPaneIds() { return []; } };
  const data = await collectSandboxes({ stateDir: f.stateDir, nono, herdr, procRoot: path.join(f.root, "no-proc"), configured: { client: "/p/profiles/herdr-opencode-client.json", server: "/p/profiles/herdr-opencode-server.json" } });
  assert.equal(data.configured.client.summary.egress, "blocked");
  assert.deepEqual(data.configured.server.summary.allowDomains, ["api.githubcopilot.com"]);
  assert.deepEqual(data.configured.server.summary.readWritePaths, ["$HOME/.opencode", "$TMPDIR"]);
  assert.equal(data.configured.server.summary.description, "about /p/profiles/herdr-opencode-server.json");
  f.cleanup();
});

test("profileSource tells shipped profiles, profile files, nono user profiles and nono's own apart", () => {
  const userProfilesDir = mkdtempSync(path.join(tmpdir(), "herdr-nono-profiles-"));
  writeFileSync(path.join(userProfilesDir, "my-server.json"), "{}");
  const options = { pluginRoot: "/p", userProfilesDir };
  assert.equal(profileSource("/p/profiles/herdr-opencode-server.json", options).kind, "shipped");
  assert.deepEqual(profileSource("/home/u/my.json", options), { kind: "file", text: "your profile file", file: "/home/u/my.json" });
  assert.deepEqual(profileSource("my-server", options), { kind: "user", text: "nono user profile", file: path.join(userProfilesDir, "my-server.json") });
  assert.equal(profileSource("node-dev", options).kind, "nono");
  assert.equal(profileSource(null, options).kind, "none");
});

test("renderTui shows the profiles view: which profile each sandbox runs, where it lives and what it allows", () => {
  const rows = SAMPLE_ROWS();
  const summary = (name, network) => ({ name, description: `${name} policy`, extends: ["nolabs-ai/opencode"], egress: network, allowDomains: network === "allowlist" ? ["api.githubcopilot.com", "github.com"] : [], loopback: false, loopbackDomains: [], afUnixMediation: "pathname", workdirAccess: "readwrite", readWritePaths: ["a", "b"], readOnlyPaths: ["c"] });
  rows[0].network = { client: summary("herdr-opencode-client", "blocked"), server: summary("herdr-opencode-server", "allowlist") };
  const configured = { client: { ref: "/p/profiles/herdr-opencode-client.json", summary: rows[0].network.client }, server: { ref: "/home/u/mine.json", summary: rows[0].network.server } };
  const profiles = { pluginRoot: "/p", configFile: "/home/u/.config/herdr/plugins/nono.sandbox/config.json", userProfilesDir: "/home/u/.config/nono/profiles" };
  const lines = renderTui({ rows, sessionError: null, selected: 0, mode: "profiles", configured, profiles }, { width: 110, height: 40, color: false, home: "/home/u" });
  assert.equal(lines.length, 40);
  assert.ok(lines.every((line) => line.length <= 110));
  const text = lines.join("\n");
  assert.match(text, /┌─ Profiles · agent w1:p1 ─/);
  assert.doesNotMatch(text, /┌─ Details/, "the profiles view replaces the sandboxes and details");
  assert.match(text, /server sandbox: opencode serve and every tool call · config key serverProfile/);
  assert.match(text, /Profile +herdr-opencode-server · shipped with the plugin/);
  assert.match(text, /File +\/p\/profiles\/herdr-opencode-server\.json/);
  assert.match(text, /Network +nono proxy to 2 hosts: api\.githubcopilot\.com, github\.com/);
  assert.match(text, /Network +blocked; the plugin opens its server's port only/);
  assert.match(text, /Files +2 read-write, 1 read-only directory, plus the workspace root/);
  assert.match(text, /Sockets +AF_UNIX pathname mediation/);
  assert.match(text, /About +herdr-opencode-server policy/);
  assert.match(text, /Next launch +server mine · client herdr-opencode-client; prefix\+shift\+b after quitting switches/);
  assert.match(text, /Config +~\/\.config\/herdr\/plugins\/nono\.sandbox\/config\.json/);
  assert.match(text, /user profiles in ~\/\.config\/nono\/profiles/);
  assert.match(lines.at(-1), /i +sandboxes/);
  const empty = renderTui({ rows: [], sessionError: null, mode: "profiles", configured, profiles }, { width: 80, height: 40, color: false, home: "/home/u" }).join("\n");
  assert.match(empty, /Profiles · next launch/);
  assert.match(empty, /Profile +herdr-opencode-server · your profile file/);
  const narrow = renderTui({ rows, sessionError: null, selected: 0 }, { width: 70, height: 30, color: false });
  assert.ok(narrow.at(-1).length <= 70, "the key hints fit a narrow screen");
  assert.match(narrow.at(-1), /i +profiles/, "the profiles key stays when refresh and prune hints go");
});

test("sandboxTree lists a supervisor's processes depth-first with their confinement", () => {
  const root = mkdtempSync(path.join(tmpdir(), "herdr-nono-proc-"));
  writeFakeProc(root, [
    { pid: 110, ppid: 1, argv: ["nono"], noNewPrivs: false },
    { pid: 111, ppid: 110, argv: ["opencode", "serve"] },
    { pid: 112, ppid: 111, argv: ["bash", "-c", "a"] },
    { pid: 114, ppid: 111, argv: ["bash", "-c", "b"] },
    { pid: 113, ppid: 112, argv: ["sleep", "5"], noNewPrivs: false },
  ]);
  assert.deepEqual(sandboxTree(110, root).map((item) => [item.pid, item.depth, item.confined]), [[111, 0, true], [112, 1, true], [113, 2, false], [114, 1, true]]);
  assert.deepEqual(sandboxTree(null, root), []);
  assert.deepEqual(sandboxTree(110, path.join(root, "absent")), []);
});

test("renderTui says what to do when nothing is mapped or nono fails", () => {
  const text = renderTui({ rows: [], sessionError: "nono ps failed (exit 1)" }, { width: 80, height: 24, color: false }).join("\n");
  assert.match(text, /No sandboxed agents\. Press ctrl\+b, shift\+a/);
  assert.match(text, /nono ps failed: nono ps failed \(exit 1\)/);
  assert.match(renderSandboxes({ rows: [], sessionError: null }).join("\n"), /0 agents · 0 running/);
});

test("fit and parseKeys", () => {
  assert.equal(fit("abcdef", 4), "abc…");
  assert.equal(fit("/a/b/c/d", 5, { fromLeft: true }), "…/c/d");
  assert.equal(fit("ab", 4), "ab  ");
  assert.deepEqual(parseKeys(`${ESC}[A${ESC}[Bjkq`), ["up", "down", "j", "k", "q"]);
  assert.deepEqual(parseKeys(`${ESC}[I${ESC}[5~Y${ESC}`), ["pageup", "y", "escape"]);
  assert.deepEqual(parseKeys(String.fromCharCode(3)), ["ctrl-c"]);
});

/** A fake terminal: keypresses go in through `press`, frames come out in `frames`. */
function fakeTerminal() {
  const input = Object.assign(new EventEmitter(), { isTTY: true, setRawMode() {}, resume() {}, pause() {} });
  const output = Object.assign(new EventEmitter(), { isTTY: false, columns: 110, rows: 30, chunks: [], write(chunk) { this.chunks.push(String(chunk)); return true; } });
  return {
    input,
    output,
    press: (keys) => input.emit("data", keys),
    screen: () => strip(output.chunks.join("").split(`${ESC}[H`).pop() ?? ""),
  };
}

async function waitFor(predicate, what) {
  const deadline = Date.now() + 8000;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await sleep(50);
  }
  assert.fail(`timed out waiting for ${what}`);
}

test("the TUI prunes stale mappings after a confirmation, and cancels without one", async () => {
  const f = createFixture({
    panes: ({ worktree }) => ({
      "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1" }),
      "w1:p2": mappingFor({ worktree }, { paneId: "w1:p2", sessionName: "herdr-opencode-000000000002" }),
    }),
  });
  const term = fakeTerminal();
  const done = runSandboxesPane(f.env({ FAKE_HERDR_MISSING_PANES: "w1:p2" }), { input: term.input, output: term.output, intervalMs: 60_000 });
  await waitFor(() => term.screen().includes("1 stale"), "the first frame");
  term.press("p");
  await waitFor(() => term.screen().includes("Forget 1 mapping whose pane is gone (w1:p2)? [y/N]"), "the prune prompt");
  term.press("n");
  await waitFor(() => term.screen().includes("cancelled"), "the cancellation");
  assert.equal(Object.keys(f.mappings().panes).length, 2, "nothing was pruned without a yes");
  term.press("p");
  await waitFor(() => term.screen().includes("[y/N]"), "the prompt again");
  term.press("y");
  await waitFor(() => term.screen().includes("pruned 1 mapping"), "the prune result");
  assert.deepEqual(Object.keys(f.mappings().panes), ["w1:p1"]);
  await waitFor(() => !term.screen().includes("stale"), "the refreshed table");
  term.press("p");
  await waitFor(() => term.screen().includes("nothing to prune"), "the empty prune");
  term.press("q");
  assert.equal(await done, 0);
  assert.ok(term.output.chunks.join("").endsWith(`${ESC}[?25h${ESC}[?1049l`), "the cursor and the main screen are restored");
  f.cleanup();
});

test("the TUI moves the selection and refuses to verify or stop an agent that is not running", async () => {
  const f = createFixture({
    panes: ({ worktree }) => ({
      "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1" }),
      "w1:p2": mappingFor({ worktree }, { paneId: "w1:p2", sessionName: "herdr-opencode-000000000002" }),
    }),
  });
  const term = fakeTerminal();
  const done = runSandboxesPane(f.env(), { input: term.input, output: term.output, intervalMs: 60_000 });
  await waitFor(() => term.screen().includes("▶○  w1:p1"), "the first frame");
  term.press(`${ESC}[B`);
  await waitFor(() => term.screen().includes("▶○  w1:p2"), "the selection to move down");
  term.press("v");
  await waitFor(() => term.screen().includes("is not running; nothing to verify"), "the verify refusal");
  term.press("x");
  await waitFor(() => term.screen().includes("herdr-opencode-000000000002 is not running"), "the stop refusal");
  term.press("k");
  await waitFor(() => term.screen().includes("▶○  w1:p1"), "the selection to move up");
  term.press(String.fromCharCode(3));
  assert.equal(await done, 0);
  f.cleanup();
});

test("the TUI stops a running agent after a confirmation", async () => {
  const bridge = fakeBridgeProcess("w1:p1");
  const f = createFixture({
    sessions: [{ session_id: "s1", name: NAME, supervisor_pid: bridge.pid, status: "running" }],
    panes: ({ worktree }) => ({ "w1:p1": mappingFor({ worktree }, { paneId: "w1:p1", lifecycleState: "running", bridgePid: bridge.pid }) }),
  });
  const term = fakeTerminal();
  const done = runSandboxesPane(f.env(), { input: term.input, output: term.output, intervalMs: 60_000 });
  await waitFor(() => term.screen().includes("▶●"), "the running row");
  term.press("x");
  await waitFor(() => term.screen().includes(`Stop ${NAME} (pane w1:p1)? [y/N]`), "the stop prompt");
  term.press("y");
  // The fake nono's stop kills the recorded supervisor, here the fake bridge.
  await waitFor(() => term.screen().includes(`stopped ${NAME}`), "the stop result");
  assert.deepEqual(f.nonoCalls().filter((call) => call.argv[0] === "stop").map((call) => call.argv), [["stop", "s1"]]);
  term.press("q");
  assert.equal(await done, 0);
  bridge.stop();
  f.cleanup();
});

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

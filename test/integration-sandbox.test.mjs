/**
 * Integration tests for what the shipped profiles confine besides egress
 * (that is test/integration-egress.test.mjs), against the real nono CLI: Unix
 * sockets such as Herdr's control socket (`herdr pane run` would run commands
 * on the host), the host's environment, doctor's escape probe for both
 * profiles, and the client and server joined by one port.
 *
 * They need nono (HERDR_NONO_BIN, else `nono` on PATH) with the
 * `nolabs-ai/opencode` pack; without it every test is skipped, and
 * HERDR_NONO_INTEGRATION=0 skips them too. The Herdr test also needs a running
 * Herdr server and the herdr CLI; it only ever addresses a pane id that does
 * not exist, so a reached server answers `pane_not_found` and runs nothing.
 */
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { homedir, tmpdir } from "node:os";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { SHIPPED_PROFILES, runProbes } from "./helpers.mjs";

/** An executable as an absolute path, or the bare name when it is not on PATH. */
function which(bin) {
  if (path.isAbsolute(bin)) return bin;
  return (process.env.PATH ?? "").split(path.delimiter).map((dir) => path.join(dir, bin)).find((file) => existsSync(file)) ?? bin;
}

const NONO = which(process.env.HERDR_NONO_BIN || "nono");
const HERDR = which(process.env.HERDR_BIN_PATH || "herdr");
const HERDR_SOCKET = process.env.HERDR_SOCKET_PATH || path.join(process.env.XDG_CONFIG_HOME || path.join(homedir(), ".config"), "herdr", "herdr.sock");
const PROFILES = SHIPPED_PROFILES;
/** A pane id Herdr never hands out: a reached server answers pane_not_found. */
const MISSING_PANE = "zz:p999999";

/** Why the real-nono tests cannot run here, or null when they can. */
function skipReason() {
  if (process.env.HERDR_NONO_INTEGRATION === "0") return "HERDR_NONO_INTEGRATION=0";
  const version = spawnSync(NONO, ["--version"], { encoding: "utf8", timeout: 10_000 });
  if (version.error || version.status !== 0 || !/^nono \d/m.test(version.stdout)) return `no real nono at "${NONO}"`;
  const pack = spawnSync(NONO, ["profile", "show", "--json", PROFILES.server], { encoding: "utf8", timeout: 20_000 });
  if (pack.status !== 0) return "nono cannot resolve the shipped server profile (is the nolabs-ai/opencode pack installed?)";
  return null;
}

const SKIP = skipReason();

/** Why the real-Herdr test cannot run here, or null when it can. */
function herdrSkipReason() {
  if (SKIP) return SKIP;
  if (!existsSync(HERDR_SOCKET)) return `no Herdr control socket at ${HERDR_SOCKET}`;
  const probe = spawnSync(HERDR, ["pane", "run", MISSING_PANE, "true"], { encoding: "utf8", timeout: 10_000, env: { ...process.env, HERDR_SOCKET_PATH: HERDR_SOCKET } });
  if (probe.error) return `no herdr CLI at "${HERDR}"`;
  if (!/pane_not_found/.test(probe.stdout + probe.stderr)) return "the Herdr server does not answer on its socket";
  return null;
}

/**
 * Runs a command in a throwaway nono sandbox and collects its output. The
 * workspace is the only `--allow` and the working directory, as for an agent.
 * @param {string} profile
 * @param {string[]} argv
 * @param {{workspace: string, extraArgs?: string[], env?: NodeJS.ProcessEnv, timeoutMs?: number}} options
 * @returns {Promise<{status: number|null, output: string}>}
 */
async function inSandbox(profile, argv, { workspace, extraArgs = [], env = process.env, timeoutMs = 60_000 }) {
  const child = spawn(NONO, ["run", "--silent", "--profile", profile, "--allow", workspace, ...extraArgs, "--", ...argv], { cwd: workspace, env, stdio: ["ignore", "pipe", "pipe"] });
  let output = "";
  child.stdout.on("data", (chunk) => { output += chunk; });
  child.stderr.on("data", (chunk) => { output += chunk; });
  const killer = setTimeout(() => child.kill("SIGKILL"), timeoutMs);
  const status = await new Promise((resolve) => child.once("close", resolve));
  clearTimeout(killer);
  return { status, output };
}

/**
 * Runs inside the sandbox: connects to each target (a Unix socket path or
 * `tcp:<host>:<port>`) and prints one JSON line mapping it to `connected`
 * plus what the peer sent, or the error code.
 */
const CONNECT_SCRIPT = String.raw`
const net = require("node:net");
const targets = JSON.parse(process.argv[1]);
function attempt(target) {
  return new Promise((resolve) => {
    let settled = false;
    let data = "";
    const tcp = target.startsWith("tcp:") ? target.slice(4).split(":") : null;
    const socket = tcp ? net.connect({ host: tcp[0], port: Number(tcp[1]) }) : net.connect(target);
    const done = (value) => { if (!settled) { settled = true; clearTimeout(timer); socket.destroy(); resolve(value); } };
    socket.on("data", (chunk) => { data += chunk; });
    socket.on("end", () => done("connected:" + data));
    socket.on("error", (error) => done(error.code || "error"));
    const timer = setTimeout(() => done("timeout"), 5000);
  });
}
(async () => {
  const results = {};
  for (const target of targets) results[target] = await attempt(target);
  process.stdout.write("RESULTS " + JSON.stringify(results) + "\n");
})();
`;

/**
 * Connects to targets from inside a sandbox; see CONNECT_SCRIPT.
 * @param {string} profile
 * @param {string[]} targets
 * @param {{workspace: string, extraArgs?: string[]}} options
 * @returns {Promise<Record<string, string>>}
 */
async function connectFrom(profile, targets, options) {
  const { output } = await inSandbox(profile, [process.execPath, "-e", CONNECT_SCRIPT, JSON.stringify(targets)], options);
  const line = output.split("\n").find((item) => item.startsWith("RESULTS "));
  assert.ok(line, `the sandboxed script did not report:\n${output}`);
  return JSON.parse(line.slice("RESULTS ".length));
}

/**
 * Listens on a Unix socket or a host loopback port and greets every client
 * with `name`, standing in for Herdr, systemd, an SSH agent or a host service.
 * @param {string|number} where
 * @param {string} name
 */
async function canary(where, name) {
  const server = createServer((socket) => socket.end(name));
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    if (typeof where === "number") server.listen(where, "127.0.0.1", () => resolve(undefined));
    else server.listen(where, () => resolve(undefined));
  });
  const address = server.address();
  return { port: typeof address === "object" && address ? address.port : null, close: () => new Promise((resolve) => server.close(() => resolve(undefined))) };
}

/**
 * Writes a copy of a shipped profile with `change` applied to its JSON.
 * @param {string} dir
 * @param {"server"|"client"} side
 * @param {string} name
 * @param {(profile: Record<string, any>) => void} change
 */
function variantProfile(dir, side, name, change) {
  const profile = JSON.parse(readFileSync(PROFILES[side], "utf8"));
  profile.meta.name = name;
  change(profile);
  const file = path.join(dir, `${name}.json`);
  writeFileSync(file, JSON.stringify(profile, null, 2));
  return file;
}

const DENIED = /^(EACCES|EPERM)$/;

describe("sandbox confinement with the real nono", { skip: SKIP ?? false, concurrency: false, timeout: 300_000 }, () => {
  let workspace;
  let outside;
  let profiles;
  before(() => {
    // Short paths: a Unix socket path must fit in 108 bytes.
    workspace = mkdtempSync(path.join(tmpdir(), "hn-ws-"));
    outside = mkdtempSync(path.join(tmpdir(), "hn-host-"));
    profiles = mkdtempSync(path.join(tmpdir(), "hn-prof-"));
  });
  after(() => {
    for (const dir of [workspace, outside, profiles]) if (dir) rmSync(dir, { recursive: true, force: true });
  });

  test("neither sandbox can connect to a host Unix socket, even one in a directory it may write", async () => {
    // /tmp is granted read-write by the OpenCode pack, and the workspace by the
    // plugin; AF_UNIX pathname mediation still refuses the connect.
    const sockets = [path.join(outside, "herdr.sock"), path.join(workspace, "agent.sock")];
    const servers = await Promise.all(sockets.map((file) => canary(file, "host")));
    try {
      for (const side of ["server", "client"]) {
        const results = await connectFrom(PROFILES[side], sockets, { workspace });
        for (const file of sockets) assert.match(results[file], DENIED, `${side} sandbox reached ${file}: ${JSON.stringify(results)}`);
      }
      // Control: without the mediation setting the same connect succeeds, so
      // the denial above comes from af_unix_mediation and the test can fail.
      const unmediated = variantProfile(profiles, "server", "herdr-test-no-mediation", (profile) => { delete profile.linux; });
      const open = await connectFrom(unmediated, sockets, { workspace });
      for (const file of sockets) assert.equal(open[file], "connected:host", JSON.stringify(open));
    } finally {
      await Promise.all(servers.map((server) => server.close()));
    }
  });

  test("herdr pane run cannot reach the Herdr server from either sandbox", async (t) => {
    const reason = herdrSkipReason();
    if (reason) {
      t.skip(reason);
      return;
    }
    // The socket path is passed by hand, as an attacker who knows Herdr's default would.
    const env = { ...process.env, HERDR_SOCKET_PATH: HERDR_SOCKET };
    const argv = ["/bin/sh", "-c", `HERDR_SOCKET_PATH='${HERDR_SOCKET}' '${HERDR}' pane run ${MISSING_PANE} true`];
    for (const side of ["server", "client"]) {
      const { output } = await inSandbox(PROFILES[side], argv, { workspace, env });
      assert.match(output, /PermissionDenied|Permission denied|Operation not permitted/, `${side}: ${output}`);
      assert.doesNotMatch(output, /pane_not_found/, `${side} sandbox reached the Herdr server: ${output}`);
    }
    const unmediated = variantProfile(profiles, "server", "herdr-test-no-mediation-herdr", (profile) => { delete profile.linux; });
    const { output } = await inSandbox(unmediated, argv, { workspace, env });
    assert.match(output, /pane_not_found/, `control: without af_unix_mediation the Herdr server answers: ${output}`);
  });

  test("the host's Herdr, SSH, GPG, D-Bus and tmux variables do not reach either sandbox", async () => {
    const leaked = { HERDR_SOCKET_PATH: "/x/herdr.sock", HERDR_PANE_ID: "w1:p1", HERDR_ENV: "1", SSH_AUTH_SOCK: "/x/agent", SSH_AGENT_PID: "1", GPG_AGENT_INFO: "/x/gpg", DBUS_SESSION_BUS_ADDRESS: "unix:path=/x/bus", TMUX: "/x/tmux,1,0", TMUX_PANE: "%1" };
    const env = { ...process.env, ...leaked };
    for (const side of ["server", "client"]) {
      const { output } = await inSandbox(PROFILES[side], [process.execPath, "-e", `process.stdout.write("ENV " + JSON.stringify(Object.keys(process.env).filter((key) => ${JSON.stringify(Object.keys(leaked))}.includes(key))) + "\\n")`], { workspace, env });
      const line = output.split("\n").find((item) => item.startsWith("ENV "));
      assert.ok(line, output);
      assert.deepEqual(JSON.parse(line.slice(4)), [], `${side} sandbox sees host variables`);
    }
  });

  test("doctor's escape probe passes every check for both shipped profiles", async () => {
    // Herdr's socket, systemd, D-Bus, Docker, the SSH and GPG agents, ~/.ssh,
    // writes to the home directory, the host service port, a loopback canary
    // directly and through the proxy, leaked variables and nono's marker.
    for (const side of ["server", "client"]) {
      const { checks } = await runProbes({ nonoBin: NONO, profile: PROFILES[side] });
      const failed = checks.filter((check) => !check.ok && check.severity !== "warning");
      assert.deepEqual(failed, [], `${side} profile: ${JSON.stringify(failed)}`);
      assert.ok(checks.some((check) => check.check === "herdrSocket"), "the probe covers Herdr's socket");
    }
  });

  test("the sandboxes may write the workspace and nothing in the home directory", async () => {
    const script = `const fs = require("node:fs"); const out = {}; for (const [name, file] of Object.entries(JSON.parse(process.argv[1]))) { try { fs.writeFileSync(file, ""); fs.unlinkSync(file); out[name] = "written"; } catch (error) { out[name] = error.code; } } process.stdout.write("FILES " + JSON.stringify(out) + "\\n");`;
    const targets = { workspace: path.join(workspace, "probe"), home: path.join(homedir(), `.herdr-nono-test-${process.pid}`), bashrc: path.join(homedir(), ".bashrc.herdr-nono-test") };
    for (const side of ["server", "client"]) {
      const { output } = await inSandbox(PROFILES[side], [process.execPath, "-e", script, JSON.stringify(targets)], { workspace });
      const line = output.split("\n").find((item) => item.startsWith("FILES "));
      assert.ok(line, output);
      const results = JSON.parse(line.slice(6));
      assert.equal(results.workspace, "written", `${side}: the workspace is the agent's to write`);
      assert.match(results.home, DENIED, `${side}: ${JSON.stringify(results)}`);
      assert.match(results.bashrc, DENIED, `${side}: ${JSON.stringify(results)}`);
    }
  });

  test("client and server join on one port: the server listens only there, the client reaches only it", async () => {
    const host = await canary(0, "host");
    const port = 20_000 + (process.pid % 20_000);
    const serverScript = `const net = require("node:net"); const s = net.createServer((c) => c.end("server")); s.on("error", (e) => { console.log("LISTEN " + e.code); process.exit(1); }); s.listen(${port}, "127.0.0.1", () => { console.log("LISTENING"); const other = net.createServer(); other.on("error", (e) => console.log("OTHER " + e.code)); other.listen(${port + 1}, "127.0.0.1", () => console.log("OTHER LISTENING")); }); setTimeout(() => process.exit(0), 30000);`;
    const server = spawn(NONO, ["run", "--silent", "--profile", PROFILES.server, "--allow", workspace, "--listen-port", String(port), "--", process.execPath, "-e", serverScript], { cwd: workspace, stdio: ["ignore", "pipe", "pipe"] });
    let serverOutput = "";
    try {
      await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error(`the server sandbox did not listen: ${serverOutput}`)), 30_000);
        const onData = (chunk) => {
          serverOutput += chunk;
          if (/OTHER (LISTENING|E[A-Z]+)/.test(serverOutput)) { clearTimeout(timer); resolve(undefined); }
          if (/LISTEN E/.test(serverOutput)) { clearTimeout(timer); reject(new Error(serverOutput)); }
        };
        server.stdout.on("data", onData);
        server.stderr.on("data", onData);
        server.once("close", () => { clearTimeout(timer); reject(new Error(`the server sandbox exited: ${serverOutput}`)); });
      });
      assert.match(serverOutput, /OTHER EACCES|OTHER EPERM/, "the server sandbox may listen on its --listen-port only");
      const targets = [`tcp:127.0.0.1:${port}`, `tcp:127.0.0.1:${host.port}`, "tcp:1.1.1.1:443"];
      const results = await connectFrom(PROFILES.client, targets, { workspace, extraArgs: ["--open-port", String(port)] });
      assert.equal(results[`tcp:127.0.0.1:${port}`], "connected:server", `the client reaches its server: ${JSON.stringify(results)}`);
      assert.match(results[`tcp:127.0.0.1:${host.port}`], DENIED, "the client reaches no other loopback port");
      assert.match(results["tcp:1.1.1.1:443"], DENIED, "the client reaches no internet host");
    } finally {
      server.kill("SIGTERM");
      await host.close();
    }
  });
});

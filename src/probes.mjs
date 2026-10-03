/**
 * Escape probes for `doctor`: a short Node script runs inside a throwaway nono
 * sandbox with the profile the agent's tools run under and tries the ways out
 * this plugin knows about (Herdr's control socket, the user's systemd and
 * D-Bus sockets, the SSH and GPG agents, a direct connect to the OpenCode host
 * service's port, secrets in the home directory, leaked environment
 * variables, the host service's password). Each check says what a
 * well-confined agent should see.
 * @module probes
 */
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import path from "node:path";
import { NONO_CALL_TIMEOUT_MS } from "./constants.mjs";
import { PluginError } from "./errors.mjs";
import { opencodeServiceFiles } from "./hostservice.mjs";

/**
 * The script that runs inside the sandbox. It receives its targets as JSON in
 * argv and prints one JSON line of results; it never prints file contents.
 */
const PROBE_SCRIPT = String.raw`
const net = require("node:net");
const fs = require("node:fs");
const targets = JSON.parse(process.argv[1]);
const results = {};
function connect(file) {
  return new Promise((resolve) => {
    if (!file) return resolve("absent");
    let settled = false;
    const done = (value) => { if (!settled) { settled = true; resolve(value); } };
    try { fs.statSync(file); } catch (error) { if (error.code === "ENOENT") return done("absent"); }
    const socket = net.connect(file);
    socket.on("connect", () => { socket.destroy(); done("allowed"); });
    socket.on("error", (error) => done(error.code === "ENOENT" ? "absent" : error.code === "ECONNREFUSED" ? "refused" : "denied"));
    setTimeout(() => { socket.destroy(); done("timeout"); }, 2000);
  });
}
function tcp(port) {
  return new Promise((resolve) => {
    let settled = false;
    const done = (value) => { if (!settled) { settled = true; resolve(value); } };
    const socket = net.connect({ host: "127.0.0.1", port });
    socket.on("connect", () => { socket.destroy(); done("allowed"); });
    socket.on("error", (error) => done(error.code === "ECONNREFUSED" ? "refused" : (error.code === "EACCES" || error.code === "EPERM") ? "denied" : error.code || "error"));
    setTimeout(() => { socket.destroy(); done("timeout"); }, 2000);
  });
}
function readable(file) {
  try { fs.readFileSync(file).subarray(0, 1); return "allowed"; } catch (error) { return error.code === "ENOENT" ? "absent" : "denied"; }
}
function listable(dir) {
  try { fs.readdirSync(dir); return "allowed"; } catch (error) { return error.code === "ENOENT" ? "absent" : "denied"; }
}
function writable(dir) {
  const file = dir + "/.herdr-nono-probe-" + process.pid;
  try { fs.writeFileSync(file, ""); fs.unlinkSync(file); return "allowed"; } catch (error) { return error.code === "ENOENT" ? "absent" : "denied"; }
}
(async () => {
  for (const [name, file] of Object.entries(targets.sockets)) results[name] = await connect(file);
  for (const [name, port] of Object.entries(targets.tcp)) results[name] = await tcp(port);
  for (const [name, file] of Object.entries(targets.files)) results[name] = readable(file);
  for (const [name, dir] of Object.entries(targets.dirs)) results[name] = listable(dir);
  for (const [name, dir] of Object.entries(targets.writes)) results[name] = writable(dir);
  results.env = Object.keys(process.env).filter((key) => /^(HERDR_|SSH_AUTH_SOCK$|DBUS_SESSION_BUS_ADDRESS$)/.test(key)).sort();
  results.marker = Boolean(process.env.NONO_CAP_FILE);
  process.stdout.write("HERDR_NONO_PROBE " + JSON.stringify(results) + "\n");
})();
`;

/**
 * The checks the probe runs and what a confined agent should see.
 * @param {NodeJS.ProcessEnv} env
 */
export function probeTargets(env = process.env) {
  const home = env.HOME || homedir();
  const runtime = env.XDG_RUNTIME_DIR || (typeof process.getuid === "function" ? `/run/user/${process.getuid()}` : null);
  const herdrSocket = env.HERDR_SOCKET_PATH || path.join(env.XDG_CONFIG_HOME || path.join(home, ".config"), "herdr", "herdr.sock");
  const service = opencodeServiceFiles(env);
  let servicePort = 4096;
  try {
    servicePort = Number(new URL(JSON.parse(readFileSync(service.stateFile, "utf8")).url).port) || servicePort;
  } catch {
    // No service file: OpenCode's default port.
  }
  return {
    tcp: {
      opencodeServicePort: servicePort,
    },
    sockets: {
      herdrSocket,
      systemdUser: runtime ? path.join(runtime, "systemd", "private") : null,
      sessionBus: runtime ? path.join(runtime, "bus") : null,
      sshAgent: env.SSH_AUTH_SOCK || (runtime ? path.join(runtime, "openssh_agent") : null),
      gpgAgent: runtime ? path.join(runtime, "gnupg", "S.gpg-agent") : null,
      dockerSocket: "/var/run/docker.sock",
    },
    files: {
      opencodeServicePassword: service.stateFile,
    },
    dirs: {
      sshKeys: path.join(home, ".ssh"),
    },
    writes: {
      homeDirectory: home,
    },
  };
}

/** What each probe result should be for a confined agent, and why it matters. */
export const PROBE_EXPECTATIONS = Object.freeze({
  herdrSocket: { expect: ["denied", "absent"], severity: "critical", why: "Herdr's control socket can type commands into any pane, outside the sandbox" },
  opencodeServicePort: { expect: ["denied"], severity: "critical", why: "a direct localhost connect reaches the OpenCode host service, which runs tools outside the sandbox" },
  systemdUser: { expect: ["denied", "absent"], severity: "critical", why: "the user's systemd manager can start processes outside the sandbox" },
  sessionBus: { expect: ["denied", "absent"], severity: "critical", why: "the D-Bus session bus can ask systemd to start processes outside the sandbox" },
  dockerSocket: { expect: ["denied", "absent"], severity: "critical", why: "the Docker socket gives root-equivalent access to the host" },
  sshAgent: { expect: ["denied", "absent"], severity: "high", why: "the SSH agent signs with your keys" },
  gpgAgent: { expect: ["denied", "absent"], severity: "high", why: "the GPG agent signs and decrypts with your keys" },
  sshKeys: { expect: ["denied", "absent"], severity: "high", why: "~/.ssh holds private keys" },
  homeDirectory: { expect: ["denied"], severity: "high", why: "writes to the home directory can plant shell start-up files" },
  opencodeServicePassword: { expect: ["denied", "absent"], severity: "warning", why: "it holds the password of the OpenCode host service; harmless while the service's port is unreachable (opencodeServicePort)" },
  env: { expect: [], severity: "high", why: "these variables point sandboxed code at sockets outside the sandbox" },
});

/**
 * Evaluates raw probe results into a list of checks.
 * @param {Record<string, any>} results
 * @returns {Array<{check: string, result: string, ok: boolean, severity: string, why: string}>}
 */
export function evaluateProbes(results) {
  const checks = [];
  for (const [check, expectation] of Object.entries(PROBE_EXPECTATIONS)) {
    if (!(check in results)) continue;
    const value = results[check];
    if (check === "env") {
      checks.push({ check, result: value.length === 0 ? "none" : value.join(" "), ok: value.length === 0, severity: expectation.severity, why: expectation.why });
      continue;
    }
    checks.push({ check, result: value, ok: expectation.expect.includes(value), severity: expectation.severity, why: expectation.why });
  }
  checks.push({ check: "marker", result: results.marker ? "NONO_CAP_FILE set" : "NONO_CAP_FILE missing", ok: Boolean(results.marker), severity: "critical", why: "a sandboxed process carries nono's capability marker" });
  return checks;
}

/**
 * Runs the probe inside a throwaway sandbox with the given profile. The
 * probe's environment deliberately keeps HERDR_* and the socket variables, so
 * the result shows whether the profile itself strips them.
 * @param {{nonoBin: string, profile: string, env?: NodeJS.ProcessEnv, nodeBin?: string, timeoutMs?: number}} input
 */
export function runProbes({ nonoBin, profile, env = process.env, nodeBin = process.execPath, timeoutMs = NONO_CALL_TIMEOUT_MS }) {
  const workspace = mkdtempSync(path.join(tmpdir(), "herdr-nono-probe-"));
  try {
    const targets = probeTargets(env);
    const args = ["run", "--silent", "--profile", profile, "--name", "herdr-nono-probe", "--allow", workspace, "--", nodeBin, "-e", PROBE_SCRIPT, JSON.stringify(targets)];
    const result = spawnSync(nonoBin, args, { cwd: workspace, encoding: "utf8", env, timeout: timeoutMs, killSignal: "SIGKILL", maxBuffer: 4 * 1024 * 1024 });
    const output = `${result.stdout ?? ""}${result.stderr ?? ""}`;
    if (result.error) {
      throw new PluginError("startup", `Could not run the escape probe through nono: ${result.error.message}`, { output });
    }
    const line = (result.stdout ?? "").split("\n").find((item) => item.startsWith("HERDR_NONO_PROBE "));
    if (!line) {
      throw new PluginError("unknown", `The escape probe did not report (nono exit ${result.status}); the profile may not grant ${nodeBin}.`, { output });
    }
    return { targets, checks: evaluateProbes(JSON.parse(line.slice("HERDR_NONO_PROBE ".length))) };
  } finally {
    rmSync(workspace, { recursive: true, force: true });
  }
}

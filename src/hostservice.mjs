/**
 * Detection of OpenCode's host background service (`opencode serve
 * --service`). The service runs outside any sandbox, executes tools on the
 * host, and publishes its URL and password in OpenCode's state and config
 * directories, which the `nolabs-ai/opencode` nono profile grants read-write
 * to the sandbox (Landlock cannot carve a single file out of a granted
 * directory). With unrestricted egress a sandboxed agent can therefore read
 * the password and drive the host service: a way out of the sandbox. The
 * plugin cannot close that hole from inside the profile, so it detects the
 * service and warns, or refuses to launch (`hostServiceCheck`).
 * This module reads the URL, pid and port only; it never reads the password.
 * @module hostservice
 */
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";
import { readProcess, TCP_LISTEN, tcpConnections } from "./procfs.mjs";

/**
 * Where OpenCode records its background service.
 * @param {NodeJS.ProcessEnv} [env]
 * @returns {{stateFile: string, configFile: string}}
 */
export function opencodeServiceFiles(env = process.env) {
  const home = env.HOME || homedir();
  const state = env.XDG_STATE_HOME || path.join(home, ".local", "state");
  const config = env.XDG_CONFIG_HOME || path.join(home, ".config");
  return { stateFile: path.join(state, "opencode", "service.json"), configFile: path.join(config, "opencode", "service.json") };
}

function readJson(file) {
  try {
    return JSON.parse(readFileSync(file, "utf8"));
  } catch {
    return null;
  }
}

function portOf(url) {
  try {
    const parsed = new URL(url);
    return parsed.port ? Number(parsed.port) : parsed.protocol === "https:" ? 443 : 80;
  } catch {
    return null;
  }
}

function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return /** @type {any} */ (error).code === "EPERM";
  }
}

/**
 * The host OpenCode service as far as it can be seen from outside, or a
 * record with `running: false` when none is running.
 * @param {{env?: NodeJS.ProcessEnv, procRoot?: string}} [options]
 * @returns {{kind: "opencode", running: boolean, pid: number|null, url: string|null, port: number|null, listening: boolean|null, sandboxed: boolean|null, stateFile: string, configFile: string}}
 */
export function detectOpencodeService({ env = process.env, procRoot = "/proc" } = {}) {
  const { stateFile, configFile } = opencodeServiceFiles(env);
  const state = readJson(stateFile);
  const config = readJson(configFile);
  const url = typeof state?.url === "string" ? state.url : null;
  const pid = Number.isInteger(state?.pid) && state.pid > 0 ? state.pid : null;
  const port = (url ? portOf(url) : null) ?? (Number.isInteger(config?.port) ? config.port : null);
  const base = { kind: /** @type {"opencode"} */ ("opencode"), pid, url, port, stateFile, configFile };
  const running = pid !== null && alive(pid);
  if (!running) {
    return { ...base, running: false, listening: null, sandboxed: null };
  }
  const info = readProcess(pid, { root: procRoot });
  if (info !== null && info.argv.length > 0 && !info.argv.some((word) => /opencode/.test(word))) {
    // The recorded pid was recycled by an unrelated process: the service is gone.
    return { ...base, running: false, listening: null, sandboxed: null };
  }
  const sandboxed = info === null || info.noNewPrivs === null ? null : Boolean(info.noNewPrivs && info.nonoCapFile);
  let listening = null;
  if (port !== null) {
    const connections = [...tcpConnections({ root: procRoot }).values()];
    if (connections.length > 0) {
      listening = connections.some((connection) => connection.state === TCP_LISTEN && connection.local.port === port);
    }
  }
  return { ...base, running: listening !== false, listening, sandboxed };
}

/**
 * A warning about a host service a sandboxed agent could use to leave the
 * sandbox, or null when there is nothing to warn about.
 * @param {ReturnType<typeof detectOpencodeService>} service
 * @returns {string|null}
 */
export function hostServiceWarning(service) {
  if (!service.running || service.sandboxed === true) {
    return null;
  }
  const where = service.url ?? (service.port ? `port ${service.port}` : "an unknown port");
  return `An OpenCode background service (pid ${service.pid}, ${where}) is running outside any sandbox. `
    + `The opencode nono profile lets sandboxed processes read ${service.stateFile}, which holds its password, `
    + "so an agent could drive that service and run tools on the host. "
    + "Stop it with \"opencode service stop\" while sandboxed agents run, and do not start plain \"opencode\" on the host meanwhile (it restarts the service).";
}

const ESC = String.fromCharCode(27);

/**
 * The block the bridge prints in the pane when it refuses to start an agent
 * because of the host service: short lines (panes are narrow), the headline
 * in bold red, and the exact command that fixes it.
 * @param {ReturnType<typeof detectOpencodeService>} service
 * @param {{home?: string, color?: boolean}} [options]
 * @returns {string[]}
 */
export function hostServiceRefusal(service, { home = homedir(), color = true } = {}) {
  const where = service.url ?? (service.port ? `port ${service.port}` : "unknown port");
  const file = home && service.stateFile.startsWith(`${home}/`) ? `~${service.stateFile.slice(home.length)}` : service.stateFile;
  const headline = "nono: the agent was NOT started";
  const rule = "=".repeat(56);
  return [
    rule,
    color ? `${ESC}[1;31m${headline}${ESC}[0m` : headline,
    rule,
    "An OpenCode background service runs OUTSIDE any sandbox:",
    `  pid ${service.pid}, ${where}`,
    "A sandboxed agent can read its password from",
    `  ${file}`,
    "and use it to run commands on the host, unsandboxed.",
    "",
    "Stop the service, then reconnect (prefix, shift+b):",
    "  opencode service stop",
    "Plain \"opencode\" on the host starts it again.",
    "",
    "To start agents anyway, set \"hostServiceCheck\": \"warn\"",
    "in the plugin's config.json (not recommended).",
    rule,
  ];
}

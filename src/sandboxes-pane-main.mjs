/**
 * The sandboxes overlay: a live table of every mapping with its pane, nono
 * session and verification result, refreshed until `q` is pressed.
 * @module sandboxes-pane-main
 */
import { spawn } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { loadConfig } from "./config.mjs";
import { readPluginEnv, requirePluginDirs } from "./context.mjs";
import { PluginError, errorMessageOf } from "./errors.mjs";
import { classifyFailure, normalizeSessionList } from "./nono.mjs";
import { serverSessionName, shellSessionName } from "./naming.mjs";
import { loadState } from "./state.mjs";
import { summarizeVerification } from "./verify.mjs";

const ESC = String.fromCharCode(27);
const CLEAR_SCREEN = `${ESC}[2J${ESC}[H`;
const CTRL_C = String.fromCharCode(3);
const NONO_TIMEOUT_MS = 15_000;
const HERDR_TIMEOUT_MS = 10_000;

/**
 * Runs a CLI without blocking the event loop, so a keypress can cancel it.
 * The child is killed on abort or after `timeoutMs`.
 * @param {string} bin
 * @param {string[]} args
 * @param {{env?: NodeJS.ProcessEnv, signal?: AbortSignal|null, timeoutMs?: number}} [options]
 * @returns {Promise<{status: number|null, stdout: string, stderr: string, output: string}>}
 */
export function runCli(bin, args, { env = process.env, signal = null, timeoutMs = NONO_TIMEOUT_MS } = {}) {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(new PluginError("cancelled", "cancelled before start"));
      return;
    }
    const child = spawn(bin, args, { env, stdio: ["ignore", "pipe", "pipe"] });
    // Decoded as streams, so a multi-byte character split across two chunks is not garbled.
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    let stdout = "";
    let stderr = "";
    let timedOut = false;
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    // Killing the child is not enough when it spawned grandchildren that keep
    // the pipes open; destroying our ends makes "close" fire right away.
    const terminate = () => {
      child.kill("SIGKILL");
      child.stdout.destroy();
      child.stderr.destroy();
    };
    const timer = setTimeout(() => {
      timedOut = true;
      terminate();
    }, timeoutMs);
    const onAbort = () => terminate();
    signal?.addEventListener("abort", onAbort, { once: true });
    const done = () => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
    };
    child.on("error", (error) => {
      done();
      reject(new PluginError(/** @type {NodeJS.ErrnoException} */ (error).code === "ENOENT" ? "startup" : "unknown", `Could not run ${bin}: ${error.message}`, { cause: error }));
    });
    child.on("close", (status) => {
      done();
      if (signal?.aborted) {
        reject(new PluginError("cancelled", `${bin} ${args[0]} cancelled`));
      } else if (timedOut) {
        reject(new PluginError("network", `${bin} ${args.join(" ")} did not answer within ${Math.round(timeoutMs / 1000)}s`, { output: stdout + stderr }));
      } else {
        resolve({ status, stdout, stderr, output: stdout + stderr });
      }
    });
  });
}

/**
 * Non-blocking clients for the overlay. They mirror the parsing of the
 * synchronous clients but never stall the terminal.
 * @param {{nonoBin: string, herdrBin: string, env?: NodeJS.ProcessEnv}} input
 */
export function createOverlayClients({ nonoBin, herdrBin, env = process.env }) {
  const parseJson = (text, what) => {
    try {
      return text.trim() === "" ? null : JSON.parse(text);
    } catch (error) {
      throw new PluginError("unknown", `${what} printed invalid JSON: ${error.message}`, { cause: error });
    }
  };
  return {
    nono: {
      async listSessions(signal) {
        const result = await runCli(nonoBin, ["ps", "--json"], { env, signal });
        if (result.status !== 0) {
          throw new PluginError(classifyFailure(result.output), `nono ps failed (exit ${result.status})`, { output: result.output });
        }
        return normalizeSessionList(parseJson(result.stdout, "nono ps --json"));
      },
    },
    herdr: {
      async listPaneIds(signal) {
        const result = await runCli(herdrBin, ["pane", "list"], { env, signal, timeoutMs: HERDR_TIMEOUT_MS });
        if (result.status !== 0) {
          throw new PluginError("unknown", `herdr pane list failed (exit ${result.status})`, { output: result.output });
        }
        const parsed = parseJson(result.stdout, "herdr pane list");
        const panes = parsed?.result?.panes ?? parsed?.panes;
        if (!Array.isArray(panes)) {
          throw new PluginError("unknown", "herdr pane list did not return a pane list.", { output: result.stdout });
        }
        return panes.map((pane) => pane?.pane_id).filter((id) => typeof id === "string" && id !== "");
      },
    },
  };
}

/**
 * Gathers one row per mapping, merging live nono and Herdr information: one
 * `nono ps` and one `herdr pane list` per frame, however many mappings exist.
 * @param {{stateDir: string, nono: {listSessions: (signal?: AbortSignal|null) => Promise<Array<Record<string, any>>>}, herdr: {listPaneIds: (signal?: AbortSignal|null) => Promise<string[]>}, signal?: AbortSignal|null}} input
 */
export async function collectSandboxes({ stateDir, nono, herdr, signal = null }) {
  const state = loadState(stateDir);
  let sessions = null;
  let sessionError = null;
  try {
    sessions = (await nono.listSessions(signal)).filter((session) => session.status !== "exited");
  } catch (error) {
    sessionError = errorMessageOf(error);
  }
  let paneIds = null;
  let paneError = null;
  try {
    paneIds = new Set(await herdr.listPaneIds(signal));
  } catch (error) {
    paneError = errorMessageOf(error);
  }
  const rows = Object.values(state.panes).map((entry) => {
    const agentSession = sessions?.find((session) => (entry.supervisorPid && session.supervisorPid === entry.supervisorPid) || session.name === entry.sessionName) ?? null;

    const shells = sessions ? sessions.filter((session) => session.name === shellSessionName(entry.sessionName)).length : null;
    const serverRunning = sessions ? sessions.some((session) => session.name === serverSessionName(entry.sessionName)) : null;
    return {
      paneId: entry.paneId,
      paneExists: paneIds ? paneIds.has(entry.paneId) : null,
      paneError,
      sessionName: entry.sessionName,
      agentKind: entry.agentKind,
      lifecycleState: entry.lifecycleState,
      running: sessions ? agentSession !== null : null,
      serverRunning,
      sessionId: agentSession?.sessionId ?? null,
      shells,
      verified: entry.verification?.ok ?? null,
      verification: entry.verification ? summarizeVerification(entry.verification) : null,
      localPath: entry.localPath,
    };
  });
  return { rows, sessionError };
}

function pad(text, width) {
  const value = String(text ?? "");
  return value.length >= width ? value : value + " ".repeat(width - value.length);
}

/**
 * Renders the overlay text.
 * @param {{rows: any[], sessionError: string|null}} data
 * @param {{at?: Date, intervalMs?: number}} [options]
 * @returns {string[]}
 */
export function renderSandboxes({ rows, sessionError }, { at = new Date(), intervalMs = 3000 } = {}) {
  const lines = [`nono sandboxes  ${rows.length} mapping${rows.length === 1 ? "" : "s"}  ${at.toLocaleTimeString()}  (q closes, refreshes every ${Math.round(intervalMs / 1000)}s)`, ""];
  if (rows.length === 0) {
    lines.push("No sandboxed agents are mapped. Run start-agent from a workspace.");
  } else {
    const widths = { pane: 14, session: 30, agent: 10, state: 11, session2: 16, verified: 10 };
    lines.push(`${pad("PANE", widths.pane)} ${pad("SESSION", widths.session)} ${pad("AGENT", widths.agent)} ${pad("STATE", widths.state)} ${pad("NONO", widths.session2)} ${pad("VERIFIED", widths.verified)} DIRECTORY`);
    for (const row of rows) {
      const pane = row.paneExists === false ? `${row.paneId} (gone)` : row.paneError ? `${row.paneId} (?)` : row.paneId;
      const extras = `${row.serverRunning ? "+srv" : ""}${row.shells ? ` +${row.shells}sh` : ""}`;
      const nono = row.running === null ? "unknown" : row.running ? `running${extras}` : row.serverRunning ? `server only${row.shells ? ` +${row.shells}sh` : ""}` : row.shells ? `${row.shells} shell${row.shells === 1 ? "" : "s"}` : "-";
      const verified = row.verified === null ? "-" : row.verified ? "ok" : "FAILED";
      lines.push(`${pad(pane, widths.pane)} ${pad(row.sessionName, widths.session)} ${pad(row.agentKind, widths.agent)} ${pad(row.lifecycleState, widths.state)} ${pad(nono, widths.session2)} ${pad(verified, widths.verified)} ${row.localPath}`);
    }
    const failed = rows.filter((row) => row.verified === false);
    for (const row of failed) {
      lines.push("", `${row.sessionName}: ${row.verification}`);
    }
    const paneErrors = [...new Set(rows.filter((row) => row.paneError).map((row) => row.paneError))];
    if (paneErrors.length > 0) {
      lines.push("", `pane check failed: ${paneErrors.join("; ")}`);
    }
    const stale = rows.filter((row) => row.paneExists === false && row.running === false).length;
    if (stale > 0) {
      lines.push("", `${stale} stale mapping${stale === 1 ? "" : "s"} (pane gone, nothing running): run prune-mappings to drop them`);
    }
  }
  if (sessionError) {
    lines.push("", `nono ps failed: ${sessionError}`);
  }
  return lines;
}

/**
 * Keeps an error on screen until a key is pressed or the time is up, because an
 * overlay closes together with its process.
 * @param {any} input
 * @param {any} output
 * @param {number} holdMs
 */
export async function holdForKey(input, output, holdMs) {
  output.write(`press any key to close (closes by itself in ${Math.round(holdMs / 1000)}s)\n`);
  const controller = new AbortController();
  const onKey = () => controller.abort();
  let rawCapable = false;
  try {
    if (typeof input.setRawMode === "function" && input.isTTY) {
      input.setRawMode(true);
      rawCapable = true;
    }
  } catch {
    rawCapable = false;
  }
  input.on("data", onKey);
  if (typeof input.resume === "function") {
    input.resume();
  }
  try {
    await sleep(holdMs, null, { signal: controller.signal });
  } catch {
    // A key was pressed.
  } finally {
    input.off("data", onKey);
    if (rawCapable) {
      try {
        input.setRawMode(false);
      } catch {
        // The terminal is gone.
      }
    }
    if (typeof input.pause === "function") {
      input.pause();
    }
  }
}

/**
 * Runs the overlay. With `once` it renders a single frame and returns.
 * @param {NodeJS.ProcessEnv} [env]
 * @param {{once?: boolean, intervalMs?: number, output?: NodeJS.WritableStream & {isTTY?: boolean}, input?: NodeJS.ReadableStream & {isTTY?: boolean, setRawMode?: (mode: boolean) => unknown}, holdMs?: number}} [options] `holdMs` is how long a startup error stays on screen.
 * @returns {Promise<number>}
 */
export async function runSandboxesPane(env = process.env, { once = false, intervalMs = 3000, output = process.stdout, input = process.stdin, holdMs = 15_000 } = {}) {
  let pluginEnv;
  let config;
  try {
    pluginEnv = requirePluginDirs(readPluginEnv(env));
    config = loadConfig(pluginEnv.configDir, env);
  } catch (error) {
    output.write(`${errorMessageOf(error)}\n`);
    if (!once) {
      await holdForKey(input, output, holdMs);
    }
    return 1;
  }
  const { nono, herdr } = createOverlayClients({ nonoBin: config.nonoBin, herdrBin: pluginEnv.herdrBin, env });
  const controller = new AbortController();
  const draw = async () => {
    let lines;
    try {
      lines = renderSandboxes(await collectSandboxes({ stateDir: pluginEnv.stateDir, nono, herdr, signal: controller.signal }), { intervalMs });
    } catch (error) {
      lines = [`could not read the sandboxes: ${errorMessageOf(error)}`];
    }
    if (!controller.signal.aborted) {
      output.write(`${once ? "" : CLEAR_SCREEN}${lines.join("\n")}\n`);
    }
  };
  if (once) {
    await draw();
    return 0;
  }
  const onKey = (chunk) => {
    const text = String(chunk);
    if (text === "q" || text === "Q" || text === CTRL_C) {
      controller.abort();
    }
  };
  let rawCapable = false;
  try {
    if (typeof input.setRawMode === "function" && input.isTTY) {
      input.setRawMode(true);
      rawCapable = true;
    }
  } catch (error) {
    output.write(`raw input unavailable (${errorMessageOf(error)}); press Ctrl-C or close the pane to leave\n`);
  }
  input.on("data", onKey);
  if (typeof input.resume === "function") {
    input.resume();
  }
  try {
    while (!controller.signal.aborted) {
      await draw();
      if (controller.signal.aborted) {
        break;
      }
      try {
        await sleep(intervalMs, null, { signal: controller.signal });
      } catch {
        break;
      }
    }
  } finally {
    input.off("data", onKey);
    if (rawCapable) {
      try {
        input.setRawMode(false);
      } catch {
        // The terminal is gone; nothing to restore.
      }
    }
    if (typeof input.pause === "function") {
      input.pause();
    }
  }
  return 0;
}

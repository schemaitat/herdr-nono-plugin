/**
 * The sandboxes overlay: an interactive terminal UI listing every mapping
 * with its pane, nono sandboxes and verification, a details panel for the
 * selected one, and keys to verify, stop and prune. Refreshes until `q`.
 * @module sandboxes-pane-main
 */
import { spawn } from "node:child_process";
import { homedir } from "node:os";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { loadConfig } from "./config.mjs";
import { readPluginEnv, requirePluginDirs } from "./context.mjs";
import { PluginError, errorMessageOf } from "./errors.mjs";
import { createHerdrClient } from "./herdr.mjs";
import { createLifecycle } from "./lifecycle.mjs";
import { classifyFailure, createNonoClient, normalizeSessionList, summarizeProfile } from "./nono.mjs";
import { descendants, procfsAvailable } from "./procfs.mjs";
import { serverSessionName, shellSessionName } from "./naming.mjs";
import { loadState } from "./state.mjs";
import { looksConfined, summarizeVerification } from "./verify.mjs";

const ESC = String.fromCharCode(27);
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
      async showProfile(ref, signal) {
        const result = await runCli(nonoBin, ["profile", "show", "--json", ref], { env, signal });
        if (result.status !== 0) {
          throw new PluginError(classifyFailure(result.output), `nono profile show ${ref} failed (exit ${result.status})`, { output: result.output });
        }
        return parseJson(result.stdout, "nono profile show --json");
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
 * The processes running under one nono supervisor, in tree order, with their
 * depth below the supervisor and whether they carry nono's confinement marks.
 * @param {number|null} supervisorPid
 * @param {string} [procRoot]
 * @returns {Array<{pid: number, depth: number, confined: boolean, argv: string[]}>}
 */
export function sandboxTree(supervisorPid, procRoot = "/proc") {
  if (!supervisorPid || !procfsAvailable(procRoot)) {
    return [];
  }
  const list = descendants(supervisorPid, { root: procRoot });
  const depth = new Map([[supervisorPid, -1]]);
  const children = new Map();
  for (const info of list) {
    depth.set(info.pid, (depth.get(/** @type {number} */ (info.ppid)) ?? -1) + 1);
    const siblings = children.get(info.ppid) ?? [];
    siblings.push(info);
    children.set(info.ppid, siblings);
  }
  // Depth-first, so each process sits right under its parent.
  const ordered = [];
  const walk = (pid) => {
    for (const info of children.get(pid) ?? []) {
      ordered.push({ pid: info.pid, depth: depth.get(info.pid) ?? 0, confined: looksConfined(info), argv: info.argv });
      walk(info.pid);
    }
  };
  walk(supervisorPid);
  return ordered;
}

/**
 * Gathers one row per mapping, merging live nono and Herdr information: one
 * `nono ps` and one `herdr pane list` per frame, however many mappings exist.
 * Running agents also get the process trees of their client and server
 * sandboxes and a summary of each sandbox's profile (cached by reference).
 * @param {{stateDir: string, nono: {listSessions: (signal?: AbortSignal|null) => Promise<Array<Record<string, any>>>, showProfile?: (ref: string, signal?: AbortSignal|null) => Promise<Record<string, any>>}, herdr: {listPaneIds: (signal?: AbortSignal|null) => Promise<string[]>}, signal?: AbortSignal|null, procRoot?: string, profileCache?: Map<string, any>}} input
 */
export async function collectSandboxes({ stateDir, nono, herdr, signal = null, procRoot = "/proc", profileCache = new Map() }) {
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
    const serverSession = sessions?.find((session) => session.name === serverSessionName(entry.sessionName)) ?? null;
    const serverRunning = sessions ? serverSession !== null : null;
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
      trees: {
        client: sandboxTree(agentSession?.supervisorPid ?? null, procRoot),
        server: sandboxTree(serverSession?.supervisorPid ?? null, procRoot),
      },
      network: { client: null, server: null },
      entry,
    };
  });
  if (typeof nono.showProfile === "function") {
    const summary = async (ref) => {
      if (!ref) return null;
      if (!profileCache.has(ref)) {
        try {
          profileCache.set(ref, summarizeProfile(await nono.showProfile(ref, signal)));
        } catch {
          profileCache.set(ref, null);
        }
      }
      return profileCache.get(ref);
    };
    for (const row of rows) {
      row.network = { client: await summary(row.entry.profile), server: await summary(row.entry.serverProfile) };
    }
  }
  return { rows, sessionError, paneIds: paneIds ? [...paneIds] : null };
}

const SGR = { reset: 0, bold: 1, dim: 2, inverse: 7, red: 31, green: 32, yellow: 33, blue: 34, magenta: 35, cyan: 36, gray: 90 };

/**
 * Wraps text in SGR codes when colour is on.
 * @param {boolean} color
 * @param {string} text
 * @param {Array<keyof typeof SGR>} styles
 */
function paint(color, text, ...styles) {
  if (!color || styles.length === 0 || text === "") return text;
  return `${ESC}[${styles.map((style) => SGR[style]).join(";")}m${text}${ESC}[0m`;
}

/**
 * Cuts or pads plain text to exactly `width` characters; long text ends in `…`
 * (or starts with it, for paths, with `fromLeft`).
 * @param {unknown} value
 * @param {number} width
 * @param {{fromLeft?: boolean}} [options]
 * @returns {string}
 */
export function fit(value, width, { fromLeft = false } = {}) {
  const text = String(value ?? "");
  if (width <= 0) return "";
  if (text.length > width) {
    return width === 1 ? "…" : fromLeft ? `…${text.slice(text.length - width + 1)}` : `${text.slice(0, width - 1)}…`;
  }
  return text + " ".repeat(width - text.length);
}

/**
 * A path with the home directory shortened to `~`.
 * @param {string|null|undefined} value
 * @param {string} [home]
 */
function shortPath(value, home = homedir()) {
  const text = String(value ?? "");
  return home && (text === home || text.startsWith(`${home}/`)) ? `~${text.slice(home.length)}` : text;
}

function clock(iso) {
  if (!iso) return "-";
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? "-" : date.toLocaleTimeString();
}

/**
 * What the NONO column says about a row's live sessions.
 * @param {Record<string, any>} row
 * @returns {{text: string, style: Array<keyof typeof SGR>}}
 */
function sessionCell(row) {
  if (row.running === null) return { text: "unknown", style: ["gray"] };
  const parts = [];
  if (row.running) parts.push("client");
  if (row.serverRunning) parts.push("server");
  if (row.shells) parts.push(`${row.shells} sh`);
  if (parts.length === 0) return { text: "-", style: ["gray"] };
  return { text: parts.join("+"), style: row.running ? ["green"] : ["yellow"] };
}

/**
 * Whether prune would drop a row: its pane is gone and nothing of it runs.
 * @param {Record<string, any>} row
 */
export function isStale(row) {
  return row.paneExists === false && row.running === false && !row.serverRunning && !row.shells;
}

/**
 * The short form of a process command line for the sandbox boxes: the
 * executable's basename and its arguments.
 * @param {string[]} argv
 * @returns {string}
 */
function shortCommand(argv) {
  if (argv.length === 0) return "?";
  // Interpreters run scripts: show the script, not the interpreter.
  const [first, ...rest] = /^(node|python3?|bun)$/.test(path.basename(argv[0])) && argv[1] && !argv[1].startsWith("-") ? argv.slice(1) : argv;
  return [path.basename(first), ...rest].join(" ");
}

/**
 * What a profile summary says about a sandbox's network, in a few words.
 * @param {{egress: string, allowDomains: string[]}|null} summary
 * @param {"client"|"server"} side
 * @param {number|null} port
 */
function networkLabel(summary, side, port) {
  const portText = port ? `:${port}` : "its port";
  if (!summary) return side === "client" ? `to ${portText}` : `on ${portText}`;
  if (summary.egress === "blocked") return side === "client" ? `no network but ${portText}` : `no network, on ${portText}`;
  if (summary.egress === "allowlist") {
    const domains = summary.allowDomains.includes("*") ? "" : ` ${summary.allowDomains.length} host${summary.allowDomains.length === 1 ? "" : "s"}`;
    return `${side === "server" ? `on ${portText}, ` : ""}proxy egress${domains}`;
  }
  return `${side === "server" ? `on ${portText}, ` : ""}OPEN egress + localhost`;
}

/**
 * Renders the whole screen as `height` lines of exactly `width` visible
 * characters. Pure: everything it shows comes from `view`.
 * @param {{rows: any[], sessionError: string|null, selected?: number, status?: {text: string, kind?: "ok"|"error"|"info"}|null, prompt?: string|null}} view
 * @param {{width?: number, height?: number, color?: boolean, at?: Date, intervalMs?: number, home?: string}} [options]
 * @returns {string[]}
 */
export function renderTui(view, { width = 100, height = 30, color = true, at = new Date(), intervalMs = 3000, home = homedir() } = {}) {
  width = Math.max(width, 70);
  height = Math.max(height, 20);
  const { rows } = view;
  const selected = rows.length === 0 ? -1 : Math.min(Math.max(view.selected ?? 0, 0), rows.length - 1);
  const row = rows[selected];
  const lines = [];
  const box = (boxWidth, title, borderStyle = ["gray"]) => {
    const innerWidth = boxWidth - 4;
    return {
      innerWidth,
      top: () => `${paint(color, "┌─", ...borderStyle)}${paint(color, ` ${title.text} `, "bold", ...(title.style ?? []))}${paint(color, `${"─".repeat(Math.max(0, boxWidth - title.text.length - 5))}┐`, ...borderStyle)}`,
      row: (content) => `${paint(color, "│", ...borderStyle)} ${content} ${paint(color, "│", ...borderStyle)}`,
      bottom: () => paint(color, `└${"─".repeat(boxWidth - 2)}┘`, ...borderStyle),
    };
  };

  // Title bar.
  const running = rows.filter((item) => item.running).length;
  const failed = rows.filter((item) => item.verified === false).length;
  const stale = rows.filter(isStale).length;
  const right = `${rows.length} agent${rows.length === 1 ? "" : "s"} · ${running} running${failed ? ` · ${failed} FAILED` : ""}${stale ? ` · ${stale} stale` : ""} · ${at.toLocaleTimeString()} `;
  const left = " nono sandboxes ";
  lines.push(paint(color, `${left}${" ".repeat(Math.max(1, width - left.length - right.length))}${right}`.slice(0, width), "inverse", "bold"));

  // Space: the table gets up to six rows, the sandbox boxes what is left after
  // the details panel; on a short screen the details panel goes first.
  const footerLines = 2;
  const tableRows = Math.min(Math.max(rows.length, 1), 6);
  const tableHeight = tableRows + 3;
  let detailHeight = row ? 9 : 0;
  let sandboxHeight = height - 1 - tableHeight - footerLines - detailHeight - 1;
  if (row && sandboxHeight < 5) {
    detailHeight = 0;
    sandboxHeight = height - 1 - tableHeight - footerLines - 1;
  }

  // Agents table.
  const table = box(width, { text: "Agents" });
  const inner = table.innerWidth;
  lines.push(table.top());
  const widths = { mark: 3, pane: 10, session: 14, agent: 9, state: 8, nono: 19, verified: 9 };
  const fixed = Object.values(widths).reduce((sum, value) => sum + value, 0) + 7;
  const dirWidth = Math.max(8, inner - fixed);
  const headerText = `${fit("", widths.mark)} ${fit("PANE", widths.pane)} ${fit("SESSION", widths.session)} ${fit("AGENT", widths.agent)} ${fit("STATE", widths.state)} ${fit("NONO", widths.nono)} ${fit("VERIFIED", widths.verified)} ${fit("DIRECTORY", dirWidth)}`;
  lines.push(table.row(paint(color, fit(headerText, inner), "bold", "gray")));
  if (rows.length === 0) {
    lines.push(table.row(fit("No sandboxed agents. Press ctrl+b, shift+a in a project pane to start one.", inner)));
  } else {
    const first = Math.min(Math.max(0, selected - tableRows + 1), Math.max(0, rows.length - tableRows));
    for (let index = first; index < first + tableRows; index += 1) {
      const item = rows[index];
      if (!item) {
        lines.push(table.row(fit("", inner)));
        continue;
      }
      const isSelected = index === selected;
      const dot = item.running ? "●" : item.lifecycleState === "failed" ? "✖" : "○";
      const dotStyle = item.running ? ["green"] : item.lifecycleState === "failed" ? ["red"] : ["gray"];
      const pane = item.paneExists === false ? `${item.paneId} ✗` : item.paneId;
      const sessions = sessionCell(item);
      const verified = item.verified === null ? { text: "-", style: ["gray"] } : item.verified ? { text: "✔ ok", style: ["green"] } : { text: "✖ FAILED", style: ["red", "bold"] };
      const stateStyle = item.lifecycleState === "running" ? ["green"] : item.lifecycleState === "failed" ? ["red"] : item.lifecycleState === "exited" ? ["yellow"] : [];
      const cells = [
        [fit(`${isSelected ? "▶" : " "}${dot}`, widths.mark), isSelected ? ["cyan", "bold"] : dotStyle],
        [fit(pane, widths.pane), item.paneExists === false ? ["gray"] : []],
        [fit(`…${String(item.sessionName).split("-").pop()}`, widths.session), []],
        [fit(item.agentKind, widths.agent), []],
        [fit(item.lifecycleState, widths.state), stateStyle],
        [fit(sessions.text, widths.nono), sessions.style],
        [fit(verified.text, widths.verified), verified.style],
        [fit(shortPath(item.localPath, home), dirWidth, { fromLeft: true }), ["gray"]],
      ];
      lines.push(table.row(isSelected
        ? paint(color, cells.map(([cell]) => cell).join(" "), "inverse")
        : cells.map(([cell, style]) => paint(color, cell, ...style)).join(" ")));
    }
  }
  lines.push(table.bottom());

  // The selected agent's two sandboxes, side by side, with their processes.
  if (row) {
    const entry = row.entry ?? {};
    const port = entry.port ?? null;
    const hasServer = Boolean(entry.serverProfile) || row.trees?.server?.length > 0;
    const toolsEgress = (hasServer ? row.network?.server : row.network?.client)?.egress ?? null;
    const blocked = toolsEgress === "open" ? [["  ✖ Herdr ✖ systemd ✖ ssh-agent ! localhost open", "red"]] : [["  ✖ localhost ✖ Herdr ✖ systemd ✖ ssh-agent", "red"]];
    const flow = hasServer
      ? [["client", "cyan"], [` ──${port ? `:${port}` : ""}──▶ `, "gray"], ["server", "magenta"], [" ──▶ ", "gray"], [toolsEgress === "open" ? "direct" : "nono proxy", "blue"], [" ──▶ internet", "gray"], ...blocked]
      : [["sandbox", "cyan"], [" ──▶ ", "gray"], [toolsEgress ?? "network", "gray"], ...blocked];
    const flowPlain = ` ${flow.map(([text]) => text).join("")}`;
    lines.push(flowPlain.length <= width
      ? ` ${flow.map(([text, style]) => paint(color, text, ...(style === "red" ? ["red"] : [style, "bold"]))).join("")}${" ".repeat(width - flowPlain.length)}`
      : paint(color, fit(flowPlain, width), "gray"));
    const leftWidth = hasServer ? Math.floor(width / 2) : width;
    const rightWidth = width - leftWidth;
    const processLines = (tree, innerWidth, side) => {
      if (!tree || tree.length === 0) {
        return [paint(color, fit(side === "client" ? (row.running ? "starting…" : "not running") : (row.serverRunning ? "starting…" : "not running"), innerWidth), "gray")];
      }
      return tree.map((proc) => {
        const isServer = side === "server" && proc.depth === 0;
        const role = side === "client" ? (proc.depth === 0 ? "tui" : "child") : isServer ? "server" : "tool";
        const mark = proc.confined ? "✔" : "✖";
        const prefix = `${mark} ${String(proc.pid).padStart(7)} ${fit(role, 6)} `;
        const indent = proc.depth > 0 ? `${"  ".repeat(proc.depth - 1)}└ ` : "";
        const command = fit(`${indent}${shortCommand(proc.argv)}`, innerWidth - prefix.length);
        const roleStyle = role === "tui" ? ["cyan", "bold"] : role === "server" ? ["magenta", "bold"] : role === "tool" ? ["yellow"] : [];
        return `${paint(color, mark, proc.confined ? "green" : "red")} ${paint(color, String(proc.pid).padStart(7), "gray")} ${paint(color, fit(role, 6), ...roleStyle)} ${paint(color, command, ...(proc.confined ? [] : ["red"]))}`;
      });
    };
    const leftLabel = hasServer || row.network?.client ? networkLabel(row.network?.client ?? null, "client", port) : "network unknown";
    const left = box(leftWidth, { text: fit(`${hasServer ? "client" : "sandbox"} · ${leftLabel}`, leftWidth - 6).trimEnd(), style: ["cyan"] }, ["cyan"]);
    const right = hasServer ? box(rightWidth, { text: fit(`server · ${networkLabel(row.network?.server ?? null, "server", port)}`, rightWidth - 6).trimEnd(), style: ["magenta"] }, ["magenta"]) : null;
    const leftBody = processLines(row.trees?.client, left.innerWidth, "client");
    const rightBody = right ? processLines(row.trees?.server, right.innerWidth, "server") : [];
    // Only as tall as the longer process list needs; the rest of the screen stays free.
    const bodyHeight = Math.max(1, Math.min(sandboxHeight - 2, Math.max(2, leftBody.length, rightBody.length)));
    const clip = (body, innerWidth) => {
      if (body.length <= bodyHeight) return body;
      return [...body.slice(0, bodyHeight - 1), paint(color, fit(`… ${body.length - bodyHeight + 1} more`, innerWidth), "gray")];
    };
    const leftLines = clip(leftBody, left.innerWidth);
    const rightLines = right ? clip(rightBody, right.innerWidth) : [];
    lines.push(`${left.top()}${right ? right.top() : ""}`);
    for (let index = 0; index < bodyHeight; index += 1) {
      lines.push(`${left.row(leftLines[index] ?? fit("", left.innerWidth))}${right ? right.row(rightLines[index] ?? fit("", right.innerWidth)) : ""}`);
    }
    lines.push(`${left.bottom()}${right ? right.bottom() : ""}`);
  }

  // Details of the selected mapping.
  if (row && detailHeight > 0) {
    const details = box(width, { text: "Details" });
    lines.push(details.top());
    const entry = row.entry ?? {};
    const out = [];
    const field = (label, value, ...style) => out.push(`${paint(color, fit(label, 12), "gray")}${paint(color, fit(value, inner - 12), ...style)}`);
    field("Session", row.sessionName, "bold");
    field("Pane", `${row.paneId} ${row.paneExists === false ? "(gone)" : row.paneExists === null ? "(?)" : "(open)"}${entry.workspaceId ? `  workspace ${entry.workspaceId}` : ""}`);
    field("Directory", `${shortPath(row.localPath, home)}${entry.workdir && entry.workdir !== row.localPath ? `  (starts in ${shortPath(entry.workdir, home)})` : ""}`);
    const profileName = (ref) => (ref ? path.basename(String(ref)).replace(/\.json$/, "") : "-");
    field("Profiles", `client ${profileName(entry.profile)}${entry.serverProfile ? ` · server ${profileName(entry.serverProfile)}` : ""}`);
    const report = entry.verification;
    if (report) {
      field("Verified", `${row.verification} (${clock(report.checkedAt)})`, report.ok ? "green" : "red");
    } else {
      field("Verified", "not yet", "gray");
    }
    field("Last launch", `${clock(entry.lastLaunchAt)}${entry.lastExitCode !== undefined && entry.lastExitCode !== null ? ` · exit ${entry.lastExitCode}` : ""} · ${entry.launchCount ?? 0} launch${entry.launchCount === 1 ? "" : "es"}`);
    if (entry.lastError) {
      field("Last error", `${entry.lastError.kind}: ${entry.lastError.message}`, "red");
    } else if (report && report.ok === false) {
      field("Problem", report.problems?.[1] ?? report.problems?.[0] ?? "", "red");
    }
    for (let index = 0; index < detailHeight - 2; index += 1) {
      lines.push(details.row(out[index] ?? fit("", inner)));
    }
    lines.push(details.bottom());
  }
  if (view.sessionError) {
    lines.push(paint(color, fit(` nono ps failed: ${view.sessionError}`, width), "red"));
  }

  // Status line and key hints.
  while (lines.length < height - footerLines) lines.push(fit("", width));
  lines.length = Math.min(lines.length, height - footerLines);
  if (view.prompt) {
    lines.push(paint(color, fit(` ? ${view.prompt}`, width), "yellow", "bold"));
  } else if (view.status) {
    const style = view.status.kind === "error" ? ["red"] : view.status.kind === "ok" ? ["green"] : ["cyan"];
    lines.push(paint(color, fit(` ${view.status.kind === "error" ? "✖" : view.status.kind === "ok" ? "✔" : "…"} ${view.status.text}`, width), ...style));
  } else {
    lines.push(paint(color, fit(` refreshes every ${Math.round(intervalMs / 1000)}s`, width), "gray"));
  }
  const keys = view.prompt
    ? [["y", "confirm"], ["n/esc", "cancel"]]
    : [["↑↓/jk", "select"], ["v", "verify"], ["x", "stop"], ["p", "prune"], ["r", "refresh"], ["q", "quit"]];
  const hint = keys.map(([key, label]) => `${paint(color, ` ${key} `, "inverse")} ${label}`).join("  ");
  const hintPlain = keys.map(([key, label]) => ` ${key}  ${label}`).join("  ");
  lines.push(`${hint}${" ".repeat(Math.max(0, width - hintPlain.length - 1))}`);
  return lines;
}

/**
 * The plain renderer kept for `--once` and scripts: the same screen, no colour.
 * @param {{rows: any[], sessionError: string|null}} data
 * @param {{at?: Date, intervalMs?: number, width?: number, height?: number}} [options]
 * @returns {string[]}
 */
export function renderSandboxes(data, { at = new Date(), intervalMs = 3000, width = 110, height = 32 } = {}) {
  return renderTui({ ...data, selected: 0 }, { width, height, color: false, at, intervalMs });
}

/**
 * Translates raw terminal input into key names.
 * @param {string} chunk
 * @returns {string[]}
 */
export function parseKeys(chunk) {
  const keys = [];
  for (let index = 0; index < chunk.length; index += 1) {
    const rest = chunk.slice(index);
    if (rest.startsWith(`${ESC}[A`) || rest.startsWith(`${ESC}OA`)) { keys.push("up"); index += 2; continue; }
    if (rest.startsWith(`${ESC}[B`) || rest.startsWith(`${ESC}OB`)) { keys.push("down"); index += 2; continue; }
    if (rest.startsWith(`${ESC}[5~`)) { keys.push("pageup"); index += 3; continue; }
    if (rest.startsWith(`${ESC}[6~`)) { keys.push("pagedown"); index += 3; continue; }
    if (rest.startsWith(`${ESC}[`)) {
      // Another escape sequence (focus events, other arrows): skip it whole.
      const end = rest.slice(2).search(/[@-~]/);
      index += end === -1 ? rest.length : end + 2;
      continue;
    }
    const char = chunk[index];
    if (char === ESC) keys.push("escape");
    else if (char === CTRL_C) keys.push("ctrl-c");
    else if (char === "\r" || char === "\n") keys.push("enter");
    else keys.push(char.toLowerCase());
  }
  return keys;
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
 * Runs the overlay. With `once` it renders a single plain frame and returns.
 * @param {NodeJS.ProcessEnv} [env]
 * @param {{once?: boolean, intervalMs?: number, output?: NodeJS.WritableStream & {isTTY?: boolean, columns?: number, rows?: number}, input?: NodeJS.ReadableStream & {isTTY?: boolean, setRawMode?: (mode: boolean) => unknown}, holdMs?: number}} [options] `holdMs` is how long a startup error stays on screen.
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
  /** @type {{rows: any[], sessionError: string|null, paneIds: string[]|null, selected: number, status: {text: string, kind?: "ok"|"error"|"info"}|null, prompt: string|null}} */
  const view = { rows: [], sessionError: null, paneIds: null, selected: 0, status: null, prompt: null };
  const profileCache = new Map();
  const refresh = async () => {
    const keep = view.rows[view.selected]?.paneId ?? null;
    try {
      Object.assign(view, await collectSandboxes({ stateDir: pluginEnv.stateDir, nono, herdr, signal: controller.signal, profileCache }));
    } catch (error) {
      view.status = { text: `could not read the sandboxes: ${errorMessageOf(error)}`, kind: "error" };
    }
    const again = keep ? view.rows.findIndex((row) => row.paneId === keep) : -1;
    view.selected = again !== -1 ? again : Math.min(view.selected, Math.max(0, view.rows.length - 1));
  };
  if (once) {
    await refresh();
    output.write(`${renderSandboxes(view, { intervalMs }).join("\n")}\n`);
    return 0;
  }

  const color = Boolean(output.isTTY);
  const draw = () => {
    if (controller.signal.aborted) return;
    const lines = renderTui(view, { width: (output.columns ?? 100) - 1, height: output.rows ?? 30, color, intervalMs });
    output.write(`${ESC}[H${lines.map((line) => `${line}${ESC}[K`).join("\r\n")}${ESC}[J`);
  };
  // The actions run in this process: the overlay is a Herdr plugin pane with the plugin's environment.
  const lifecycle = createLifecycle({
    stateDir: pluginEnv.stateDir,
    config,
    pluginRoot: pluginEnv.pluginRoot,
    nono: createNonoClient({ bin: config.nonoBin, env }),
    herdr: createHerdrClient({ bin: pluginEnv.herdrBin, env }),
    log: () => {},
    env,
  });
  /** @type {null|(() => Promise<void>)} */
  let pending = null;
  let busy = false;
  const run = async (label, task) => {
    busy = true;
    view.status = { text: label, kind: "info" };
    draw();
    try {
      view.status = await task();
    } catch (error) {
      view.status = { text: errorMessageOf(error), kind: "error" };
    } finally {
      busy = false;
    }
    await refresh();
    draw();
  };
  const ask = (question, task) => {
    view.prompt = `${question} [y/N]`;
    pending = task;
    draw();
  };
  const onKey = async (key) => {
    if (key === "q" || key === "ctrl-c") {
      controller.abort();
      return;
    }
    if (view.prompt) {
      const task = pending;
      view.prompt = null;
      pending = null;
      if (key === "y" && task) {
        await task();
      } else {
        view.status = { text: "cancelled", kind: "info" };
        draw();
      }
      return;
    }
    if (busy) return;
    const row = view.rows[view.selected];
    switch (key) {
      case "up":
      case "k":
        view.selected = Math.max(0, view.selected - 1);
        break;
      case "down":
      case "j":
        view.selected = Math.min(Math.max(0, view.rows.length - 1), view.selected + 1);
        break;
      case "pageup":
        view.selected = Math.max(0, view.selected - 5);
        break;
      case "pagedown":
        view.selected = Math.min(Math.max(0, view.rows.length - 1), view.selected + 5);
        break;
      case "r":
        await run("refreshing…", async () => ({ text: "refreshed", kind: "ok" }));
        return;
      case "v":
        if (!row) break;
        if (!row.running) {
          view.status = { text: `${row.sessionName} is not running; nothing to verify`, kind: "error" };
          break;
        }
        await run(`verifying ${row.sessionName}…`, async () => {
          const { report } = lifecycle.verify(row.paneId);
          return { text: `${row.sessionName}: ${summarizeVerification(report)}`, kind: report.ok === false ? "error" : "ok" };
        });
        return;
      case "x":
        if (!row) break;
        if (!row.running && !row.serverRunning) {
          view.status = { text: `${row.sessionName} is not running`, kind: "error" };
          break;
        }
        ask(`Stop ${row.sessionName} (pane ${row.paneId})?`, () => run(`stopping ${row.sessionName}…`, async () => {
          await lifecycle.stop(row.paneId);
          return { text: `stopped ${row.sessionName}`, kind: "ok" };
        }));
        return;
      case "p": {
        const stale = view.rows.filter(isStale);
        if (view.paneIds === null) {
          view.status = { text: "Herdr's pane list is unavailable, so nothing can be pruned", kind: "error" };
          break;
        }
        if (stale.length === 0) {
          view.status = { text: "nothing to prune: every mapping has a pane or something running", kind: "info" };
          break;
        }
        ask(`Forget ${stale.length} mapping${stale.length === 1 ? "" : "s"} whose pane is gone (${stale.map((item) => item.paneId).join(", ")})?`, () => run("pruning…", async () => {
          const { pruned, kept } = lifecycle.prune(/** @type {string[]} */ (view.paneIds));
          return { text: `pruned ${pruned.length} mapping${pruned.length === 1 ? "" : "s"}${kept.length ? `, kept ${kept.length} (${kept[0].reason})` : ""}`, kind: "ok" };
        }));
        return;
      }
      default:
        return;
    }
    draw();
  };

  let rawCapable = false;
  try {
    if (typeof input.setRawMode === "function" && input.isTTY) {
      input.setRawMode(true);
      rawCapable = true;
    }
  } catch (error) {
    view.status = { text: `raw input unavailable (${errorMessageOf(error)}); press Ctrl-C or close the pane to leave`, kind: "error" };
  }
  const onData = (chunk) => {
    for (const key of parseKeys(String(chunk))) {
      onKey(key).catch((error) => {
        view.status = { text: errorMessageOf(error), kind: "error" };
        draw();
      });
    }
  };
  const onResize = () => draw();
  input.on("data", onData);
  output.on?.("resize", onResize);
  if (typeof input.resume === "function") {
    input.resume();
  }
  // Alternate screen, hidden cursor; both restored on the way out.
  output.write(`${ESC}[?1049h${ESC}[?25l${ESC}[2J`);
  try {
    while (!controller.signal.aborted) {
      if (!busy) {
        await refresh();
        draw();
      }
      try {
        await sleep(intervalMs, null, { signal: controller.signal });
      } catch {
        break;
      }
    }
  } finally {
    input.off("data", onData);
    output.off?.("resize", onResize);
    output.write(`${ESC}[?25h${ESC}[?1049l`);
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

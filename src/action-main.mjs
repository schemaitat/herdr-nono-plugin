/**
 * Action dispatcher. Every action prints the result marker line first and
 * uses stderr for diagnostics, so `herdr plugin log list` stays parseable.
 * @module action-main
 */
import { spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { existsSync } from "node:fs";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { resolveAgent } from "./agents.mjs";
import { loadConfig } from "./config.mjs";
import { BRIDGE_START_TIMEOUT_ENV, BRIDGE_START_TIMEOUT_MS, KEYBINDING_INSTALL_TIMEOUT_MS, MIN_NONO_VERSION } from "./constants.mjs";
import { readContext, readPluginEnv, requirePluginDirs, resolvePaneId, resolveWorkdir, resolveWorkspaceRoot } from "./context.mjs";
import { PluginError, errorKindOf, errorMessageOf } from "./errors.mjs";
import { createHerdrClient } from "./herdr.mjs";
import { detectOpencodeService, hostServiceWarning } from "./hostservice.mjs";
import { agentForEntry, assertWorkspaceRoot, bridgeIsRunning, createLifecycle, findExecutable, liveShells, shellIsRunning } from "./lifecycle.mjs";
import { sessionNameFor } from "./naming.mjs";
import { createNonoClient, summarizeProfile } from "./nono.mjs";
import { runProbes } from "./probes.mjs";
import { emitResult, failurePayload } from "./result.mjs";
import { buildPaneCommand } from "./shell.mjs";
import { deletePaneEntry, getPaneEntry, loadState, paneLockPath, savePaneEntry, withPaneLock } from "./state.mjs";
import { summarizeVerification } from "./verify.mjs";

/**
 * Builds the command typed into a pane to run the bridge.
 * @param {{pluginEnv: {pluginRoot: string, stateDir: string, configDir: string, herdrBin?: string}, mode: string, paneId: string, detectionKind?: string|null, nonoBin?: string|null, launchId?: string|null}} input
 * @returns {string}
 */
export function bridgeCommand({ pluginEnv, mode, paneId, detectionKind = null, nonoBin = null, launchId = null }) {
  const argv = [
    process.execPath,
    path.join(pluginEnv.pluginRoot, "src", "bridge.mjs"),
    mode,
    "--state-dir", pluginEnv.stateDir,
    "--config-dir", pluginEnv.configDir,
    "--pane-id", paneId,
    "--plugin-root", pluginEnv.pluginRoot,
    "--herdr-bin", pluginEnv.herdrBin ?? "herdr",
  ];
  if (nonoBin) argv.push("--nono-bin", nonoBin);
  if (launchId) argv.push("--launch-id", launchId);
  return buildPaneCommand({ argv, env: detectionKind ? { HERDR_AGENT: detectionKind } : {} });
}

function workspaceOf(deps) {
  return deps.context.workspace_id ?? deps.env.HERDR_WORKSPACE_ID ?? null;
}

/**
 * Finds the mapping a pane-scoped action should act on: the focused pane's
 * own mapping; else the single mapping in the focused workspace (users tend
 * to run actions from the pane next to the agent); else the single mapping
 * whose pane Herdr no longer has, which happens after a Herdr restart. Such an
 * orphan is flagged so the caller can give it a new pane.
 */
function requireFocusedMapping(deps) {
  const focused = resolvePaneId(deps.context, deps.env);
  const own = getPaneEntry(deps.pluginEnv.stateDir, focused);
  if (own) {
    return { paneId: focused, entry: own, viaWorkspace: false, orphan: false };
  }
  const workspaceId = workspaceOf(deps);
  const all = Object.values(loadState(deps.pluginEnv.stateDir).panes);
  const candidates = workspaceId
    ? all.filter((entry) => entry.workspaceId === workspaceId || String(entry.paneId).startsWith(`${workspaceId}:`))
    : [];
  if (candidates.length === 1) {
    const [entry] = candidates;
    const paneExists = deps.herdr.getPane(entry.paneId) !== null;
    process.stderr.write(`pane ${focused ?? "(none)"} has no sandboxed agent; using the workspace's only mapping, pane ${entry.paneId} (${entry.sessionName})${paneExists ? "" : ", whose pane is gone"}\n`);
    return { paneId: entry.paneId, entry, viaWorkspace: paneExists, orphan: !paneExists };
  }
  if (candidates.length > 1) {
    const list = candidates.map((entry) => `${entry.paneId} (${entry.sessionName})`).join(", ");
    throw new PluginError("target", `Pane ${focused ?? "(none)"} has no sandboxed agent and this workspace has several: ${list}. Focus the pane you mean.`);
  }
  if (all.length > 0) {
    const paneIds = new Set(deps.herdr.listPaneIds());
    const orphans = all.filter((entry) => !paneIds.has(entry.paneId));
    if (orphans.length === 1) {
      const [entry] = orphans;
      process.stderr.write(`pane ${entry.paneId} no longer exists; adopting its mapping ${entry.sessionName}\n`);
      return { paneId: entry.paneId, entry, viaWorkspace: false, orphan: true };
    }
    if (orphans.length > 1) {
      const list = orphans.map((entry) => `${entry.paneId} (${entry.sessionName})`).join(", ");
      throw new PluginError("target", `Several mappings lost their panes: ${list}. Run prune-mappings, then try again.`);
    }
    const elsewhere = all.map((entry) => `${entry.paneId} (${entry.sessionName})`).join(", ");
    throw new PluginError("target", `No sandboxed agent is mapped in workspace ${workspaceId ?? "(unknown)"}. Existing mappings: ${elsewhere}. Focus that workspace, for example "herdr workspace focus ${String(all[0].paneId).split(":")[0]}", and try again.`);
  }
  throw new PluginError("target", focused ? `No sandboxed agent is mapped to pane ${focused} or to another pane in this workspace. Run start-agent first.` : "No focused pane was provided, so there is no mapping to act on.");
}

function refuseWhileAgentRuns(deps, target, verb, { shellsBlock = true } = {}) {
  if (bridgeIsRunning(target.entry)) {
    throw new PluginError("conflict", `Pane ${target.paneId} still runs ${target.entry.sessionName} (bridge pid ${target.entry.bridgePid}, since ${target.entry.bridgeStartedAt}). Exit the agent, or run stop, before you ${verb}.`);
  }
  if (shellsBlock) {
    const shells = liveShells(target.entry);
    if (shells.length > 0) {
      throw new PluginError("conflict", `Pane ${target.paneId} has ${shells.length === 1 ? "an open-shell session" : `${shells.length} open-shell sessions`} for ${target.entry.sessionName} (pid ${shells.map((shell) => shell.pid).join(", ")}). Close ${shells.length === 1 ? "it" : "them"} before you ${verb}.`);
    }
  }
  if (target.orphan) {
    return;
  }
  const agent = target.viaWorkspace ? deps.herdr.getPane(target.paneId)?.agent ?? null : deps.context.focused_pane_agent ?? null;
  if (agent) {
    throw new PluginError("conflict", `Pane ${target.paneId} is still running agent "${agent}". Exit it before you ${verb}. If nothing is running there, clear a stale report with "herdr pane release-agent ${target.paneId} --source nono.sandbox --agent ${agent}".`);
  }
}

/**
 * How long to wait for a typed bridge command to acknowledge itself, in
 * milliseconds. An unset, empty or non-positive override means the default.
 * @param {Record<string, string|undefined>} env
 * @returns {number}
 */
export function bridgeStartTimeout(env) {
  const raw = Number(env[BRIDGE_START_TIMEOUT_ENV]);
  return Number.isFinite(raw) && raw > 0 ? raw : BRIDGE_START_TIMEOUT_MS;
}

/**
 * The directory a pane for a mapping opens in: the recorded working
 * directory, else the workspace root, else nothing when both are gone.
 * @param {{workdir?: string|null, localPath?: string}} entry
 * @returns {string|null}
 */
export function entryCwd(entry) {
  if (typeof entry.workdir === "string" && existsSync(entry.workdir)) {
    return entry.workdir;
  }
  return typeof entry.localPath === "string" && existsSync(entry.localPath) ? entry.localPath : null;
}

/**
 * Types the bridge command into `paneId` and waits until the bridge has
 * touched the mapping. A pane that is not at a shell prompt swallows typed
 * text, so when nothing happens the mapping is moved to a fresh pane next to
 * the user and started there.
 * @returns {Promise<{paneId: string, movedTo: string|null}>}
 */
async function startBridge(deps, { paneId, mode, agent, label }) {
  const launchId = randomBytes(6).toString("hex");
  deps.herdr.runInPane(paneId, bridgeCommand({ pluginEnv: deps.pluginEnv, mode, paneId, detectionKind: agent.herdrDetectionKind, nonoBin: deps.nono.bin, launchId }));
  const acknowledged = () => getPaneEntry(deps.pluginEnv.stateDir, paneId)?.bridgeLaunchId === launchId;
  const deadline = Date.now() + bridgeStartTimeout(deps.env);
  while (Date.now() < deadline) {
    await sleep(150);
    if (acknowledged()) {
      return { paneId, movedTo: null };
    }
  }
  const entry = getPaneEntry(deps.pluginEnv.stateDir, paneId);
  if (!entry || acknowledged()) {
    return { paneId, movedTo: null };
  }
  if (bridgeIsRunning(entry)) {
    process.stderr.write(`pane ${paneId} still runs an earlier bridge for ${entry.sessionName} (pid ${entry.bridgePid}); leaving the mapping there\n`);
    return { paneId, movedTo: null };
  }
  process.stderr.write(`pane ${paneId} did not run the bridge command; starting in a new pane instead\n`);
  const fresh = rehomeOrphan(deps, { paneId, entry }, label, { abandonIf: acknowledged });
  if (fresh === null) {
    process.stderr.write(`pane ${paneId} ran the bridge command after all; keeping the agent there\n`);
    return { paneId, movedTo: null };
  }
  try {
    deps.herdr.renamePane(paneId, `${label} (moved to ${fresh})`);
  } catch (error) {
    process.stderr.write(`could not relabel pane ${paneId}: ${errorMessageOf(error)}\n`);
  }
  deps.herdr.runInPane(fresh, bridgeCommand({ pluginEnv: deps.pluginEnv, mode, paneId: fresh, detectionKind: agent.herdrDetectionKind, nonoBin: deps.nono.bin }));
  return { paneId: fresh, movedTo: fresh };
}

function freshEntry({ sessionName, agent, localPath, workdir, sourcePaneId, workspaceId = null }) {
  return {
    sessionName,
    workspaceId,
    agentKind: agent.kind,
    localPath,
    workdir,
    profile: agent.profileRef,
    serverProfile: agent.serverProfileRef,
    lifecycleState: "provisional",
    createdAt: new Date().toISOString(),
    sourcePaneId,
    launchCount: 0,
    lastError: null,
    verification: null,
  };
}

/**
 * Extracts `major.minor.patch` from a version string.
 * @param {unknown} value
 * @returns {number[]|null}
 */
export function parseVersion(value) {
  const match = String(value ?? "").match(/(\d+)\.(\d+)\.(\d+)/);
  return match ? [Number(match[1]), Number(match[2]), Number(match[3])] : null;
}

/**
 * Returns a warning when the nono version is older than the one this plugin targets, else null.
 * @param {string|null} version
 * @returns {string|null}
 */
export function nonoVersionWarning(version) {
  const found = parseVersion(version);
  const wanted = parseVersion(MIN_NONO_VERSION);
  if (!found || !wanted) {
    return null;
  }
  for (let index = 0; index < 3; index += 1) {
    if (found[index] !== wanted[index]) {
      return found[index] < wanted[index] ? `nono ${found.join(".")} is older than ${MIN_NONO_VERSION}; the plugin was written against the newer CLI and some flags may be missing.` : null;
    }
  }
  return null;
}

/**
 * Opens the pane an agent will run in: a split next to the anchor pane, or a
 * new tab when `openIn` is "tab". Returns the new pane id, already labelled.
 */
function openAgentPane(deps, { anchorPaneId, cwd, label }) {
  let paneId;
  if (deps.config.openIn === "tab") {
    ({ paneId } = deps.herdr.createTab({ workspaceId: workspaceOf(deps), cwd, label, focus: true }));
  } else {
    paneId = deps.herdr.splitPane({ paneId: anchorPaneId, direction: deps.config.paneDirection, ratio: deps.config.paneRatio, cwd, focus: true });
  }
  deps.herdr.renamePane(paneId, label);
  return paneId;
}

/**
 * Gives a mapping a new pane next to the focused one and moves the mapping
 * there. Returns the new pane id, or null when `abandonIf` reports, once the
 * pane exists, that the mapping must stay where it is; the unused pane is
 * closed again then.
 * @returns {string|null}
 */
function rehomeOrphan(deps, target, label, { abandonIf = null } = {}) {
  const stateDir = deps.pluginEnv.stateDir;
  const oldPaneId = target.entry.paneId;
  const focused = resolvePaneId(deps.context, deps.env);
  const paneId = openAgentPane(deps, { anchorPaneId: focused, cwd: entryCwd(target.entry), label });
  const closeSpare = () => {
    try {
      deps.herdr.closePane(paneId);
    } catch (error) {
      process.stderr.write(`could not close the unused pane ${paneId}: ${errorMessageOf(error)}\n`);
    }
  };
  if (paneId !== oldPaneId && abandonIf && abandonIf()) {
    closeSpare();
    return null;
  }
  let moved;
  try {
    // Both panes' locks, in a fixed order, so two mirrored moves cannot wait for each other.
    const ordered = [...new Set([oldPaneId, paneId])].sort((a, b) => (paneLockPath(stateDir, a) < paneLockPath(stateDir, b) ? -1 : 1));
    const locked = (fn) => ordered.reduceRight((inner, id) => () => withPaneLock(stateDir, id, inner), fn)();
    moved = locked(() => {
      const latest = getPaneEntry(stateDir, oldPaneId);
      if (!latest) {
        throw new PluginError("conflict", `The mapping of pane ${oldPaneId} disappeared while a new pane was being opened for it; nothing was moved.`);
      }
      if (paneId !== oldPaneId && abandonIf && abandonIf()) {
        return null;
      }
      if (bridgeIsRunning(latest)) {
        throw new PluginError("conflict", `The mapping of pane ${oldPaneId} is in use again (bridge ${latest.bridgePid}); nothing was moved.`);
      }
      const next = { ...latest, workspaceId: workspaceOf(deps), sourcePaneId: focused, adoptedFrom: oldPaneId };
      if (paneId !== oldPaneId) {
        const displaced = getPaneEntry(stateDir, paneId);
        if (displaced && (bridgeIsRunning(displaced) || shellIsRunning(displaced))) {
          throw new PluginError("conflict", `Herdr handed out pane ${paneId}, but its mapping to ${displaced.sessionName} is still in use; nothing was moved.`);
        }
      }
      savePaneEntry(stateDir, paneId, next);
      if (paneId !== oldPaneId) {
        deletePaneEntry(stateDir, oldPaneId);
      }
      return next;
    });
  } catch (error) {
    closeSpare();
    throw error;
  }
  if (moved === null) {
    closeSpare();
    return null;
  }
  return paneId;
}

/** Pane label for an agent pane: the agent kind plus the session's short id, so several agents in one workspace stay apart. */
function paneLabel(agentKind, sessionName) {
  const shortId = String(sessionName).split("-").pop().slice(0, 6);
  return `nono ${agentKind} ${shortId}`;
}

/**
 * Parses the report lines printed by scripts/install-keybindings.sh.
 * @param {string} text
 * @returns {{configPath: string|null, added: Array<{key: string, action: string}>, existing: Array<{key: string, action: string}>, warnings: string[], reloaded: boolean}}
 */
export function parseKeybindingReport(text) {
  const report = { configPath: /** @type {string|null} */ (null), added: /** @type {Array<{key: string, action: string}>} */ ([]), existing: /** @type {Array<{key: string, action: string}>} */ ([]), warnings: /** @type {string[]} */ ([]), reloaded: false };
  for (const line of text.split("\n")) {
    let match;
    if ((match = line.match(/^config: (.+)$/))) {
      report.configPath = match[1];
    } else if ((match = line.match(/^bound (\S+) -> nono\.sandbox\.(\S+)$/))) {
      report.added.push({ key: match[1], action: match[2] });
    } else if ((match = line.match(/^already bound: nono\.sandbox\.(\S+) \((.+)\)$/))) {
      report.existing.push({ key: match[2], action: match[1] });
    } else if ((match = line.match(/^warning: (.+)$/))) {
      report.warnings.push(match[1]);
    } else if (line === "reloaded") {
      report.reloaded = true;
    }
  }
  return report;
}

const ACTIONS = {
  async doctor(deps) {
    const version = deps.nono.version();
    const versionWarning = nonoVersionWarning(version.version);
    const agent = resolveAgent(deps.config, { pluginRoot: deps.pluginEnv.pluginRoot });
    const resolve = (ref) => {
      try {
        return { ref, ...summarizeProfile(deps.nono.showProfile(ref)) };
      } catch (error) {
        const hint = /nolabs-ai\/opencode/.test(/** @type {any} */ (error)?.output ?? "") ? ' Install the OpenCode pack with "nono pull nolabs-ai/opencode".' : "";
        throw new PluginError("config", `nono cannot resolve the profile ${ref}: ${errorMessageOf(error)}.${hint}`, { output: /** @type {any} */ (error)?.output, cause: error });
      }
    };
    const profile = resolve(agent.profileRef);
    const serverProfile = agent.serverProfileRef ? resolve(agent.serverProfileRef) : null;
    // The agent's tools run where its server runs.
    const tools = serverProfile ?? profile;
    const warnings = [];
    if (versionWarning) warnings.push(versionWarning);
    const agentBinary = findExecutable(agent.command[0], deps.env);
    if (!agentBinary) warnings.push(`The agent command "${agent.command[0]}" is not on the PATH Herdr gives plugins.`);
    for (const item of [profile, serverProfile].filter(Boolean)) {
      if (item.afUnixMediation !== "pathname") warnings.push(`Profile ${item.ref} does not set linux.af_unix_mediation to "pathname", so the sandbox can connect to Herdr's control socket and other host sockets.`);
    }
    if (serverProfile && profile.egress !== "blocked") warnings.push(`The client profile ${profile.ref} does not block the network; the client only needs its server's port.`);
    if (tools.egress === "open") warnings.push(`Profile ${tools.ref}, which the agent's tools run under, lets them connect to localhost services directly.`);
    for (const item of tools.loopbackDomains) warnings.push(`Profile ${tools.ref}, which the agent's tools run under, allows "${item.domain}", which ${item.reason}; nono's proxy then forwards to localhost services.`);
    const hostService = agent.hostService === "opencode" ? detectOpencodeService({ env: deps.env }) : null;
    const serviceReachable = Boolean(hostService?.running) && tools.loopback;
    const serviceWarning = hostService && serviceReachable ? hostServiceWarning(hostService) : null;
    if (serviceWarning) warnings.push(serviceWarning);
    let probes = null;
    let probeError = null;
    try {
      probes = (await runProbes({ nonoBin: deps.nono.bin, profile: tools.ref, env: deps.env })).checks;
    } catch (error) {
      probeError = { kind: errorKindOf(error), message: errorMessageOf(error) };
      warnings.push(`The escape probe could not run: ${errorMessageOf(error)}`);
    }
    const failedProbes = (probes ?? []).filter((check) => !check.ok);
    for (const check of failedProbes.filter((item) => item.severity !== "warning")) {
      warnings.push(`probe ${check.check}: ${check.result} (${check.why})`);
    }
    const payload = {
      nonoBin: deps.nono.bin,
      nonoVersion: version.version ?? version.raw,
      versionWarning,
      node: process.execPath,
      pluginRoot: deps.pluginEnv.pluginRoot,
      stateDir: deps.pluginEnv.stateDir,
      configDir: deps.pluginEnv.configDir,
      agentKind: agent.kind,
      agentBinary,
      launchArgv: agent.launchArgv,
      serverArgv: agent.server?.command ?? null,
      profile,
      serverProfile,
      hostService: hostService ? { ...hostService, reachable: serviceReachable } : null,
      probes,
      probeError,
      warnings,
    };
    const describeProfile = (label, item) => `${label}: ${item.ref} (extends ${item.extends.join(", ") || "nothing"}; egress ${item.egress}${item.allowDomains.length > 0 ? ` [${item.allowDomains.join(", ")}]` : ""}; localhost ${item.loopback ? "REACHABLE" : "unreachable"}; AF_UNIX mediation ${item.afUnixMediation})`;
    const lines = [
      `nono executable: ${deps.nono.bin} (${version.raw})`,
      `node: ${process.execPath}`,
      `plugin root: ${deps.pluginEnv.pluginRoot}`,
      `state dir: ${deps.pluginEnv.stateDir}`,
      `config dir: ${deps.pluginEnv.configDir}`,
      `configured agent: ${agent.kind} (${agent.launchArgv.join(" ")}) at ${agentBinary ?? "NOT FOUND"}`,
      describeProfile(serverProfile ? "client profile" : "profile", profile),
    ];
    if (serverProfile) {
      lines.push(`server: ${agent.server.command.join(" ")}`, describeProfile("server profile", serverProfile));
    }
    if (hostService?.running) {
      lines.push(`OpenCode host service: pid ${hostService.pid}, ${hostService.url ?? `port ${hostService.port}`}, ${serviceReachable ? "REACHABLE from the agent's tools" : "not reachable from the agent's tools"}`);
    }
    lines.push(`escape probe with ${tools.ref}:`);
    for (const check of probes ?? []) {
      lines.push(`probe ${check.ok ? "ok  " : check.severity === "warning" ? "warn" : "FAIL"} ${check.check}: ${check.result}${check.ok ? "" : ` (${check.why})`}`);
    }
    for (const warning of warnings) lines.push(`warning: ${warning}`);
    if (serviceWarning && deps.config.hostServiceCheck === "refuse") {
      throw new PluginError("unconfined", `start-agent and reconnect will refuse to run: an OpenCode background service (pid ${hostService?.pid}) runs outside any sandbox and the agent's tools can reach it, so they could use it to run commands on the host. Stop it with "opencode service stop", and restrict network.allow_domain in the server profile to your provider's hosts.`, { payload, output: lines.join("\n") });
    }
    if (agent.hostService === "opencode" && tools.loopback && deps.config.hostServiceCheck === "refuse") {
      throw new PluginError("unconfined", `start-agent and reconnect will refuse to run: the agent's tools can reach localhost under profile ${tools.ref}, where an OpenCode host service can start at any time. Restrict network.allow_domain in the server profile to your provider's hosts.`, { payload, output: lines.join("\n") });
    }
    const critical = failedProbes.filter((check) => check.severity === "critical");
    if (critical.length > 0) {
      throw new PluginError("unconfined", `The escape probe got through with profile ${tools.ref}: ${critical.map((check) => `${check.check} ${check.result}`).join(", ")}.`, { payload, output: lines.join("\n") });
    }
    return { payload, lines };
  },

  "install-keybindings"(deps) {
    const result = spawnSync("sh", ["scripts/install-keybindings.sh"], {
      cwd: deps.pluginEnv.pluginRoot,
      encoding: "utf8",
      env: { ...deps.env, HERDR_BIN_PATH: deps.pluginEnv.herdrBin },
      timeout: KEYBINDING_INSTALL_TIMEOUT_MS,
      killSignal: "SIGKILL",
      maxBuffer: 4 * 1024 * 1024,
    });
    const output = `${result.stdout ?? ""}${result.stderr ?? ""}`;
    if (result.error) {
      const timedOut = /** @type {any} */ (result.error).code === "ETIMEDOUT";
      throw new PluginError("startup", timedOut
        ? `scripts/install-keybindings.sh did not finish within ${Math.round(KEYBINDING_INSTALL_TIMEOUT_MS / 1000)}s (Herdr's config check or reload hung) and was killed.`
        : `Could not run scripts/install-keybindings.sh: ${result.error.message}`, { output });
    }
    const report = parseKeybindingReport(result.stdout ?? "");
    if (result.status !== 0) {
      const restored = report.warnings.some((warning) => /restored to its previous content/.test(warning));
      throw new PluginError("config", `scripts/install-keybindings.sh exited with status ${result.status}; ${report.configPath ?? "the Herdr config"} ${restored ? "was restored to its previous content" : "was left as it was"} and Herdr was not reloaded.`, { output });
    }
    return { payload: { ...report }, lines: output.split("\n").filter(Boolean) };
  },

  "start-agent"(deps) {
    const root = resolveWorkspaceRoot(deps.context);
    if (!root) {
      throw new PluginError("target", "The invocation context has no workspace or pane directory. Invoke start-agent from a workspace or pane.");
    }
    const localPath = assertWorkspaceRoot(root);
    const workdir = resolveWorkdir(deps.context, localPath);
    const sourcePaneId = resolvePaneId(deps.context, deps.env);
    const agent = resolveAgent(deps.config, { pluginRoot: deps.pluginEnv.pluginRoot });
    // Fail before touching Herdr when nono is missing.
    deps.nono.version();
    const sessionName = sessionNameFor({ prefix: deps.config.sessionNamePrefix, agentKind: agent.kind, localPath, paneId: sourcePaneId });
    const paneId = openAgentPane(deps, { anchorPaneId: sourcePaneId, cwd: workdir, label: paneLabel(agent.kind, sessionName) });
    const entry = freshEntry({ sessionName, agent, localPath, workdir, sourcePaneId, workspaceId: workspaceOf(deps) });
    withPaneLock(deps.pluginEnv.stateDir, paneId, () => {
      const stale = getPaneEntry(deps.pluginEnv.stateDir, paneId);
      if (stale && (bridgeIsRunning(stale) || shellIsRunning(stale))) {
        throw new PluginError("conflict", `Herdr handed out pane ${paneId}, but its mapping to ${stale.sessionName} is still in use (bridge ${stale.bridgePid ?? "none"}). Try again in a moment.`);
      }
      if (stale) {
        process.stderr.write(`pane ${paneId} was previously mapped to ${stale.sessionName}; replacing that mapping\n`);
      }
      savePaneEntry(deps.pluginEnv.stateDir, paneId, entry);
    });
    deps.herdr.runInPane(paneId, bridgeCommand({ pluginEnv: deps.pluginEnv, mode: "start", paneId, detectionKind: agent.herdrDetectionKind, nonoBin: deps.nono.bin }));
    deps.herdr.notify("nono sandbox starting", `${agent.title} in ${sessionName}`);
    return { payload: { paneId, sourcePaneId, sessionName, agentKind: agent.kind, localPath, workdir, profile: agent.profileRef, serverProfile: agent.serverProfileRef, launchArgv: agent.launchArgv, serverArgv: agent.server?.command ?? null, openIn: deps.config.openIn } };
  },

  async reconnect(deps) {
    const target = requireFocusedMapping(deps);
    const { entry } = target;
    // An open shell is a separate sandbox; it does not stop the agent from coming back.
    refuseWhileAgentRuns(deps, target, "reconnect", { shellsBlock: false });
    const agent = agentForEntry(deps.config, entry, deps.pluginEnv.pluginRoot);
    const label = paneLabel(agent.kind, entry.sessionName);
    const paneId = target.orphan ? rehomeOrphan(deps, target, label) : target.paneId;
    if (paneId === null) {
      throw new PluginError("conflict", `The mapping of pane ${target.paneId} is in use again; nothing was started.`);
    }
    // Resume arguments only make sense once the agent has run in this pane.
    const mode = (entry.launchCount ?? 0) > 0 ? "connect" : "start";
    const started = await startBridge(deps, { paneId, mode, agent, label });
    return { payload: { paneId: started.paneId, sessionName: entry.sessionName, agentKind: entry.agentKind, mode, argv: mode === "connect" ? agent.resumeArgv : agent.launchArgv, adoptedFrom: target.orphan ? entry.paneId : null, movedTo: started.movedTo } };
  },

  "open-shell"(deps) {
    const target = requireFocusedMapping(deps);
    const { paneId, entry } = target;
    // The shell opens below the pane the user is in, wherever the agent pane lives.
    const anchor = resolvePaneId(deps.context, deps.env) ?? paneId;
    const shellPaneId = deps.herdr.splitPane({ paneId: anchor, direction: "down", ratio: 0.5, cwd: entryCwd(entry), focus: true });
    deps.herdr.renamePane(shellPaneId, `nono shell ${String(entry.sessionName).split("-").pop().slice(0, 6)}`);
    deps.herdr.runInPane(shellPaneId, bridgeCommand({ pluginEnv: deps.pluginEnv, mode: "shell", paneId, nonoBin: deps.nono.bin }));
    return { payload: { paneId: shellPaneId, mappedPaneId: paneId, sessionName: entry.sessionName } };
  },

  async stop(deps) {
    const { paneId } = requireFocusedMapping(deps);
    const outcome = await deps.lifecycle.stop(paneId);
    deps.herdr.notify("nono sandbox stopped", outcome.sessionName);
    return { payload: { paneId, sessionName: outcome.sessionName, sessionId: outcome.sessionId, serverSessionIds: outcome.serverSessionIds } };
  },

  info(deps) {
    const { paneId } = requireFocusedMapping(deps);
    const description = deps.lifecycle.describe(paneId);
    const lines = [JSON.stringify(description, null, 2)];
    return { payload: { paneId, ...description }, lines };
  },

  "verify-sandbox"(deps) {
    const { paneId } = requireFocusedMapping(deps);
    const { entry, report } = deps.lifecycle.verify(paneId);
    const lines = [`${entry.sessionName}: ${summarizeVerification(report)}`];
    for (const item of report.processes) {
      lines.push(`  ${item.confined ? "confined  " : "UNCONFINED"} ${String(item.pid).padStart(7)} ${item.sandbox.padEnd(6)} ${item.role.padEnd(6)} ${item.command}`);
    }
    for (const problem of report.problems) lines.push(`problem: ${problem}`);
    for (const warning of report.warnings) lines.push(`warning: ${warning}`);
    const payload = { paneId, sessionName: entry.sessionName, verified: report.ok, report };
    if (report.ok === false) {
      throw new PluginError("unconfined", `Sandbox verification failed for ${entry.sessionName}: ${report.problems.join(" ")}`, { payload, output: lines.join("\n") });
    }
    return { payload, lines };
  },

  "prune-mappings"(deps) {
    let paneIds;
    try {
      paneIds = deps.herdr.listPaneIds();
    } catch (error) {
      throw new PluginError(errorKindOf(error), `Cannot prune without Herdr's pane list: ${errorMessageOf(error)}`, { output: /** @type {any} */ (error)?.output });
    }
    const { pruned, kept } = deps.lifecycle.prune(paneIds);
    const lines = [`pruned ${pruned.length} mapping${pruned.length === 1 ? "" : "s"}`];
    for (const item of pruned) lines.push(`  ${item.paneId}\t${item.sessionName}`);
    for (const item of kept) lines.push(`kept ${item.paneId}\t${item.sessionName}\t${item.reason}`);
    return { payload: { pruned, kept }, lines };
  },

  sandboxes(deps) {
    deps.herdr.openPluginPane({ pluginId: deps.pluginEnv.pluginId, entrypointId: "sandboxes", focus: true });
    return { payload: { entrypoint: "sandboxes" } };
  },

  "list-sandboxes"(deps) {
    const outcome = deps.lifecycle.listAll();
    let paneIds = null;
    let paneError = null;
    try {
      paneIds = new Set(deps.herdr.listPaneIds());
    } catch (error) {
      paneError = errorMessageOf(error);
    }
    outcome.mappings = outcome.mappings.map((item) => ({ ...item, paneExists: paneIds ? paneIds.has(item.paneId) : null, paneError }));
    const lines = outcome.mappings.length === 0
      ? ["No sandboxed agents are mapped."]
      : outcome.mappings.map((item) => `${item.paneId}${item.paneExists === false ? " (pane gone)" : item.paneExists === null ? " (pane ?)" : ""}\t${item.sessionName}\t${item.agentKind}\t${item.lifecycleState}\t${item.running === null ? "unknown" : item.running ? "running" : "not running"}\t${item.verification ?? "not verified"}\t${item.localPath}`);
    if (outcome.sessionError) {
      lines.push(`nono ps failed (${outcome.sessionError.kind}): ${outcome.sessionError.message}`);
    }
    return { payload: outcome, lines };
  },

  "forget-mapping"(deps) {
    const target = requireFocusedMapping(deps);
    const { paneId, entry } = target;
    refuseWhileAgentRuns(deps, target, "forget the mapping");
    const removed = withPaneLock(deps.pluginEnv.stateDir, paneId, () => {
      const now = getPaneEntry(deps.pluginEnv.stateDir, paneId);
      if (now && (bridgeIsRunning(now) || shellIsRunning(now))) {
        throw new PluginError("conflict", `Pane ${paneId} started ${now.sessionName} again meanwhile; nothing was forgotten.`);
      }
      return deletePaneEntry(deps.pluginEnv.stateDir, paneId);
    });
    return { payload: { paneId, sessionName: entry.sessionName, removed } };
  },
};

/** Action ids handled by this dispatcher, in manifest order. */
export const ACTION_IDS = Object.freeze(Object.keys(ACTIONS));

/**
 * Runs the action named by HERDR_PLUGIN_ACTION_ID and returns the exit code.
 * @param {NodeJS.ProcessEnv} [env]
 * @returns {Promise<number>}
 */
export async function main(env = process.env) {
  const pluginEnv = readPluginEnv(env);
  const action = pluginEnv.actionId;
  try {
    requirePluginDirs(pluginEnv);
    const handler = typeof action === "string" && Object.hasOwn(ACTIONS, action) ? ACTIONS[action] : null;
    if (!handler) {
      throw new PluginError("target", `Unknown action "${action}". Known actions: ${ACTION_IDS.join(", ")}.`);
    }
    const context = readContext(env);
    const config = loadConfig(pluginEnv.configDir, env);
    const nono = createNonoClient({ bin: config.nonoBin, env });
    const herdr = createHerdrClient({ bin: pluginEnv.herdrBin, env });
    const lifecycle = createLifecycle({ stateDir: pluginEnv.stateDir, config, pluginRoot: pluginEnv.pluginRoot, nono, log: (line) => process.stderr.write(`${line}\n`), herdr, env });
    const { payload, lines = [] } = await handler({ env, pluginEnv, context, config, nono, herdr, lifecycle });
    emitResult({ action, ok: true, ...payload }, lines);
    return 0;
  } catch (error) {
    emitResult(failurePayload(action, error));
    process.stderr.write(`${errorMessageOf(error)}\n`);
    const output = /** @type {any} */ (error)?.output;
    if (typeof output === "string" && output.trim() !== "") {
      process.stderr.write(`${output.trim()}\n`);
    }
    return 1;
  }
}

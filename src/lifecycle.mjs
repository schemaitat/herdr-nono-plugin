/**
 * Session lifecycle shared by the action dispatcher, the pane bridge and the
 * event hook. A nono sandbox lives exactly as long as the process it runs, so
 * the lifecycle is launch, run, exit; the mapping remembers the pane, the
 * workspace root, the agent and the last launch so `reconnect` can run the
 * agent again under the same name and policy. Progress messages go through
 * the injected `log` so pane modes print to the terminal while captured modes
 * keep stdout clean for the result marker.
 * @module lifecycle
 */
import { spawn, spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, statSync } from "node:fs";
import { request } from "node:http";
import { createServer } from "node:net";
import { homedir } from "node:os";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { resolveAgent, withPort } from "./agents.mjs";
import { AGENT_REPORT_SOURCE, LOGS_DIR, SERVER_READY_TIMEOUT_ENV, SERVER_READY_TIMEOUT_MS, SERVER_STOP_GRACE_MS, STOP_WAIT_MS, STRIPPED_ENV_PATTERNS, TERMINAL_RESTORE_SEQUENCE, VERIFY_INTERVAL_MS, VERIFY_WINDOW_ENV, VERIFY_WINDOW_MS } from "./constants.mjs";
import { canonicalPath } from "./context.mjs";
import { PluginError, errorKindOf, errorMessageOf } from "./errors.mjs";
import { detectOpencodeService, hostServiceRefusal, hostServiceWarning } from "./hostservice.mjs";
import { serverSessionName, shellSessionName } from "./naming.mjs";
import { buildRunArgs, createNonoClient, summarizeProfile } from "./nono.mjs";
import { procfsAvailable } from "./procfs.mjs";
import { deletePaneEntryIfUnchanged, getPaneEntry, loadState, ownStartToken, processStartToken, requirePaneEntry, updatePaneEntry, withPaneLock } from "./state.mjs";
import { summarizeVerification, verifySession } from "./verify.mjs";

/**
 * The command line of a process, or null when it cannot be read. Linux exposes
 * it under /proc; elsewhere `ps` answers.
 * @param {number} pid
 * @returns {string|null}
 */
export function processCommandLine(pid) {
  try {
    return readFileSync(`/proc/${pid}/cmdline`, "utf8").replace(/\0/g, " ").trim();
  } catch {
    // Not Linux, or the process is gone: fall through to ps.
  }
  const result = spawnSync("ps", ["-o", "command=", "-p", String(pid)], { encoding: "utf8", timeout: 5000 });
  if (result.error || result.status !== 0) {
    return null;
  }
  const line = (result.stdout ?? "").trim();
  return line === "" ? null : line;
}

/**
 * Whether a recorded process is alive, is the same incarnation that was
 * recorded (start token), and runs one of the given scripts for the pane. A
 * live process whose command line cannot be read counts as the owner, so the
 * checks fail closed.
 * @param {{pid: unknown, token: string|null}} record
 * @param {string[]} scripts Script basenames one of which the command line must end a word with.
 * @param {string|null} paneId When given, the command line must carry `--pane-id <paneId>`.
 * @param {string|null} mode When given, the command line must carry this word (the bridge mode).
 * @returns {boolean}
 */
export function processOwns(record, scripts, paneId = null, mode = null) {
  const pid = Number(record?.pid);
  if (!Number.isInteger(pid) || pid <= 0) {
    return false;
  }
  try {
    process.kill(pid, 0);
  } catch {
    // ESRCH: gone. EPERM: another user's process, which a process of ours never is.
    return false;
  }
  if (record.token && record.token !== "-") {
    const current = processStartToken(pid);
    if (current !== null && current !== record.token) {
      // The pid was recycled since the record was written.
      return false;
    }
  }
  const commandLine = processCommandLine(pid);
  if (commandLine === null) {
    return true;
  }
  const words = commandLine.split(/\s+/);
  if (!words.some((word) => scripts.some((script) => word.endsWith(script)))) {
    return false;
  }
  if (mode !== null && !words.includes(mode)) {
    return false;
  }
  if (paneId === null) {
    return true;
  }
  const paneIndex = words.indexOf("--pane-id");
  return paneIndex !== -1 && words[paneIndex + 1] === String(paneId);
}

/**
 * Whether the bridge process that last acknowledged a mapping is still alive
 * and really is that bridge. The bridge lives exactly as long as the agent's
 * nono session, so a live one means the agent is running in the pane.
 * @param {{bridgePid?: number|null, bridgeToken?: string|null, paneId?: string}} entry
 * @returns {boolean}
 */
export function bridgeIsRunning(entry) {
  return processOwns({ pid: entry?.bridgePid, token: entry?.bridgeToken ?? null }, ["bridge.mjs"], entry?.paneId);
}

/**
 * The open-shell sessions of a mapping that are still alive.
 * @param {{shellPids?: Array<{pid: number, token?: string|null, since?: string, paneId?: string}>, paneId?: string}} entry
 * @returns {Array<{pid: number, token?: string|null, since?: string, paneId?: string}>}
 */
export function liveShells(entry) {
  return (entry?.shellPids ?? []).filter((shell) => processOwns({ pid: shell.pid, token: shell.token ?? null }, ["bridge.mjs"], shell.paneId ?? entry?.paneId, "shell"));
}

/**
 * Whether an open-shell session is running for the mapping.
 * @param {{shellPids?: Array<{pid: number, token?: string|null}>, paneId?: string}} entry
 * @returns {boolean}
 */
export function shellIsRunning(entry) {
  return liveShells(entry).length > 0;
}

/**
 * Throws unless the path is an existing absolute directory.
 * @param {unknown} localPath
 * @returns {string}
 */
export function assertLocalPath(localPath) {
  if (typeof localPath !== "string" || !path.isAbsolute(localPath)) {
    throw new PluginError("target", `The workspace path must be absolute (got ${JSON.stringify(localPath)}).`);
  }
  if (!existsSync(localPath) || !statSync(localPath).isDirectory()) {
    throw new PluginError("target", `The workspace path ${localPath} is not an existing directory.`);
  }
  return localPath;
}

/**
 * Like {@link assertLocalPath}, but also refuses the filesystem root and the
 * home directory: granting either read-write to an agent is never intended.
 * @param {unknown} localPath
 * @returns {string} The resolved path.
 */
export function assertWorkspaceRoot(localPath) {
  assertLocalPath(localPath);
  const resolved = canonicalPath(/** @type {string} */ (localPath));
  const forbidden = [path.parse(resolved).root, canonicalPath(homedir())];
  if (forbidden.includes(resolved)) {
    throw new PluginError("target", `Refusing to grant ${resolved} to a sandboxed agent. Invoke start-agent from a project directory.`);
  }
  return resolved;
}

/**
 * Resolves the adapter recorded in a mapping, honoring current config overrides.
 * @param {Record<string, any>} config
 * @param {{agentKind?: string}} entry
 * @param {string} pluginRoot
 */
export function agentForEntry(config, entry, pluginRoot) {
  return resolveAgent({ ...config, agentKind: entry.agentKind }, { pluginRoot });
}

/**
 * The directory a launch starts in: the recorded working directory when it
 * still exists inside the workspace root, else the root.
 * @param {{workdir?: string|null, localPath: string}} entry
 * @returns {string}
 */
export function launchCwd(entry) {
  return typeof entry.workdir === "string" && existsSync(entry.workdir) ? entry.workdir : entry.localPath;
}

/**
 * The environment handed to `nono run`: the bridge's own environment minus
 * the variables in {@link STRIPPED_ENV_PATTERNS}, plus `agentEnv`.
 * @param {NodeJS.ProcessEnv} env
 * @param {string[]} [agentEnv] `KEY=VALUE` entries.
 * @returns {NodeJS.ProcessEnv}
 */
export function sandboxEnv(env, agentEnv = []) {
  /** @type {NodeJS.ProcessEnv} */
  const result = {};
  for (const [key, value] of Object.entries(env)) {
    if (!STRIPPED_ENV_PATTERNS.some((pattern) => pattern.test(key))) {
      result[key] = value;
    }
  }
  for (const item of agentEnv) {
    const index = item.indexOf("=");
    result[item.slice(0, index)] = item.slice(index + 1);
  }
  return result;
}

/**
 * Finds an executable on PATH the way a shell would, or null.
 * @param {string} command
 * @param {NodeJS.ProcessEnv} env
 * @returns {string|null}
 */
export function findExecutable(command, env = process.env) {
  if (command.includes("/")) {
    return existsSync(command) ? command : null;
  }
  for (const dir of String(env.PATH ?? "").split(path.delimiter)) {
    if (!dir) continue;
    const candidate = path.join(dir, command);
    try {
      if (statSync(candidate).isFile()) {
        return candidate;
      }
    } catch {
      // Not in this directory.
    }
  }
  return null;
}

/**
 * The argv and extra environment for an `open-shell` sandbox. nono's profiles
 * deny shell start-up files (they are a classic persistence target), so the
 * shells this plugin knows are started without them instead of printing
 * permission errors, with a prompt that says where the shell runs.
 * @param {string} shell
 * @param {string} sessionName
 * @returns {{argv: string[], env: Record<string, string>}}
 */
export function shellLaunch(shell, sessionName) {
  const label = `nono:${String(sessionName).split("-").pop().slice(0, 6)}`;
  switch (path.basename(shell)) {
    case "bash":
      return { argv: [shell, "--noprofile", "--norc"], env: { PS1: `[${label}] \\w \\$ ` } };
    case "zsh":
      return { argv: [shell, "--no-rcs"], env: { PS1: `[${label}] %~ %# ` } };
    case "fish":
      return { argv: [shell, "--no-config"], env: {} };
    default:
      return { argv: [shell], env: { PS1: `[${label}] $ ` } };
  }
}

/**
 * Picks a free TCP port on 127.0.0.1 by binding port 0 on the host, where
 * that is allowed, and releasing it again for the sandboxed server.
 * @returns {Promise<number>}
 */
export function pickFreePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = /** @type {import("node:net").AddressInfo} */ (server.address());
      server.close(() => resolve(port));
    });
  });
}

/**
 * One authenticated GET against the private server; resolves to the HTTP
 * status, or null when nothing answered.
 * @param {{port: number, path: string, user: string, password: string}} input
 * @returns {Promise<number|null>}
 */
function probeServer({ port, path: urlPath, user, password }) {
  return new Promise((resolve) => {
    const req = request({ host: "127.0.0.1", port, path: urlPath, auth: `${user}:${password}`, timeout: 1000 }, (res) => {
      res.resume();
      resolve(res.statusCode ?? null);
    });
    req.on("timeout", () => req.destroy());
    req.on("error", () => resolve(null));
    req.end();
  });
}

/**
 * The last lines of a log file, for error messages.
 * @param {string} file
 * @param {number} [count]
 * @returns {string}
 */
function tail(file, count = 15) {
  try {
    return readFileSync(file, "utf8").trimEnd().split("\n").slice(-count).join("\n");
  } catch {
    return "";
  }
}

function describeError(error) {
  return { kind: errorKindOf(error), message: errorMessageOf(error), at: new Date().toISOString() };
}

function verifyWindow(env) {
  const raw = Number(env[VERIFY_WINDOW_ENV]);
  return Number.isFinite(raw) && raw >= 0 ? raw : VERIFY_WINDOW_MS;
}

/**
 * Creates the lifecycle API bound to one state dir, config and nono client.
 * @param {{stateDir: string, config: Record<string, any>, pluginRoot: string, nono?: ReturnType<typeof createNonoClient>, log?: (line: string) => void, herdr?: Record<string, any>|null, env?: NodeJS.ProcessEnv, procRoot?: string}} input
 */
export function createLifecycle({ stateDir, config, pluginRoot, nono = createNonoClient({ bin: config.nonoBin }), log = (line) => process.stderr.write(`${line}\n`), herdr = null, env = process.env, procRoot = "/proc" }) {
  /** Set when the verification of the running launch failed and ended it. */
  let stoppedForVerification = false;

  /**
   * The host service an agent could use to leave the sandbox, when the
   * adapter knows of one and the check is on.
   * @param {{hostService?: string|null}} agent
   */
  function hostServiceFor(agent) {
    if (agent.hostService !== "opencode" || config.hostServiceCheck === "off") {
      return null;
    }
    return detectOpencodeService({ env, procRoot });
  }

  /**
   * Checks the host before a launch: the workspace root exists, the agent
   * binary is on PATH, and no host service offers a way out (refuse, or warn).
   * @param {Record<string, any>} entry
   * @param {Record<string, any>} agent
   * @returns {{hostService: ReturnType<typeof detectOpencodeService>|null, warning: string|null, loopbackOpen: boolean}}
   */
  function preflight(entry, agent) {
    assertLocalPath(entry.localPath);
    if (!findExecutable(agent.command[0], env)) {
      throw new PluginError("config", `The agent command "${agent.command[0]}" was not found on PATH (${env.PATH ?? "unset"}). Install it or set command in a custom agent.`);
    }
    const hostService = hostServiceFor(agent);
    // The service is only a way out when the sandbox that runs the tools may
    // open TCP connections directly; behind nono's proxy, localhost is denied.
    const loopbackOpen = hostService?.running ? toolsReachLoopback(agent) : false;
    const warning = hostService && loopbackOpen ? hostServiceWarning(hostService) : null;
    if (warning && config.hostServiceCheck === "refuse") {
      for (const line of hostServiceRefusal(/** @type {any} */ (hostService), { home: env.HOME || homedir() })) log(line);
      herdr?.notify?.("nono: agent NOT started", `An unsandboxed OpenCode service runs (pid ${hostService?.pid}). Run "opencode service stop", then reconnect.`);
      throw new PluginError("unconfined", `Not started: an unsandboxed OpenCode service runs (pid ${hostService?.pid}); hostServiceCheck is "refuse".`);
    }
    return { hostService, warning, loopbackOpen };
  }

  /**
   * Whether the sandbox that runs the agent's tools (the server sandbox when
   * there is one) may connect to localhost ports directly. A profile nono
   * cannot resolve counts as open, so the host-service check fails safe.
   * @param {{profileRef: string, serverProfileRef?: string|null}} agent
   * @returns {boolean}
   */
  function toolsReachLoopback(agent) {
    try {
      return summarizeProfile(nono.showProfile(agent.serverProfileRef ?? agent.profileRef)).egress === "open";
    } catch (error) {
      log(`could not resolve the profile to check localhost access: ${errorMessageOf(error)}`);
      return true;
    }
  }

  /**
   * Starts the agent's private server in its own nono sandbox, detached from
   * the pane's terminal, and waits until it answers on its port.
   * @param {{entry: Record<string, any>, agent: Record<string, any>, port: number, password: string}} input
   * @returns {Promise<{child: import("node:child_process").ChildProcess, logFile: string}>}
   */
  async function startServer({ entry, agent, port, password }) {
    const logFile = path.join(stateDir, LOGS_DIR, `${entry.sessionName}-server.log`);
    mkdirSync(path.dirname(logFile), { recursive: true });
    const fd = openSync(logFile, "w", 0o600);
    const args = buildRunArgs({ profile: agent.serverProfileRef, sessionName: serverSessionName(entry.sessionName), workspaceRoot: entry.localPath, allowPaths: config.allowPaths, readPaths: config.readPaths, silent: true, extraArgs: [...config.nonoArgs, "--listen-port", String(port)], argv: withPort(agent.server.command, port) });
    let child;
    try {
      // Its own process group: Ctrl-C in the pane must not reach the server.
      child = spawn(nono.bin, args, { cwd: launchCwd(entry), stdio: ["ignore", fd, fd], detached: true, env: { ...sandboxEnv(env, config.agentEnv), [agent.server.passwordEnv]: password } });
    } finally {
      closeSync(fd);
    }
    let exited = false;
    child.once("exit", () => {
      exited = true;
    });
    child.once("error", () => {
      exited = true;
    });
    const raw = Number(env[SERVER_READY_TIMEOUT_ENV]);
    const timeoutMs = Number.isFinite(raw) && raw > 0 ? raw : SERVER_READY_TIMEOUT_MS;
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline && !exited) {
      const status = await probeServer({ port, path: agent.server.readyPath, user: agent.server.user, password });
      if (status === 200) {
        return { child, logFile };
      }
      await sleep(250);
    }
    await stopServer(child);
    const reason = exited ? "exited before it answered" : `did not answer on port ${port} within ${Math.round(timeoutMs / 1000)}s`;
    throw new PluginError("startup", `${agent.title}'s server ${reason}. Its log is ${logFile}.`, { output: tail(logFile) });
  }

  /**
   * Stops a private server: SIGTERM to its nono supervisor, SIGKILL after a grace period.
   * @param {import("node:child_process").ChildProcess|null} child
   */
  async function stopServer(child) {
    if (!child || child.exitCode !== null || child.signalCode !== null) {
      return;
    }
    const gone = new Promise((resolve) => child.once("exit", resolve));
    try {
      child.kill("SIGTERM");
    } catch {
      return;
    }
    // Unreferenced timers: a pending grace period must not keep the bridge alive.
    const killed = await Promise.race([gone.then(() => true), sleep(SERVER_STOP_GRACE_MS, false, { ref: false })]);
    if (!killed) {
      try {
        child.kill("SIGKILL");
      } catch {
        // Already gone.
      }
      await Promise.race([gone, sleep(1000, undefined, { ref: false })]);
    }
  }

  /**
   * Stops server sessions of this mapping that outlived their bridge (a
   * bridge killed with SIGKILL cannot take its server down).
   * @param {Record<string, any>} entry
   */
  function stopOrphanedServers(entry) {
    let sessions;
    try {
      sessions = nono.listSessions();
    } catch {
      return;
    }
    for (const session of sessionsOf(entry, sessions).servers) {
      try {
        nono.stop(session.sessionId);
        log(`Stopped a leftover server session ${session.sessionId} of ${entry.sessionName}.`);
      } catch (error) {
        log(`could not stop the leftover server session ${session.sessionId}: ${errorMessageOf(error)}`);
      }
    }
  }

  /**
   * Finds the nono session a supervisor pid belongs to.
   * @param {number} supervisorPid
   */
  function sessionForSupervisor(supervisorPid) {
    try {
      return nono.listSessions().find((session) => session.supervisorPid === supervisorPid) ?? null;
    } catch (error) {
      log(`could not list nono sessions: ${errorMessageOf(error)}`);
      return null;
    }
  }

  /**
   * Launches the agent in the pane and returns once it exits. The agent runs
   * under `nono run` with the terminal inherited; while it starts, the bridge
   * checks from outside that every process of the session is confined and
   * records the result (and shows a Herdr toast when it is not).
   * @param {string} paneId
   * @param {{mode?: "start"|"connect"}} [options]
   * @returns {Promise<{exitCode: number, entry: Record<string, any>|null}>}
   */
  async function launch(paneId, { mode = "start" } = {}) {
    const entry = requirePaneEntry(stateDir, paneId);
    const agent = agentForEntry(config, entry, pluginRoot);
    let checked;
    try {
      checked = preflight(entry, agent);
    } catch (error) {
      updatePaneEntry(stateDir, paneId, { lifecycleState: "failed", lastError: describeError(error) });
      throw error;
    }
    if (checked.warning) {
      log(`warning: ${checked.warning}`);
      herdr?.notify?.("nono: host service reachable", "An unsandboxed OpenCode service is running; see the pane or run doctor.");
    }
    const cwd = launchCwd(entry);
    let port = null;
    let server = null;
    const password = randomBytes(24).toString("hex");
    if (agent.server) {
      stopOrphanedServers(entry);
      port = await pickFreePort();
    }
    const argv = withPort(mode === "connect" ? agent.resumeArgv : agent.launchArgv, port);
    const args = buildRunArgs({ profile: agent.profileRef, sessionName: entry.sessionName, workspaceRoot: entry.localPath, allowPaths: config.allowPaths, readPaths: config.readPaths, silent: config.silent, extraArgs: [...config.nonoArgs, ...(port === null ? [] : ["--open-port", String(port)])], argv });
    updatePaneEntry(stateDir, paneId, { lifecycleState: "starting", lastError: null, verification: null, profile: agent.profileRef, serverProfile: agent.serverProfileRef, port, launchArgv: argv, launchCount: (entry.launchCount ?? 0) + 1, lastLaunchAt: new Date().toISOString() });
    if (agent.server) {
      log(`Starting ${agent.title}'s server in nono sandbox ${serverSessionName(entry.sessionName)} (profile ${agent.serverProfileRef}, port ${port})...`);
      try {
        server = await startServer({ entry, agent, port: /** @type {number} */ (port), password });
      } catch (error) {
        updatePaneEntry(stateDir, paneId, { lifecycleState: "failed", lastError: describeError(error) });
        throw error;
      }
      updatePaneEntry(stateDir, paneId, { serverSupervisorPid: server.child.pid ?? null, serverToken: server.child.pid ? processStartToken(server.child.pid) : null, serverLog: server.logFile });
    }
    log(`Launching ${agent.title} in nono sandbox ${entry.sessionName} (profile ${agent.profileRef}, read-write ${entry.localPath})...`);
    const report = config.reportAgentStatus && herdr && !agent.herdrDetectionKind;
    const identity = { paneId, source: AGENT_REPORT_SOURCE, agent: agent.kind };
    if (report) {
      try {
        herdr.reportAgent({ ...identity, state: "unknown", message: `${agent.title} in nono sandbox ${entry.sessionName}` });
      } catch (error) {
        log(`could not report the agent to Herdr: ${errorMessageOf(error)}`);
      }
    }
    let child;
    try {
      child = spawn(nono.bin, args, { cwd, stdio: "inherit", env: { ...sandboxEnv(env, config.agentEnv), ...(agent.server ? { [agent.server.passwordEnv]: password } : {}) } });
    } catch (error) {
      await stopServer(server?.child ?? null);
      const failure = new PluginError("startup", `Could not start nono ("${nono.bin}"): ${errorMessageOf(error)}`, { cause: error });
      updatePaneEntry(stateDir, paneId, { lifecycleState: "failed", lastError: describeError(failure) });
      throw failure;
    }
    const exited = new Promise((resolve) => {
      child.once("error", (error) => resolve({ code: null, signal: null, error }));
      child.once("exit", (code, signal) => resolve({ code, signal, error: null }));
    });
    // The terminal sends Ctrl-C and Ctrl-\ to the whole foreground group, so
    // nono already gets them; the bridge must survive them to record the exit.
    const ignore = () => {};
    const forward = (signal) => {
      try {
        child.kill(signal);
      } catch {
        // Already gone.
      }
    };
    process.on("SIGINT", ignore);
    process.on("SIGQUIT", ignore);
    process.on("SIGTERM", forward);
    process.on("SIGHUP", forward);
    let running = true;
    stoppedForVerification = false;
    const supervisorPid = child.pid ?? null;
    if (supervisorPid) {
      updatePaneEntry(stateDir, paneId, { lifecycleState: "running", supervisorPid, supervisorToken: processStartToken(supervisorPid), sessionId: null });
    }
    const terminate = () => forward("SIGTERM");
    const serverSupervisorPid = server?.child.pid ?? null;
    const verification = supervisorPid ? verifyWhileStarting({ paneId, agent, supervisorPid, serverSupervisorPid, port, hostService: checked.hostService, hostServiceReachable: checked.loopbackOpen, isRunning: () => running, terminate }) : Promise.resolve(null);
    let outcome;
    try {
      outcome = /** @type {{code: number|null, signal: string|null, error: Error|null}} */ (await exited);
    } finally {
      running = false;
      process.off("SIGINT", ignore);
      process.off("SIGQUIT", ignore);
      process.off("SIGTERM", forward);
      process.off("SIGHUP", forward);
      process.stdout.write(TERMINAL_RESTORE_SEQUENCE);
      if (report) {
        try {
          herdr.releaseAgent(identity);
        } catch (error) {
          log(`could not release the agent in Herdr: ${errorMessageOf(error)}`);
        }
      }
    }
    await verification;
    await stopServer(server?.child ?? null);
    if (server) {
      updatePaneEntry(stateDir, paneId, { serverSupervisorPid: null, serverToken: null });
    }
    if (outcome.error) {
      const failure = new PluginError(/** @type {any} */ (outcome.error).code === "ENOENT" ? "startup" : "unknown", `Could not run nono ("${nono.bin}"): ${outcome.error.message}. Install nono or set nonoBin in config.json.`, { cause: outcome.error });
      updatePaneEntry(stateDir, paneId, { lifecycleState: "failed", lastError: describeError(failure), supervisorPid: null, supervisorToken: null });
      throw failure;
    }
    const exitCode = outcome.code ?? (outcome.signal === "SIGINT" ? 130 : 128 + 15);
    if (stoppedForVerification) {
      const report = getPaneEntry(stateDir, paneId)?.verification;
      const error = new PluginError("unconfined", `The sandbox verification failed, so ${agent.title} was stopped: ${(report?.problems ?? []).join(" ") || "see info"}`);
      updatePaneEntry(stateDir, paneId, { lifecycleState: "failed", lastExitCode: exitCode, exitedAt: new Date().toISOString(), supervisorPid: null, supervisorToken: null, lastError: describeError(error) });
      log(`error: ${error.message}`);
      log('Run verify-sandbox after reconnect to see the process tree, or set "onVerificationFailure": "warn" to keep such sessions running.');
      // The agent may well have exited cleanly on SIGTERM; the launch still failed.
      return { exitCode: exitCode === 0 ? 1 : exitCode, entry: getPaneEntry(stateDir, paneId) };
    }
    updatePaneEntry(stateDir, paneId, { lifecycleState: "exited", lastExitCode: exitCode, exitedAt: new Date().toISOString(), supervisorPid: null, supervisorToken: null });
    log(`${agent.title} exited with code ${exitCode}. Use reconnect to resume it in the sandbox, or open-shell for a shell with the same policy.`);
    return { exitCode, entry: getPaneEntry(stateDir, paneId) };
  }

  /**
   * Verifies a freshly started session until it passes or the window ends,
   * records the report on the mapping, and on failure raises a toast and,
   * with `onVerificationFailure: "stop"`, ends the session: an agent whose
   * server or tools are not confined must not keep running. The agent's
   * server needs a moment to start, so early misses are retried.
   * @param {{paneId: string, agent: Record<string, any>, supervisorPid: number, serverSupervisorPid: number|null, port: number|null, hostService: any, hostServiceReachable: boolean, isRunning: () => boolean, terminate: () => void}} input
   */
  async function verifyWhileStarting({ paneId, agent, supervisorPid, serverSupervisorPid, port, hostService, hostServiceReachable, isRunning, terminate }) {
    if (!config.verifyAfterStart || !procfsAvailable(procRoot)) {
      return null;
    }
    const deadline = Date.now() + verifyWindow(env);
    let report = null;
    let sessionId = null;
    while (isRunning()) {
      await sleep(VERIFY_INTERVAL_MS);
      if (!isRunning()) break;
      if (!sessionId) {
        sessionId = sessionForSupervisor(supervisorPid)?.sessionId ?? null;
      }
      report = verifySession({ supervisorPid, serverSupervisorPid, port, agent, hostService, hostServiceReachable, procRoot });
      if (report.ok || Date.now() >= deadline) break;
    }
    if (!report) {
      return null;
    }
    try {
      updatePaneEntry(stateDir, paneId, { verification: report, ...(sessionId ? { sessionId } : {}) });
    } catch (error) {
      log(`could not record the verification: ${errorMessageOf(error)}`);
    }
    if (report.ok === false && isRunning()) {
      const stopping = config.onVerificationFailure === "stop";
      herdr?.notify?.("nono sandbox verification FAILED", `${summarizeVerification(report)}${stopping ? " The agent was stopped." : ""}`);
      if (stopping) {
        stoppedForVerification = true;
        terminate();
      }
    }
    return report;
  }

  /**
   * Opens an interactive shell in a new nono sandbox with the agent's policy
   * and workspace grant. It is a second sandbox, not the agent's own: nono
   * sandboxes are processes and cannot be entered from outside.
   * @param {string} paneId
   * @returns {Promise<{exitCode: number}>}
   */
  async function shell(paneId) {
    acknowledgeShell(paneId);
    try {
      const entry = requirePaneEntry(stateDir, paneId);
      const agent = agentForEntry(config, entry, pluginRoot);
      assertLocalPath(entry.localPath);
      const launchShell = shellLaunch(config.shell, entry.sessionName);
      // The shell gets the policy the agent's tools run under: the server's, when there is one.
      const profile = agent.serverProfileRef ?? agent.profileRef;
      const args = buildRunArgs({ profile, sessionName: shellSessionName(entry.sessionName), workspaceRoot: entry.localPath, allowPaths: config.allowPaths, readPaths: config.readPaths, silent: config.silent, extraArgs: config.nonoArgs, argv: launchShell.argv });
      log(`Opening ${config.shell} in a nono sandbox with the policy ${entry.sessionName}'s tools run under (profile ${profile})...`);
      const child = spawn(nono.bin, args, { cwd: launchCwd(entry), stdio: "inherit", env: { ...sandboxEnv(env, config.agentEnv), ...launchShell.env } });
      const ignore = () => {};
      process.on("SIGINT", ignore);
      process.on("SIGQUIT", ignore);
      let outcome;
      try {
        outcome = await new Promise((resolve) => {
          child.once("error", (error) => resolve({ code: null, error }));
          child.once("exit", (code) => resolve({ code, error: null }));
        });
      } finally {
        process.off("SIGINT", ignore);
        process.off("SIGQUIT", ignore);
        process.stdout.write(TERMINAL_RESTORE_SEQUENCE);
      }
      if (outcome.error) {
        throw new PluginError("startup", `Could not run nono ("${nono.bin}"): ${outcome.error.message}`, { cause: outcome.error });
      }
      log(`Shell exited with code ${outcome.code ?? 1}.`);
      return { exitCode: outcome.code ?? 1 };
    } finally {
      releaseShell(paneId);
    }
  }

  /**
   * Records this process as an open-shell session of the mapping.
   * @param {string} paneId
   */
  function acknowledgeShell(paneId) {
    withPaneLock(stateDir, paneId, () => {
      const entry = requirePaneEntry(stateDir, paneId);
      const shells = liveShells(entry).filter((item) => item.pid !== process.pid);
      shells.push({ pid: process.pid, token: ownStartToken(), since: new Date().toISOString(), paneId });
      updatePaneEntry(stateDir, paneId, { shellPids: shells });
    });
  }

  /**
   * Removes this process from the mapping's open-shell sessions.
   * @param {string} paneId
   */
  function releaseShell(paneId) {
    try {
      withPaneLock(stateDir, paneId, () => {
        const entry = getPaneEntry(stateDir, paneId);
        if (entry) {
          updatePaneEntry(stateDir, paneId, { shellPids: (entry.shellPids ?? []).filter((item) => item.pid !== process.pid) });
        }
      });
    } catch (error) {
      log(`Could not release the shell record for pane ${paneId}: ${errorMessageOf(error)}`);
    }
  }

  /**
   * Records that a bridge process started for a pane. The action that typed
   * the bridge command waits for the launch id it generated, so an older
   * acknowledgement can never satisfy a newer launch.
   * @param {string} paneId
   * @param {string|null} [launchId]
   */
  function acknowledgeBridge(paneId, launchId = null) {
    withPaneLock(stateDir, paneId, () => {
      const entry = requirePaneEntry(stateDir, paneId);
      if (bridgeIsRunning(entry) && entry.bridgePid !== process.pid) {
        throw new PluginError("conflict", `Pane ${paneId} already runs a bridge for ${entry.sessionName} (pid ${entry.bridgePid}, since ${entry.bridgeStartedAt}); not starting a second agent. Exit that agent first, or use another pane.`);
      }
      updatePaneEntry(stateDir, paneId, { bridgeStartedAt: new Date().toISOString(), bridgeLaunchId: launchId, bridgePid: process.pid, bridgeToken: ownStartToken() });
    });
  }

  /**
   * Clears this process's ownership of a mapping when the bridge exits.
   * @param {string} paneId
   */
  function releaseBridge(paneId) {
    try {
      withPaneLock(stateDir, paneId, () => {
        const entry = getPaneEntry(stateDir, paneId);
        if (!entry || entry.bridgePid !== process.pid) {
          return;
        }
        updatePaneEntry(stateDir, paneId, { bridgePid: null, bridgeToken: null, bridgeExitedAt: new Date().toISOString() });
      });
    } catch (error) {
      log(`Could not release the mapping for pane ${paneId} on exit: ${errorMessageOf(error)}`);
    }
  }

  /**
   * The live nono sessions of a mapping: its agent session (matched by
   * supervisor pid, else by name), its private server sessions and its
   * open-shell sessions (by name).
   * @param {Record<string, any>} entry
   * @param {ReturnType<ReturnType<typeof createNonoClient>["listSessions"]>} sessions
   */
  function sessionsOf(entry, sessions) {
    const running = sessions.filter((session) => session.status !== "exited");
    const agentSession = running.find((session) => entry.supervisorPid && session.supervisorPid === entry.supervisorPid)
      ?? running.find((session) => session.name === entry.sessionName)
      ?? null;
    const shells = running.filter((session) => session.name === shellSessionName(entry.sessionName));
    const servers = running.filter((session) => session.name === serverSessionName(entry.sessionName));
    return { agentSession, shells, servers };
  }

  /**
   * Stops the agent's running nono session (SIGTERM, SIGKILL after nono's
   * grace period) and waits for the bridge to record the exit.
   * @param {string} paneId
   * @param {{force?: boolean}} [options]
   */
  async function stop(paneId, { force = false } = {}) {
    const entry = requirePaneEntry(stateDir, paneId);
    const { agentSession, servers } = sessionsOf(entry, nono.listSessions());
    if (!agentSession && servers.length === 0) {
      throw new PluginError("not-found", `No nono session is running for ${entry.sessionName}; nothing to stop.`);
    }
    if (agentSession) {
      nono.stop(agentSession.sessionId, { force });
    }
    const deadline = Date.now() + STOP_WAIT_MS;
    while (Date.now() < deadline && bridgeIsRunning(getPaneEntry(stateDir, paneId) ?? {})) {
      await sleep(200);
    }
    // The bridge takes its server down when the client exits; a server whose bridge is gone is stopped here.
    const stoppedServers = [];
    for (const session of sessionsOf(entry, nono.listSessions()).servers) {
      try {
        nono.stop(session.sessionId, { force });
        stoppedServers.push(session.sessionId);
      } catch (error) {
        if (errorKindOf(error) !== "not-found") throw error;
      }
    }
    return { sessionName: entry.sessionName, sessionId: agentSession?.sessionId ?? null, serverSessionIds: stoppedServers };
  }

  /**
   * Verifies the mapping's running session now and records the report.
   * @param {string} paneId
   */
  function verify(paneId) {
    const entry = requirePaneEntry(stateDir, paneId);
    const agent = agentForEntry(config, entry, pluginRoot);
    let supervisorPid = entry.supervisorPid ?? null;
    if (supervisorPid && entry.supervisorToken && processStartToken(supervisorPid) !== entry.supervisorToken) {
      supervisorPid = null;
    }
    if (!supervisorPid) {
      // A session started by an older bridge, or one whose mapping missed the pid: ask nono.
      try {
        supervisorPid = sessionsOf(entry, nono.listSessions()).agentSession?.supervisorPid ?? null;
      } catch (error) {
        log(`could not list nono sessions: ${errorMessageOf(error)}`);
      }
    }
    if (!supervisorPid) {
      throw new PluginError("not-found", `No nono session is running for ${entry.sessionName}; start or reconnect the agent, then verify.`);
    }
    let serverSupervisorPid = entry.serverSupervisorPid ?? null;
    if (serverSupervisorPid && entry.serverToken && processStartToken(serverSupervisorPid) !== entry.serverToken) {
      serverSupervisorPid = null;
    }
    if (agent.server && !serverSupervisorPid) {
      try {
        serverSupervisorPid = sessionsOf(entry, nono.listSessions()).servers[0]?.supervisorPid ?? null;
      } catch (error) {
        log(`could not list nono sessions: ${errorMessageOf(error)}`);
      }
    }
    const hostService = hostServiceFor(agent);
    const report = verifySession({ supervisorPid, serverSupervisorPid, port: entry.port ?? null, agent, hostService, hostServiceReachable: hostService?.running ? toolsReachLoopback(agent) : false, procRoot });
    updatePaneEntry(stateDir, paneId, { verification: report });
    return { entry: getPaneEntry(stateDir, paneId), agent, report };
  }

  /**
   * Describes a mapping together with its live sessions when nono answers.
   * @param {string} paneId
   */
  function describe(paneId) {
    const entry = requirePaneEntry(stateDir, paneId);
    const agent = agentForEntry(config, entry, pluginRoot);
    let sessions = null;
    let sessionError = null;
    try {
      const { agentSession, shells, servers } = sessionsOf(entry, nono.listSessions());
      sessions = { agent: agentSession, server: servers[0] ?? null, shells };
    } catch (error) {
      sessionError = describeError(error);
    }
    return {
      mapping: entry,
      agent: { kind: agent.kind, title: agent.title, profile: agent.profileRef, serverProfile: agent.serverProfileRef, launchArgv: agent.launchArgv, resumeArgv: agent.resumeArgv, serverArgv: agent.server?.command ?? null, herdrDetectionKind: agent.herdrDetectionKind },
      sessions,
      sessionError,
      verification: entry.verification ?? null,
    };
  }

  /**
   * Lists every mapping with the state of its agent session.
   */
  function listAll() {
    const state = loadState(stateDir);
    let sessions = null;
    let sessionError = null;
    try {
      sessions = nono.listSessions();
    } catch (error) {
      sessionError = describeError(error);
    }
    const mappings = Object.values(state.panes).map((entry) => {
      const live = sessions ? sessionsOf(entry, sessions) : null;
      return {
        paneId: entry.paneId,
        sessionName: entry.sessionName,
        agentKind: entry.agentKind,
        localPath: entry.localPath,
        workdir: entry.workdir ?? entry.localPath,
        lifecycleState: entry.lifecycleState,
        running: live ? live.agentSession !== null : null,
        sessionId: live?.agentSession?.sessionId ?? null,
        serverRunning: live ? live.servers.length > 0 : null,
        shells: live ? live.shells.length : null,
        verification: entry.verification ? summarizeVerification(entry.verification) : null,
        verified: entry.verification?.ok ?? null,
      };
    });
    return { mappings, sessionError };
  }

  /**
   * Drops every mapping whose Herdr pane is gone and whose agent and shells
   * are not running. Each candidate is decided again under its lock, so a
   * mapping rewritten since the snapshot, or one whose bridge or shell still
   * runs (the pane closed under a live agent), stays.
   * @param {Iterable<string>} livePaneIds Every pane id Herdr knows right now.
   * @returns {{pruned: Array<{paneId: string, sessionName: string}>, kept: Array<{paneId: string, sessionName: string, reason: string}>}}
   */
  function prune(livePaneIds) {
    const paneIds = new Set(livePaneIds);
    const pruned = [];
    const kept = [];
    for (const entry of Object.values(loadState(stateDir).panes)) {
      if (paneIds.has(entry.paneId)) {
        continue;
      }
      const outcome = withPaneLock(stateDir, entry.paneId, () => {
        const now = getPaneEntry(stateDir, entry.paneId);
        if (now && (bridgeIsRunning(now) || shellIsRunning(now))) {
          return "busy";
        }
        return deletePaneEntryIfUnchanged(stateDir, entry.paneId, entry) ? "pruned" : "changed";
      });
      if (outcome === "pruned") {
        pruned.push({ paneId: entry.paneId, sessionName: entry.sessionName });
      } else {
        kept.push({ paneId: entry.paneId, sessionName: entry.sessionName, reason: outcome === "busy" ? "its agent or shell still runs; stop it first" : "mapping changed while pruning; run prune-mappings again" });
      }
    }
    return { pruned, kept };
  }

  return { launch, shell, stop, verify, describe, listAll, prune, acknowledgeBridge, releaseBridge, acknowledgeShell, releaseShell, sessionsOf, hostServiceFor, toolsReachLoopback };
}

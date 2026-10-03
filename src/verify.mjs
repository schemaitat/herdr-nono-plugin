/**
 * Proves, from the host's side, that a launched agent runs confined: every
 * process under the nono supervisors (the client's and, for agents with a
 * server sandbox, the server's) carries `no_new_privs` and nono's
 * `NONO_CAP_FILE` marker, the agent's own server is one of those processes,
 * the client was started with the arguments that point it at that server, and
 * no sandboxed process holds a connection to an unsandboxed host service.
 * Linux only: it reads /proc.
 * @module verify
 */
import path from "node:path";
import { withPort } from "./agents.mjs";
import { descendants, procfsAvailable, readProcess, socketInodes, TCP_ESTABLISHED, tcpConnections } from "./procfs.mjs";
import { hostServiceWarning } from "./hostservice.mjs";

const LOOPBACK = new Set(["127.0.0.1", "::1", "0.0.0.0", "::"]);

function commandText(argv) {
  const text = argv.join(" ");
  return text.length > 200 ? `${text.slice(0, 197)}...` : text;
}

/**
 * Whether a process shows the marks of a nono-confined process. A missing
 * `NONO_CAP_FILE` only counts against it when the environment was readable.
 * @param {import("./procfs.mjs").ProcessInfo} info
 * @returns {boolean}
 */
export function looksConfined(info) {
  return info.noNewPrivs === true && info.nonoCapFile !== false;
}

/**
 * Finds the agent's client process in the sandboxed tree: the first process
 * whose executable, or script for interpreters, is the adapter's command.
 * @param {import("./procfs.mjs").ProcessInfo[]} tree
 * @param {string} command
 * @returns {import("./procfs.mjs").ProcessInfo|null}
 */
export function findClient(tree, command) {
  const wanted = path.basename(command);
  return tree.find((info) => info.argv.slice(0, 2).some((word) => path.basename(word) === wanted)) ?? null;
}

/**
 * Verifies one running launch.
 * @param {{supervisorPid: number|null, serverSupervisorPid?: number|null, port?: number|null, agent: {command: string[], requiredArgs?: string[], serverPattern?: string|null, title?: string, server?: object|null}, hostService?: ReturnType<typeof import("./hostservice.mjs").detectOpencodeService>|null, hostServiceReachable?: boolean, procRoot?: string}} input `hostServiceReachable` says whether the tools' sandbox may connect to localhost directly; only then is a running host service a warning.
 */
export function verifySession({ supervisorPid, serverSupervisorPid = null, port = null, agent, hostService = null, hostServiceReachable = true, procRoot = "/proc" }) {
  const report = {
    checkedAt: new Date().toISOString(),
    supported: procfsAvailable(procRoot),
    ok: /** @type {boolean|null} */ (null),
    supervisorPid,
    serverSupervisorPid,
    port,
    running: false,
    processes: /** @type {Array<{pid: number, ppid: number|null, sandbox: "client"|"server", role: string, confined: boolean, noNewPrivs: boolean|null, nonoCapFile: boolean|null, command: string}>} */ ([]),
    client: /** @type {{pid: number, command: string, requiredArgs: string[], missingArgs: string[]}|null} */ (null),
    server: /** @type {{pattern: string, pid: number|null, command: string|null}|null} */ (null),
    hostConnections: /** @type {Array<{pid: number, remote: string}>} */ ([]),
    hostService: hostService ? { running: hostService.running, pid: hostService.pid, url: hostService.url, sandboxed: hostService.sandboxed, reachable: Boolean(hostService.running) && hostServiceReachable } : null,
    problems: /** @type {string[]} */ ([]),
    warnings: /** @type {string[]} */ ([]),
  };
  const warning = hostService && hostServiceReachable ? hostServiceWarning(hostService) : null;
  if (warning) {
    report.warnings.push(warning);
  }
  if (!report.supported) {
    report.warnings.push("This system has no /proc, so the process tree cannot be inspected; verification only runs on Linux.");
    return report;
  }
  const alive = (pid) => Number.isInteger(pid) && pid !== null && readProcess(pid, { root: procRoot }) !== null;
  if (!alive(supervisorPid)) {
    report.problems.push("The nono session is not running, so there is nothing to verify.");
    report.ok = false;
    return report;
  }
  report.running = true;
  const clientTree = descendants(/** @type {number} */ (supervisorPid), { root: procRoot });
  if (clientTree.length === 0) {
    report.problems.push(`The nono supervisor (pid ${supervisorPid}) has not started the sandboxed process yet.`);
  }
  let serverTree = [];
  if (agent.server) {
    if (!alive(serverSupervisorPid)) {
      report.problems.push("The agent's server sandbox is not running.");
    } else {
      serverTree = descendants(/** @type {number} */ (serverSupervisorPid), { root: procRoot });
      if (serverTree.length === 0) {
        report.problems.push(`The server's nono supervisor (pid ${serverSupervisorPid}) has not started the server yet.`);
      }
    }
  }
  const serverRegex = agent.serverPattern ? new RegExp(withPort(agent.serverPattern, port)) : null;
  const client = findClient(clientTree, agent.command[0]);
  const serverHome = agent.server ? serverTree : clientTree;
  const server = serverRegex ? serverHome.find((info) => serverRegex.test(info.argv.join(" "))) ?? null : null;
  for (const [sandbox, tree] of /** @type {Array<["client"|"server", import("./procfs.mjs").ProcessInfo[]]>} */ ([["client", clientTree], ["server", serverTree]])) {
    for (const info of tree) {
      const confined = looksConfined(info);
      const role = info === server ? "server" : info === client ? "client" : sandbox === "server" ? "tool" : "child";
      report.processes.push({ pid: info.pid, ppid: info.ppid, sandbox, role, confined, noNewPrivs: info.noNewPrivs, nonoCapFile: info.nonoCapFile, command: commandText(info.argv) });
      if (!confined) {
        report.problems.push(`Process ${info.pid} (${commandText(info.argv)}) under the ${sandbox} sandbox's supervisor is not confined (no_new_privs ${info.noNewPrivs}, NONO_CAP_FILE ${info.nonoCapFile}).`);
      }
    }
  }
  if (clientTree.length > 0) {
    const required = withPort(agent.requiredArgs ?? [], port);
    if (!client) {
      report.problems.push(`No ${path.basename(agent.command[0])} process runs inside the sandbox.`);
    } else {
      const missingArgs = required.filter((arg) => !client.argv.includes(arg));
      report.client = { pid: client.pid, command: commandText(client.argv), requiredArgs: required, missingArgs };
      if (missingArgs.length > 0) {
        report.problems.push(`The ${agent.title ?? agent.command[0]} client (pid ${client.pid}) was started without ${missingArgs.join(" ")}, so its server may run outside the sandbox.`);
      }
    }
  }
  if (serverRegex) {
    report.server = { pattern: withPort(agent.serverPattern ?? "", port), pid: server?.pid ?? null, command: server ? commandText(server.argv) : null };
    if (!server) {
      report.problems.push(`No process matching /${report.server.pattern}/ (the agent's own server) runs inside the ${agent.server ? "server " : ""}sandbox.`);
    }
  }
  if (hostService?.running && hostService.port) {
    const connections = tcpConnections({ root: procRoot });
    for (const info of [...clientTree, ...serverTree]) {
      for (const inode of socketInodes(info.pid, { root: procRoot }) ?? []) {
        const connection = connections.get(inode);
        if (connection && connection.state === TCP_ESTABLISHED && connection.remote.port === hostService.port && LOOPBACK.has(connection.remote.address)) {
          report.hostConnections.push({ pid: info.pid, remote: `${connection.remote.address}:${connection.remote.port}` });
          report.problems.push(`Sandboxed process ${info.pid} (${commandText(info.argv)}) is connected to the host service at ${connection.remote.address}:${connection.remote.port}.`);
        }
      }
    }
  }
  report.ok = report.problems.length === 0;
  return report;
}

/**
 * One-line summary of a verification report for panes, toasts and tables.
 * @param {ReturnType<typeof verifySession>|null|undefined} report
 * @returns {string}
 */
export function summarizeVerification(report) {
  if (!report) return "not verified";
  if (!report.supported) return "unsupported (no /proc)";
  if (report.ok) {
    const server = report.server?.pid ? `, server pid ${report.server.pid}` : "";
    return `confined: ${report.processes.length} process${report.processes.length === 1 ? "" : "es"}${server}`;
  }
  return `FAILED: ${report.problems[0] ?? "unknown problem"}`;
}

/**
 * Thin wrapper around the nono CLI. Every call is a direct argv (never a shell
 * string). Captured calls are killed after a timeout so a wedged nono cannot
 * hang an action; the interactive `nono run` lives in the bridge.
 * @module nono
 */
import { spawnSync } from "node:child_process";
import { isIP } from "node:net";
import { NONO_CALL_TIMEOUT_ENV, NONO_CALL_TIMEOUT_MS } from "./constants.mjs";
import { PluginError } from "./errors.mjs";

const MAX_BUFFER = 16 * 1024 * 1024;

/**
 * Maps nono's error wording to an error kind. nono's exit codes are not
 * documented, so the output decides; anything unrecognised is `unknown` and
 * carries the output.
 * @param {string} output
 * @returns {string}
 */
export function classifyFailure(output) {
  const text = String(output ?? "");
  if (/session not found|profile not found|profile file not found|no such file|not found/i.test(text)) return "not-found";
  if (/permission denied|operation not permitted|EACCES|EPERM/i.test(text)) return "permission";
  if (/invalid|unexpected argument|unrecognized|parse error|validation/i.test(text)) return "config";
  return "unknown";
}

/**
 * Normalises one `nono ps --json` record into the fields the plugin uses.
 * @param {Record<string, any>} raw
 * @returns {{sessionId: string, name: string|null, supervisorPid: number|null, childPid: number|null, status: string|null, attachment: string|null, exitCode: number|null, command: string[], profile: string|null, workdir: string|null, network: string|null, started: string|null}}
 */
export function normalizeSession(raw) {
  const pid = (value) => (Number.isInteger(value) && value > 0 ? value : null);
  return {
    sessionId: String(raw.session_id ?? raw.id ?? ""),
    name: typeof raw.name === "string" ? raw.name : null,
    supervisorPid: pid(raw.supervisor_pid),
    childPid: pid(raw.child_pid),
    status: typeof raw.status === "string" ? raw.status : null,
    attachment: typeof raw.attachment === "string" ? raw.attachment : null,
    exitCode: Number.isInteger(raw.exit_code) ? raw.exit_code : null,
    command: Array.isArray(raw.command) ? raw.command.map(String) : [],
    profile: typeof raw.profile === "string" ? raw.profile : null,
    workdir: typeof raw.workdir === "string" ? raw.workdir : null,
    network: typeof raw.network === "string" ? raw.network : null,
    started: typeof raw.started === "string" ? raw.started : null,
  };
}

/**
 * Parses `nono ps --json` output. An empty output or `null` is an empty list;
 * anything that is not an array of objects is an error rather than "no
 * sessions", so a format change cannot make running agents look stopped.
 * @param {unknown} parsed
 */
export function normalizeSessionList(parsed) {
  if (parsed === null || parsed === undefined) {
    return [];
  }
  const list = Array.isArray(parsed) ? parsed : Array.isArray(/** @type {any} */ (parsed)?.sessions) ? /** @type {any} */ (parsed).sessions : null;
  if (!list || !list.every((item) => item && typeof item === "object")) {
    throw new PluginError("unknown", "nono ps --json printed an unexpected shape.", { output: JSON.stringify(parsed).slice(0, 2000) });
  }
  return list.map(normalizeSession).filter((session) => session.sessionId !== "");
}

/**
 * Builds the `nono run` argv for one launch. The workspace root is granted
 * read-write with `--allow` (never `--allow-cwd`, which would grant whatever
 * directory the pane happens to be in); the agent command follows `--`.
 * @param {{profile: string, sessionName: string, workspaceRoot: string, allowPaths?: string[], readPaths?: string[], silent?: boolean, extraArgs?: string[], argv: string[]}} input
 * @returns {string[]}
 */
export function buildRunArgs({ profile, sessionName, workspaceRoot, allowPaths = [], readPaths = [], silent = false, extraArgs = [], argv }) {
  if (!Array.isArray(argv) || argv.length === 0) {
    throw new PluginError("startup", "buildRunArgs needs the command to run inside the sandbox.");
  }
  const args = ["run"];
  if (silent) args.push("--silent");
  args.push("--profile", profile, "--name", sessionName, "--allow", workspaceRoot);
  for (const dir of allowPaths) args.push("--allow", dir);
  for (const dir of readPaths) args.push("--read", dir);
  args.push(...extraArgs, "--", ...argv);
  return args;
}

/**
 * Public wildcard DNS services whose names resolve to any address written in
 * them, including 127.0.0.1 (`127.0.0.1.nip.io`, `a.localtest.me`).
 */
export const WILDCARD_DNS_DOMAINS = Object.freeze(["nip.io", "sslip.io", "xip.io", "localtest.me", "lvh.me", "vcap.me", "lacolhost.com", "localho.st", "traefik.me", "fuf.me", "localhost.direct"]);

/** Name suffixes that resolve on the local network or the host itself. */
const LOCAL_SUFFIXES = Object.freeze(["localhost", "localdomain", "local", "internal", "lan", "home.arpa", "home", "corp"]);

/**
 * The host name of one `allow_domain` entry: a hostname, a `*.` wildcard, or
 * a URL with a path glob (`https://github.com/org/**`).
 * @param {string} entry
 * @returns {string}
 */
export function allowDomainHost(entry) {
  let host = String(entry).trim().toLowerCase();
  if (host.includes("://")) {
    try {
      host = new URL(host.replace("://*.", "://wildcard-placeholder.")).hostname.replace(/^wildcard-placeholder\./, "*.");
    } catch {
      host = host.split("://")[1] ?? host;
    }
  }
  host = host.split("/")[0];
  if (host.startsWith("[")) return host.slice(1, host.indexOf("]") === -1 ? undefined : host.indexOf("]"));
  if ((host.match(/:/g) ?? []).length === 1) host = host.split(":")[0];
  return host.replace(/\.$/, "");
}

/**
 * Why an `allow_domain` entry can reach services on the host's loopback
 * interface through nono's proxy, or null when it cannot. nono's proxy
 * resolves an allowed name and connects to whatever address it gets, and does
 * not filter loopback or private addresses, so a name that resolves to the
 * host is as good as allowing 127.0.0.1. An entry is unsafe when it is a
 * catch-all wildcard, an IP address, a loopback or local-network name, a
 * single-label name (`/etc/hosts` often maps the host name to 127.0.1.1), or
 * a name under a wildcard DNS service.
 * @param {string} entry
 * @returns {string|null}
 */
export function loopbackReason(entry) {
  const host = allowDomainHost(entry);
  if (host === "" || host === "*") return "allows every host, including localhost";
  // Shorthand forms such as 127.1 or 0x7f.1 are IP addresses to the resolver.
  if (isIP(host) !== 0 || /^(0x[0-9a-f]+|\d+)(\.(0x[0-9a-f]+|\d+))*$/.test(host)) return "is an IP address, which may be the host itself";
  if (host.includes("*") && !/^\*\.[^*]+$/.test(host)) return "is a wildcard nono's proxy may match against localhost";
  const name = host.replace(/^\*\./, "");
  if (!name.includes(".")) return host.startsWith("*.") ? "covers a whole top-level domain, including names that resolve to localhost" : "is a single-label name, which can resolve to the host itself";
  if (LOCAL_SUFFIXES.some((suffix) => name === suffix || name.endsWith(`.${suffix}`)) || name.startsWith("localhost.")) return "resolves on the host or the local network";
  if (WILDCARD_DNS_DOMAINS.some((domain) => name === domain || name.endsWith(`.${domain}`))) return "is a wildcard DNS service whose names resolve to 127.0.0.1";
  return null;
}

/**
 * The `allow_domain` entries that let a sandbox behind nono's proxy reach
 * localhost, each with the reason.
 * @param {string[]} allowDomains
 * @returns {Array<{domain: string, reason: string}>}
 */
export function loopbackDomains(allowDomains) {
  return allowDomains.flatMap((domain) => {
    const reason = loopbackReason(domain);
    return reason ? [{ domain, reason }] : [];
  });
}

/**
 * A short description of a resolved nono profile (`nono profile show --json`).
 * `egress` is `open` when the sandbox may open TCP connections directly, which
 * includes connections to services on localhost; `allowlist` routes egress
 * through nono's proxy and denies direct connects; `blocked` allows none.
 * `loopback` says whether the sandbox can reach services on localhost: with
 * open egress directly, behind the proxy when an allowed domain can resolve to
 * the host (`loopbackDomains` lists those entries). `readWritePaths` and
 * `readOnlyPaths` are the profile's directory grants, before the plugin adds
 * the workspace root.
 * @param {Record<string, any>} profile
 * @returns {{name: string|null, description: string|null, extends: string[], egress: string, allowDomains: string[], loopback: boolean, loopbackDomains: Array<{domain: string, reason: string}>, afUnixMediation: string, workdirAccess: string|null, readWritePaths: string[], readOnlyPaths: string[]}}
 */
export function summarizeProfile(profile) {
  const network = profile.network ?? {};
  const allowDomains = Array.isArray(network.allow_domain) ? network.allow_domain.map((item) => (typeof item === "string" ? item : item?.domain ?? JSON.stringify(item))) : [];
  let egress = "open";
  if (network.block === true) {
    egress = "blocked";
  } else if (allowDomains.length > 0 || network.network_profile) {
    egress = "allowlist";
  }
  const unsafe = egress === "allowlist" ? loopbackDomains(allowDomains) : [];
  return {
    name: profile.name ?? null,
    description: typeof profile.description === "string" && profile.description !== "" ? profile.description : null,
    extends: [].concat(profile.extends ?? []),
    egress,
    allowDomains,
    loopback: egress === "open" || unsafe.length > 0,
    loopbackDomains: unsafe,
    afUnixMediation: profile.linux?.af_unix_mediation ?? "off",
    workdirAccess: profile.workdir?.access ?? null,
    readWritePaths: Array.isArray(profile.filesystem?.allow) ? profile.filesystem.allow : [],
    readOnlyPaths: Array.isArray(profile.filesystem?.read) ? profile.filesystem.read : [],
  };
}

/**
 * Creates a client bound to one nono executable.
 * @param {{bin?: string, env?: NodeJS.ProcessEnv}} [options]
 */
export function createNonoClient({ bin = "nono", env = process.env } = {}) {
  const configured = Number(env[NONO_CALL_TIMEOUT_ENV]);
  const timeout = Number.isFinite(configured) && configured > 0 ? configured : NONO_CALL_TIMEOUT_MS;

  /**
   * Runs nono with captured output and returns status and output, throwing
   * only when nono cannot be started or times out.
   * @param {string[]} args
   * @returns {{status: number|null, stdout: string, stderr: string, output: string}}
   */
  function run(args) {
    const result = spawnSync(bin, args, { encoding: "utf8", env, maxBuffer: MAX_BUFFER, timeout, killSignal: "SIGKILL" });
    const stdout = result.stdout ?? "";
    const stderr = result.stderr ?? "";
    if (result.error) {
      if (/** @type {any} */ (result.error).code === "ETIMEDOUT") {
        throw new PluginError("unknown", `nono ${args.slice(0, 2).join(" ")} did not finish within ${Math.round(timeout / 1000)}s and was killed (${NONO_CALL_TIMEOUT_ENV} raises the limit).`, { output: stdout + stderr });
      }
      throw new PluginError("startup", `Could not run nono ("${bin}"): ${result.error.message}. Install nono or set nonoBin in config.json.`, { cause: result.error });
    }
    return { status: result.status, stdout, stderr, output: `${stdout}${stderr}` };
  }

  /**
   * Runs nono and throws a classified error on a non-zero exit.
   * @param {string[]} args
   * @param {string} step
   */
  function runChecked(args, step) {
    const result = run(args);
    if (result.status !== 0) {
      const exit = result.status === null ? "a signal" : `exit ${result.status}`;
      throw new PluginError(classifyFailure(result.output), `nono failed while ${step} (${exit}).`, { output: result.output });
    }
    return result;
  }

  /**
   * `nono --version`.
   * @returns {{raw: string, version: string|null}}
   */
  function version() {
    const { stdout } = runChecked(["--version"], "reading its version");
    const match = stdout.match(/(\d+\.\d+\.\d+)/);
    return { raw: stdout.trim(), version: match ? match[1] : null };
  }

  /**
   * Lists sessions (`nono ps --json`), including exited ones with `all`.
   * @param {{all?: boolean}} [options]
   */
  function listSessions({ all = false } = {}) {
    const { stdout } = runChecked(all ? ["ps", "--json", "--all"] : ["ps", "--json"], "listing sessions");
    let parsed;
    try {
      parsed = stdout.trim() === "" ? [] : JSON.parse(stdout);
    } catch (error) {
      throw new PluginError("unknown", `nono ps --json printed invalid JSON: ${error.message}`, { output: stdout.slice(0, 2000) });
    }
    return normalizeSessionList(parsed);
  }

  /**
   * Stops a session by id (`nono stop`), SIGTERM first unless `force`.
   * @param {string} sessionId
   * @param {{force?: boolean}} [options]
   */
  function stop(sessionId, { force = false } = {}) {
    return runChecked(force ? ["stop", "--force", sessionId] : ["stop", sessionId], `stopping session ${sessionId}`);
  }

  /**
   * Resolves a profile (`nono profile show --json`). Throws `not-found` when
   * nono does not know it, which is how doctor validates the configuration.
   * @param {string} profile
   * @returns {Record<string, any>}
   */
  function showProfile(profile) {
    const { stdout } = runChecked(["profile", "show", "--json", profile], `resolving profile ${profile}`);
    try {
      return JSON.parse(stdout);
    } catch (error) {
      throw new PluginError("unknown", `nono profile show --json printed invalid JSON: ${error.message}`, { output: stdout.slice(0, 2000) });
    }
  }

  return { bin, run, runChecked, version, listSessions, stop, showProfile };
}

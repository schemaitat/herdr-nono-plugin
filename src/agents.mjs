/**
 * Agent adapters: which command runs inside the nono sandbox, which nono
 * profile confines it, which label tells Herdr's screen detection what it is
 * looking at, and what the verification looks for in the sandboxed process
 * tree.
 * @module agents
 */
import path from "node:path";
import { PluginError } from "./errors.mjs";

/**
 * Built-in adapters keyed by agent kind.
 *
 * OpenCode 2.x is a client plus a server; the server runs the model loop and
 * every tool call. Plain `opencode` connects to a background service on the
 * host, which would leave the server, and every tool it runs, outside the
 * sandbox. The plugin therefore starts a private server per pane in its own
 * nono sandbox (`server`), where egress goes through nono's proxy so direct
 * connects to localhost services are denied, and the TUI client in a second,
 * network-blocked sandbox that may reach only that server's port. `{port}` is
 * replaced with the port the plugin picks for each launch. The client's
 * `--server` argument is required, so `agentArgs` cannot point it elsewhere,
 * and the verification looks for the server process inside the server
 * sandbox.
 */
export const BUILTIN_AGENTS = Object.freeze({
  opencode: Object.freeze({
    title: "OpenCode",
    command: ["opencode"],
    requiredArgs: ["--server", "http://127.0.0.1:{port}"],
    defaultArgs: [],
    resumeArgs: ["--continue"],
    herdrDetectionKind: "opencode",
    profile: "profiles/herdr-opencode-client.json",
    serverPattern: "\\bserve\\b.*--port {port}\\b",
    hostService: "opencode",
    server: Object.freeze({
      command: ["opencode", "serve", "--hostname", "127.0.0.1", "--port", "{port}"],
      profile: "profiles/herdr-opencode-server.json",
      passwordEnv: "OPENCODE_PASSWORD",
      readyPath: "/api/info",
      user: "opencode",
    }),
  }),
});

/**
 * Replaces `{port}` in every word of an argv (or in a pattern string).
 * @template {string|string[]} T
 * @param {T} value
 * @param {number|null} port
 * @returns {T}
 */
export function withPort(value, port) {
  const fill = (text) => (port === null ? text : text.replaceAll("{port}", String(port)));
  return /** @type {T} */ (Array.isArray(value) ? value.map(fill) : fill(value));
}

const KIND_PATTERN = /^[a-z0-9][a-z0-9-]*$/;

function isStringArray(value) {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

/**
 * Validates one custom agent profile from config.customAgents.
 * @param {string} kind
 * @param {unknown} profile
 * @returns {{title: string, command: string[], requiredArgs: string[], defaultArgs: string[], resumeArgs: string[], herdrDetectionKind: string|null, profile: string, serverPattern: string|null, hostService: string|null, server: {command: string[], profile: string, passwordEnv: string, readyPath: string, user: string}|null}}
 */
export function validateCustomAgent(kind, profile) {
  const fail = (reason) => {
    throw new PluginError("config", `customAgents.${kind} ${reason}.`);
  };
  if (!KIND_PATTERN.test(kind)) fail("has an invalid kind; use lowercase letters, digits and hyphens");
  if (!profile || typeof profile !== "object" || Array.isArray(profile)) fail("must be an object");
  const known = ["title", "command", "requiredArgs", "defaultArgs", "resumeArgs", "herdrDetectionKind", "profile", "serverPattern"];
  const unknown = Object.keys(profile).filter((key) => !known.includes(key));
  if (unknown.length > 0) fail(`has unknown field(s): ${unknown.join(", ")}`);
  const { title, command, requiredArgs = [], defaultArgs = [], resumeArgs = [], herdrDetectionKind = null, profile: nonoProfile, serverPattern = null } = /** @type {any} */ (profile);
  if (typeof title !== "string" || title === "") fail("needs a non-empty title");
  if (!isStringArray(command) || command.length === 0 || command[0] === "") fail("needs command: a non-empty array of strings launched inside the sandbox");
  for (const [name, value] of [["requiredArgs", requiredArgs], ["defaultArgs", defaultArgs], ["resumeArgs", resumeArgs]]) {
    if (!isStringArray(value)) fail(`${name} must be an array of strings`);
  }
  if (typeof nonoProfile !== "string" || nonoProfile === "") fail("needs profile: a nono profile name or the path of a profile JSON file");
  if (herdrDetectionKind !== null && (typeof herdrDetectionKind !== "string" || herdrDetectionKind === "")) fail("herdrDetectionKind must be a non-empty string or null");
  if (serverPattern !== null) {
    if (typeof serverPattern !== "string" || serverPattern === "") fail("serverPattern must be a non-empty regular expression string or null");
    try {
      new RegExp(serverPattern);
    } catch (error) {
      fail(`serverPattern is not a valid regular expression (${error.message})`);
    }
  }
  return { title, command, requiredArgs, defaultArgs, resumeArgs, herdrDetectionKind, profile: nonoProfile, serverPattern, hostService: null, server: null };
}

/**
 * Returns every adapter available for a config: built-ins overlaid by custom profiles.
 * @param {{customAgents?: Record<string, unknown>}} config
 * @returns {Record<string, ReturnType<typeof validateCustomAgent>>}
 */
export function availableAgents(config) {
  const agents = { ...BUILTIN_AGENTS };
  for (const [kind, profile] of Object.entries(config.customAgents ?? {})) {
    agents[kind] = validateCustomAgent(kind, profile);
  }
  return agents;
}

/**
 * Turns a profile reference into what `nono run --profile` takes: a name is
 * passed through, a relative `.json` path is resolved against the plugin root
 * (the shipped profiles live in `profiles/`), an absolute path stays as it is.
 * @param {string} profile
 * @param {string} pluginRoot
 * @returns {string}
 */
export function resolveProfileRef(profile, pluginRoot) {
  if (profile.endsWith(".json") && !path.isAbsolute(profile)) {
    return path.join(pluginRoot, profile);
  }
  return profile;
}

/**
 * Puts the adapter's required arguments right after the command and drops
 * any copy of a required flag from the configured arguments, together with
 * its value when the required form has one (`--server <url>`), so
 * configuration can never override them.
 * @param {string[]} command
 * @param {string[]} requiredArgs
 * @param {string[]} args
 * @returns {string[]}
 */
export function withRequiredArgs(command, requiredArgs, args) {
  const takesValue = new Map();
  requiredArgs.forEach((arg, index) => {
    if (arg.startsWith("-")) {
      const next = requiredArgs[index + 1];
      takesValue.set(arg, next !== undefined && !next.startsWith("-"));
    }
  });
  const kept = [];
  for (let index = 0; index < args.length; index += 1) {
    const [flag] = args[index].split("=", 1);
    if (takesValue.has(flag)) {
      if (takesValue.get(flag) && !args[index].includes("=")) index += 1;
      continue;
    }
    kept.push(args[index]);
  }
  return [...command, ...requiredArgs, ...kept];
}

/**
 * Resolves the configured agent, its nono profiles and its launch argv (and
 * the argv `reconnect` uses to resume). The argv may still contain `{port}`;
 * {@link withPort} fills it in per launch.
 * @param {{agentKind: string, agentArgs?: Record<string, string[]>, resumeArgs?: Record<string, string[]>, customAgents?: Record<string, unknown>, profile?: string|null, serverProfile?: string|null}} config
 * @param {{pluginRoot?: string}} [options]
 * @returns {ReturnType<typeof validateCustomAgent> & {kind: string, launchArgv: string[], resumeArgv: string[], profileRef: string, serverProfileRef: string|null}}
 */
export function resolveAgent(config, { pluginRoot = process.cwd() } = {}) {
  const agents = availableAgents(config);
  const adapter = typeof config.agentKind === "string" && Object.hasOwn(agents, config.agentKind) ? agents[config.agentKind] : undefined;
  if (!adapter) {
    throw new PluginError("config", `Unknown agentKind "${config.agentKind}". Available: ${Object.keys(agents).join(", ")}.`);
  }
  const args = config.agentArgs?.[config.agentKind] ?? adapter.defaultArgs;
  const resume = config.resumeArgs?.[config.agentKind] ?? adapter.resumeArgs;
  const launchArgv = withRequiredArgs(adapter.command, adapter.requiredArgs, args);
  const resumeArgv = withRequiredArgs(adapter.command, adapter.requiredArgs, [...args, ...resume.filter((arg) => !args.includes(arg))]);
  const profileRef = resolveProfileRef(config.profile ?? adapter.profile, pluginRoot);
  const serverProfileRef = adapter.server ? resolveProfileRef(config.serverProfile ?? adapter.server.profile, pluginRoot) : null;
  return { ...adapter, kind: config.agentKind, launchArgv, resumeArgv, profileRef, serverProfileRef };
}

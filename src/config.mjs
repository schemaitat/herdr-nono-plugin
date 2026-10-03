/**
 * User configuration stored as JSON in HERDR_PLUGIN_CONFIG_DIR/config.json.
 * Unknown keys are rejected so typos never silently fall back to defaults.
 * @module config
 */
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { CONFIG_FILE, NONO_BIN_ENV } from "./constants.mjs";
import { PluginError } from "./errors.mjs";

/** Directions accepted for the agent pane split. */
export const PANE_DIRECTIONS = Object.freeze(["right", "down"]);

/** Where start-agent puts the agent: a split next to the focused pane or a new tab. */
export const OPEN_MODES = Object.freeze(["split", "tab"]);

/** What the bridge does when an unsandboxed OpenCode background service is running on the host. */
export const HOST_SERVICE_CHECKS = Object.freeze(["warn", "refuse", "off"]);

/** What the bridge does when the verification of a freshly started session fails. */
export const VERIFICATION_FAILURE_ACTIONS = Object.freeze(["stop", "warn"]);

/**
 * The validated user configuration: every key of {@link CONFIG_DEFAULTS} with
 * the type the README documents for it. New config keys are added here too.
 * @typedef {object} PluginConfig
 * @property {string} agentKind
 * @property {Record<string, string[]>} agentArgs
 * @property {Record<string, string[]>} resumeArgs
 * @property {string[]} agentEnv
 * @property {Record<string, unknown>} customAgents
 * @property {string|null} profile
 * @property {string|null} serverProfile
 * @property {string[]} allowPaths
 * @property {string[]} readPaths
 * @property {string[]} nonoArgs
 * @property {string|null} nonoBin Null until {@link loadConfig} resolves the executable.
 * @property {boolean} silent
 * @property {string} shell
 * @property {"right"|"down"} paneDirection
 * @property {number} paneRatio
 * @property {"split"|"tab"} openIn
 * @property {boolean} reportAgentStatus
 * @property {string} sessionNamePrefix
 * @property {boolean} cleanupOnWorktreeRemoved
 * @property {"warn"|"refuse"|"off"} hostServiceCheck
 * @property {boolean} verifyAfterStart
 * @property {"stop"|"warn"} onVerificationFailure
 */

/**
 * Every supported key with its default value.
 * @type {Readonly<PluginConfig>}
 */
export const CONFIG_DEFAULTS = Object.freeze({
  agentKind: "opencode",
  agentArgs: {},
  resumeArgs: {},
  agentEnv: [],
  customAgents: {},
  profile: null,
  serverProfile: null,
  allowPaths: [],
  readPaths: [],
  nonoArgs: [],
  nonoBin: null,
  silent: false,
  shell: "bash",
  paneDirection: "right",
  paneRatio: 0.5,
  openIn: "split",
  reportAgentStatus: true,
  sessionNamePrefix: "herdr",
  cleanupOnWorktreeRemoved: true,
  hostServiceCheck: "refuse",
  verifyAfterStart: true,
  onVerificationFailure: "stop",
});

/**
 * Returns the config file path for a config directory.
 * @param {string} configDir
 * @returns {string}
 */
export function configPath(configDir) {
  return path.join(configDir, CONFIG_FILE);
}

function isStringArray(value) {
  return Array.isArray(value) && value.every((item) => typeof item === "string" && item !== "");
}

const ENV_ENTRY = /^[A-Za-z_][A-Za-z0-9_]*=.*$/;

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/**
 * Validates a raw config object and merges it over the defaults.
 * @param {Record<string, unknown>} raw
 * @returns {PluginConfig}
 */
export function validateConfig(raw) {
  if (!isPlainObject(raw)) {
    throw new PluginError("config", "config.json must contain a JSON object.");
  }
  const unknown = Object.keys(raw).filter((key) => !(key in CONFIG_DEFAULTS));
  if (unknown.length > 0) {
    throw new PluginError("config", `Unknown config key(s): ${unknown.join(", ")}. Supported keys: ${Object.keys(CONFIG_DEFAULTS).join(", ")}.`);
  }
  // Typed loosely until every field has been checked below.
  const config = /** @type {Record<string, any>} */ ({ ...CONFIG_DEFAULTS, ...raw });
  const fail = (key, expectation) => {
    throw new PluginError("config", `Config key "${key}" ${expectation}.`);
  };
  if (typeof config.agentKind !== "string" || config.agentKind === "") fail("agentKind", "must be a non-empty string");
  for (const key of ["agentArgs", "resumeArgs"]) {
    if (!isPlainObject(config[key]) || !Object.values(config[key]).every((value) => Array.isArray(value) && value.every((item) => typeof item === "string"))) fail(key, "must map agent kinds to arrays of strings");
  }
  if (!isPlainObject(config.customAgents)) fail("customAgents", "must be an object keyed by agent kind");
  for (const key of ["nonoArgs"]) {
    if (!isStringArray(config[key])) fail(key, "must be an array of non-empty strings");
  }
  for (const key of ["allowPaths", "readPaths"]) {
    if (!isStringArray(config[key]) || !config[key].every((item) => path.isAbsolute(item))) fail(key, "must be an array of absolute paths");
  }
  if (!isStringArray(config.agentEnv) || !config.agentEnv.every((item) => ENV_ENTRY.test(item))) fail("agentEnv", "must be an array of KEY=VALUE strings");
  for (const key of ["profile", "serverProfile", "nonoBin"]) {
    if (config[key] !== null && (typeof config[key] !== "string" || config[key] === "")) fail(key, "must be a non-empty string or null");
  }
  if (config.nonoArgs.includes("--")) fail("nonoArgs", "must not contain \"--\"; the plugin adds it before the agent command");
  for (const key of ["silent", "reportAgentStatus", "cleanupOnWorktreeRemoved", "verifyAfterStart"]) {
    if (typeof config[key] !== "boolean") fail(key, "must be true or false");
  }
  if (typeof config.shell !== "string" || config.shell === "") fail("shell", "must be a non-empty string");
  if (!PANE_DIRECTIONS.includes(config.paneDirection)) fail("paneDirection", `must be one of ${PANE_DIRECTIONS.join(", ")}`);
  if (!OPEN_MODES.includes(config.openIn)) fail("openIn", `must be one of ${OPEN_MODES.join(", ")}`);
  if (!HOST_SERVICE_CHECKS.includes(config.hostServiceCheck)) fail("hostServiceCheck", `must be one of ${HOST_SERVICE_CHECKS.join(", ")}`);
  if (!VERIFICATION_FAILURE_ACTIONS.includes(config.onVerificationFailure)) fail("onVerificationFailure", `must be one of ${VERIFICATION_FAILURE_ACTIONS.join(", ")}`);
  if (typeof config.paneRatio !== "number" || !(config.paneRatio > 0 && config.paneRatio < 1)) fail("paneRatio", "must be a number between 0 and 1 (exclusive)");
  if (typeof config.sessionNamePrefix !== "string" || !/^[A-Za-z0-9][A-Za-z0-9-]*$/.test(config.sessionNamePrefix)) fail("sessionNamePrefix", "must start with a letter or digit and contain only letters, digits and hyphens");
  return /** @type {PluginConfig} */ (config);
}

/**
 * Loads and validates the configuration, resolving the nono executable.
 * @param {string} configDir
 * @param {NodeJS.ProcessEnv} [env]
 * @returns {PluginConfig & {nonoBin: string}}
 */
export function loadConfig(configDir, env = process.env) {
  const file = configPath(configDir);
  let raw = {};
  if (existsSync(file)) {
    let text;
    try {
      text = readFileSync(file, "utf8");
    } catch (error) {
      throw new PluginError("config", `Could not read ${file}: ${error.message}`, { cause: error });
    }
    try {
      raw = text.trim() === "" ? {} : JSON.parse(text);
    } catch (error) {
      throw new PluginError("config", `${file} is not valid JSON: ${error.message}`, { cause: error });
    }
  }
  const config = validateConfig(raw);
  config.nonoBin = config.nonoBin ?? env[NONO_BIN_ENV] ?? "nono";
  return /** @type {PluginConfig & {nonoBin: string}} */ (config);
}

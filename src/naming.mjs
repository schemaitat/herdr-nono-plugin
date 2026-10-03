/**
 * Session naming. Every agent pane gets a stable name such as
 * `herdr-opencode-3f9a1c0b2d4e` that is passed to `nono run --name`, so
 * `nono ps` shows which pane a sandboxed process belongs to. Each launch in
 * the pane is its own nono session, but they all carry this name.
 * @module naming
 */
import { createHash, randomBytes } from "node:crypto";
import { SERVER_SESSION_SUFFIX } from "./constants.mjs";
import { PluginError } from "./errors.mjs";

/** Pattern every session name produced or accepted by the plugin must match. */
export const SESSION_NAME_PATTERN = /^[A-Za-z0-9][A-Za-z0-9.-]+$/;

/**
 * Lower-cases a label and replaces everything outside [a-z0-9] with hyphens.
 * @param {string} value
 * @param {string} [fallback]
 * @returns {string}
 */
export function slugify(value, fallback = "agent") {
  const slug = String(value ?? "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return slug === "" ? fallback : slug;
}

/**
 * Throws when a name is not a plain session name.
 * @param {string} name
 */
export function assertSessionName(name) {
  if (!SESSION_NAME_PATTERN.test(name)) {
    throw new PluginError("config", `Invalid session name "${name}": use at least two characters, start with a letter or digit, and only use letters, digits, hyphens and periods.`);
  }
}

/**
 * Produces a fresh unique session name such as `herdr-opencode-3f9a1c0b2d4e`.
 * @param {{prefix?: string, agentKind: string, localPath: string, paneId: string|null}} input
 * @returns {string}
 */
export function sessionNameFor({ prefix = "herdr", agentKind, localPath, paneId }) {
  const digest = createHash("sha256")
    .update(`${localPath}\n${paneId ?? ""}\n${randomBytes(8).toString("hex")}`)
    .digest("hex")
    .slice(0, 12);
  const name = `${slugify(prefix, "herdr")}-${slugify(agentKind)}-${digest}`;
  assertSessionName(name);
  return name;
}

/**
 * Name of the nono session an `open-shell` pane runs under.
 * @param {string} sessionName
 * @returns {string}
 */
export function shellSessionName(sessionName) {
  return `${sessionName}-shell`;
}

/**
 * Name of the nono session an agent's private server runs under.
 * @param {string} sessionName
 * @returns {string}
 */
export function serverSessionName(sessionName) {
  return `${sessionName}${SERVER_SESSION_SUFFIX}`;
}

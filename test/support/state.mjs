/**
 * The little the tests need of the plugin's mapping files and result lines, so
 * fixtures and the fake CLIs do not depend on the plugin's own code: where a
 * pane's mapping file lives, reading and writing one, and parsing the result
 * marker line. The files follow the format documented in docs/design.md; the
 * Rust binary reads and writes the same ones.
 */
import { createHash, randomBytes } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";

export const RESULT_MARKER = "HERDR_SANDBOX_RESULT:";

/** The file that stores one pane's mapping: a readable prefix plus a hash of the pane id. */
export function paneEntryPath(stateDir, paneId) {
  const readable = paneId.replace(/[^A-Za-z0-9._-]/g, "_").slice(0, 64);
  const digest = createHash("sha256").update(paneId).digest("hex").slice(0, 10);
  return path.join(stateDir, "panes", `${readable}-${digest}.json`);
}

/** Every mapping, keyed by pane id (a null-prototype object). */
export function loadState(stateDir) {
  const panes = Object.create(null);
  const dir = path.join(stateDir, "panes");
  if (existsSync(dir)) {
    for (const name of readdirSync(dir).filter((item) => item.endsWith(".json")).sort()) {
      const entry = JSON.parse(readFileSync(path.join(dir, name), "utf8"));
      panes[entry.paneId] = entry;
    }
  }
  return { version: 1, panes };
}

/** One pane's mapping, or null. */
export function getPaneEntry(stateDir, paneId) {
  if (!paneId) return null;
  const file = paneEntryPath(stateDir, paneId);
  return existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : null;
}

/** Writes a mapping the way the plugin does, atomically; fixtures and fakes do not need its lock. */
export function savePaneEntry(stateDir, paneId, entry) {
  const stored = { ...entry, version: 1, paneId, updatedAt: new Date().toISOString(), revision: randomBytes(6).toString("hex") };
  const file = paneEntryPath(stateDir, paneId);
  mkdirSync(path.dirname(file), { recursive: true });
  const temp = `${file}.${process.pid}.${randomBytes(4).toString("hex")}.tmp`;
  writeFileSync(temp, `${JSON.stringify(stored, null, 2)}\n`, { mode: 0o600 });
  renameSync(temp, file);
  return stored;
}

/** The parsed payload of the first result marker line in captured stdout, or null. */
export function parseResultLine(text) {
  for (const line of String(text).split("\n")) {
    if (line.startsWith(RESULT_MARKER)) return JSON.parse(line.slice(RESULT_MARKER.length).trim());
  }
  return null;
}

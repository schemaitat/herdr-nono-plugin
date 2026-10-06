#!/usr/bin/env node
/**
 * Fake `herdr` CLI for tests. Logs argv to FAKE_HERDR_LOG and answers with the
 * JSON shapes the plugin relies on. FAKE_HERDR_FAIL="pane split" makes that
 * command exit 1. `pane list` answers with every pane id in FAKE_HERDR_PANES
 * plus every mapped pane in the state dir, minus FAKE_HERDR_MISSING_PANES.
 * With FAKE_HERDR_BRIDGE_STARTS=1 a `pane run` acknowledges the typed bridge
 * command on the mapping right away, emulating a bridge that starts in the
 * pane. `plugin action invoke` answers like Herdr does (with a log_id), and
 * `plugin log list` returns the entries in FAKE_HERDR_ACTION_LOGS (a JSON file
 * holding an array), newest first. FAKE_HERDR_CONFIG_CHECK=fail makes
 * `config check` fail.
 */
import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { getPaneEntry, loadState, savePaneEntry } from "../../src/state.mjs";

const argv = process.argv.slice(2);
const logFile = process.env.FAKE_HERDR_LOG;
let previous = [];
if (logFile && existsSync(logFile)) {
  previous = readFileSync(logFile, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
}
if (logFile) {
  appendFileSync(logFile, `${JSON.stringify({ argv })}\n`);
}

function missingPanes() {
  return (process.env.FAKE_HERDR_MISSING_PANES ?? "").split(",").map((item) => item.trim()).filter(Boolean);
}

const command = argv.slice(0, 2).join(" ");
const failing = (process.env.FAKE_HERDR_FAIL ?? "").split(",").map((item) => item.trim()).filter(Boolean);
if (failing.includes(command) || failing.includes(argv.slice(0, 3).join(" "))) {
  process.stdout.write(`${JSON.stringify({ error: { code: "fake_failure", message: `fake herdr refuses ${command}` } })}\n`);
  process.exit(1);
}

if (command === "pane run" && process.env.FAKE_HERDR_BRIDGE_STARTS === "1" && process.env.HERDR_PLUGIN_STATE_DIR) {
  const entry = getPaneEntry(process.env.HERDR_PLUGIN_STATE_DIR, argv[2]);
  const commandText = String(argv[3] ?? "");
  // The typed command is `node src/bridge.mjs <mode>` (Node) or `<binary> bridge <mode>` (Rust).
  if (entry && /bridge(\.mjs)? (start|connect)\b/.test(commandText)) {
    const launchId = commandText.match(/--launch-id (\S+)/)?.[1] ?? null;
    savePaneEntry(process.env.HERDR_PLUGIN_STATE_DIR, argv[2], { ...entry, bridgeStartedAt: new Date().toISOString(), bridgeLaunchId: launchId });
  }
  process.stdout.write(`${JSON.stringify({ result: { ok: true } })}\n`);
} else if (command === "pane get") {
  const paneId = argv[2];
  if (missingPanes().includes(paneId)) {
    process.stderr.write(`${JSON.stringify({ error: { code: "pane_not_found", message: `pane ${paneId} not found` } })}\n`);
    process.exit(1);
  }
  const agent = process.env.FAKE_HERDR_PANE_AGENT;
  process.stdout.write(`${JSON.stringify({ result: { pane: { pane_id: paneId, agent_status: agent ? "working" : "unknown", ...(agent ? { agent } : {}) } } })}\n`);
} else if (command === "pane list") {
  const missing = missingPanes();
  const ids = new Set((process.env.FAKE_HERDR_PANES ?? "").split(",").map((item) => item.trim()).filter(Boolean));
  if (process.env.HERDR_PLUGIN_STATE_DIR && existsSync(process.env.HERDR_PLUGIN_STATE_DIR)) {
    for (const paneId of Object.keys(loadState(process.env.HERDR_PLUGIN_STATE_DIR).panes)) ids.add(paneId);
  }
  const panes = [...ids].filter((id) => !missing.includes(id)).map((pane_id) => ({ pane_id, workspace_id: pane_id.split(":")[0] }));
  process.stdout.write(`${JSON.stringify({ result: { panes } })}\n`);
} else if (command === "pane split" || command === "tab create") {
  const created = previous.filter((entry) => ["pane split", "tab create"].includes(entry.argv.slice(0, 2).join(" "))).length + 1;
  const newPaneId = process.env.FAKE_HERDR_NEW_PANE_ID || `pane-new-${created}`;
  if (command === "tab create") {
    process.stdout.write(`${JSON.stringify({ result: { tab: { tab_id: `tab-new-${created}` }, root_pane: { pane_id: newPaneId } } })}\n`);
  } else {
    process.stdout.write(`${JSON.stringify({ result: { pane: { pane_id: newPaneId } } })}\n`);
  }
} else if (command === "plugin action" && argv[2] === "invoke") {
  process.stdout.write(`${JSON.stringify({ result: { type: "plugin_action_invoked", log: { action_id: argv[3], log_id: process.env.FAKE_HERDR_LOG_ID ?? "plugin-log-1", status: "running" } } })}\n`);
} else if (command === "plugin log") {
  const file = process.env.FAKE_HERDR_ACTION_LOGS;
  const logs = file && existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : [];
  process.stdout.write(`${JSON.stringify({ result: { type: "plugin_log_list", logs } })}\n`);
} else if (command === "config check") {
  if (process.env.FAKE_HERDR_CONFIG_CHECK === "fail") {
    process.stderr.write("config error: bad key\n");
    process.exit(1);
  }
  process.stdout.write("config ok\n");
} else {
  process.stdout.write(`${JSON.stringify({ result: { ok: true } })}\n`);
}

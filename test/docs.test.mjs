import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { test } from "node:test";
import { ACTION_IDS } from "../src/action-main.mjs";
import { BUILTIN_AGENTS } from "../src/agents.mjs";
import { CONFIG_DEFAULTS } from "../src/config.mjs";
import { NONO_BIN_ENV, PLUGIN_ID, RESULT_MARKER } from "../src/constants.mjs";
import { ERROR_KINDS } from "../src/errors.mjs";
import { ROOT } from "./helpers.mjs";

const manifest = readFileSync(path.join(ROOT, "herdr-plugin.toml"), "utf8");
const readme = readFileSync(path.join(ROOT, "README.md"), "utf8");
const changelog = readFileSync(path.join(ROOT, "CHANGELOG.md"), "utf8");
const pkg = JSON.parse(readFileSync(path.join(ROOT, "package.json"), "utf8"));

function manifestTables() {
  const parts = manifest.split(/^\[\[(\w+)\]\]$/m);
  const tables = { head: parts[0], actions: [], events: [], panes: [], build: [], link_handlers: [] };
  for (let index = 1; index < parts.length; index += 2) {
    tables[parts[index]].push(parts[index + 1]);
  }
  return tables;
}

function field(block, name) {
  const match = block.match(new RegExp(`^${name} = "([^"]+)"`, "m"));
  return match ? match[1] : null;
}

function commandPath(block) {
  const match = block.match(/^command = \["sh", "(bin\/run\.sh|scripts\/[^"]+)"(?:, "([^"]+)")?\]/m);
  return match ? match[2] ?? match[1] : null;
}

const tables = manifestTables();

test("manifest header matches the code and package metadata", () => {
  assert.equal(field(tables.head, "id"), PLUGIN_ID);
  assert.equal(field(tables.head, "version"), pkg.version);
  assert.ok(field(tables.head, "min_herdr_version"));
  assert.match(tables.head, /^platforms = \["linux"\]$/m);
  assert.ok(changelog.includes(`## ${pkg.version}`), "CHANGELOG has a heading for the current version");
});

test("manifest actions and the dispatcher agree, in the same order", () => {
  assert.deepEqual(tables.actions.map((block) => field(block, "id")), [...ACTION_IDS]);
  for (const block of tables.actions) {
    assert.ok(field(block, "title"), "every action has a title");
    assert.match(block, /^contexts = \[/m);
  }
});

test("every manifest command points at an existing script through the node shim", () => {
  for (const block of [...tables.actions, ...tables.events, ...tables.panes]) {
    assert.match(block, /^command = \["sh", "bin\/run\.sh", "src\/[a-z-]+\.mjs"\]$/m, block.trim().split("\n")[0]);
    assert.ok(existsSync(path.join(ROOT, commandPath(block))), `${commandPath(block)} exists`);
  }
  assert.equal(tables.build.length, 1);
  assert.ok(existsSync(path.join(ROOT, commandPath(tables.build[0]))), "build script exists");
  assert.deepEqual(tables.events.map((block) => field(block, "on")), ["worktree.removed"]);
  assert.deepEqual(tables.panes.map((block) => field(block, "id")), ["sandboxes"]);
});

test("the shipped profiles extend the OpenCode pack, close the host sockets and lock down localhost", () => {
  const read = (ref) => JSON.parse(readFileSync(path.join(ROOT, ref), "utf8"));
  const client = read(BUILTIN_AGENTS.opencode.profile);
  const server = read(BUILTIN_AGENTS.opencode.server.profile);
  for (const profile of [client, server]) {
    assert.equal(profile.extends, "nolabs-ai/opencode");
    assert.equal(profile.linux.af_unix_mediation, "pathname");
    assert.ok(profile.environment.deny_vars.includes("HERDR_*"));
    assert.ok(profile.environment.deny_vars.includes("SSH_AUTH_SOCK"));
    assert.deepEqual(profile.filesystem.suppress_save_prompt, ["/"]);
  }
  assert.deepEqual(client.network, { block: true }, "the client reaches nothing but its server's port");
  assert.deepEqual(server.network, { allow_domain: ["*"] }, "the server reaches the internet through nono's proxy only, so direct localhost connects are denied");
});

test("README documents every action, config key, agent kind and error kind", () => {
  for (const id of ACTION_IDS) {
    assert.ok(readme.includes(`\`${id}\``), `README mentions action ${id}`);
  }
  const words = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen"];
  const count = words[ACTION_IDS.length];
  assert.ok(readme.includes(`# ${count} actions`), `README's install check names ${count} actions`);
  assert.ok(readFileSync(path.join(ROOT, "docs", "manual-testing.md"), "utf8").includes(`${count} actions`), `docs/manual-testing.md names ${count} actions`);
  for (const key of Object.keys(CONFIG_DEFAULTS)) {
    assert.ok(readme.includes(`\`${key}\``), `README mentions config key ${key}`);
  }
  for (const kind of Object.keys(BUILTIN_AGENTS)) {
    assert.ok(readme.includes(`\`${kind}\``), `README mentions agent kind ${kind}`);
  }
  for (const kind of ERROR_KINDS) {
    assert.ok(readme.includes(`\`${kind}\``), `README mentions error kind ${kind}`);
  }
  assert.ok(readme.includes(RESULT_MARKER));
});

test("README documents the hook, the overlay, the environment overrides and every module", () => {
  for (const token of ["worktree.removed", NONO_BIN_ENV, "HERDR_AGENT", "--listen-port", "--open-port", "OPENCODE_PASSWORD", "profiles/herdr-opencode-client.json", "profiles/herdr-opencode-server.json", "docs/security.md", "docs/manual-testing.md", "bin/run.sh", "scripts/write-node-path.sh", "scripts/install-keybindings.sh", "scripts/run-action.sh"]) {
    assert.ok(readme.includes(token), `README mentions ${token}`);
  }
  for (const file of readdirSync(path.join(ROOT, "src")).filter((name) => name.endsWith(".mjs"))) {
    assert.ok(readme.includes(`src/${file}`), `README file table lists src/${file}`);
  }
});

test("the bootstrap keeps the marker literal in sync with the constant", () => {
  const bootstrap = readFileSync(path.join(ROOT, "src", "action.mjs"), "utf8");
  assert.ok(bootstrap.includes(`const RESULT_MARKER = "${RESULT_MARKER}";`));
  assert.ok(bootstrap.includes(`plugin: "${PLUGIN_ID}"`));
  const shim = readFileSync(path.join(ROOT, "bin", "run.sh"), "utf8");
  assert.ok(shim.includes(`"plugin":"${PLUGIN_ID}"`));
});

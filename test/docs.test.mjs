import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { test } from "node:test";
import { ROOT, SHIPPED_PROFILES, describeBinary } from "./helpers.mjs";

const facts = describeBinary();
const manifest = readFileSync(path.join(ROOT, "herdr-plugin.toml"), "utf8");
const readme = readFileSync(path.join(ROOT, "README.md"), "utf8");
const changelog = readFileSync(path.join(ROOT, "CHANGELOG.md"), "utf8");
const pkg = JSON.parse(readFileSync(path.join(ROOT, "package.json"), "utf8"));
const cargo = readFileSync(path.join(ROOT, "Cargo.toml"), "utf8");

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

const tables = manifestTables();

test("manifest header matches the binary, the crate and the package metadata", () => {
  assert.equal(field(tables.head, "id"), facts.pluginId);
  const version = field(tables.head, "version");
  assert.equal(version, facts.version, "the manifest and the binary agree");
  assert.equal(version, cargo.match(/^version = "([^"]+)"/m)[1], "Cargo.toml agrees");
  assert.equal(version, pkg.version, "package.json agrees");
  assert.ok(field(tables.head, "min_herdr_version"));
  assert.match(tables.head, /^platforms = \["linux"\]$/m);
  assert.ok(changelog.includes(`## ${version}`) || changelog.includes("## Unreleased"), "CHANGELOG has a heading for the current version");
});

test("manifest actions and the binary's dispatcher agree, in the same order", () => {
  assert.deepEqual(tables.actions.map((block) => field(block, "id")), facts.actionIds);
  for (const block of tables.actions) {
    assert.ok(field(block, "title"), "every action has a title");
    assert.match(block, /^contexts = \[/m);
  }
});

test("every manifest command goes through the shim to a subcommand of the binary, and the build step installs it", () => {
  const subcommands = new Set(["action", "events", "pane"]);
  for (const block of [...tables.actions, ...tables.events, ...tables.panes]) {
    const match = block.match(/^command = \["sh", "bin\/run\.sh", "(\w+)"\]$/m);
    assert.ok(match && subcommands.has(match[1]), block.trim().split("\n")[0]);
  }
  assert.ok(existsSync(path.join(ROOT, "bin", "run.sh")), "the shim exists");
  assert.equal(tables.build.length, 1);
  assert.match(tables.build[0], /^command = \["sh", "scripts\/install-binary\.sh"\]$/m);
  assert.ok(existsSync(path.join(ROOT, "scripts", "install-binary.sh")), "the build script exists");
  assert.deepEqual(tables.events.map((block) => field(block, "on")), ["worktree.removed"]);
  assert.deepEqual(tables.panes.map((block) => field(block, "id")), ["sandboxes"]);
});

test("the shipped profiles are where the built-in agent says they are", () => {
  const opencode = facts.builtinAgents.opencode;
  assert.equal(path.join(ROOT, opencode.profile), SHIPPED_PROFILES.client);
  assert.equal(path.join(ROOT, opencode.serverProfile), SHIPPED_PROFILES.server);
  for (const file of Object.values(SHIPPED_PROFILES)) assert.ok(existsSync(file), file);
  // What the profiles must contain is checked next to the code that reads them (cargo test).
});

const docsDir = path.join(ROOT, "docs");
const docPages = readdirSync(docsDir, { recursive: true }).filter((name) => name.endsWith(".md")).map((name) => name.split(path.sep).join("/")).sort();
const doc = (page) => readFileSync(path.join(docsDir, page), "utf8");
const allDocs = docPages.map(doc).join("\n");

test("the reference documents every action, config key, agent kind and error kind", () => {
  const actions = doc("actions.md");
  for (const id of facts.actionIds) {
    assert.equal(actions.split(`| \`${id}\` |`).length - 1, 2, `actions.md has a row for action ${id} and one for its result fields`);
  }
  const words = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen"];
  const count = words[facts.actionIds.length];
  for (const page of ["getting-started.md", "development.md"]) {
    assert.ok(doc(page).includes(`${count} actions`), `${page} names ${count} actions`);
  }
  const configuration = doc("configuration.md");
  for (const key of facts.configKeys) {
    assert.ok(configuration.includes(`| \`${key}\` |`), `configuration.md has a row for config key ${key}`);
  }
  for (const kind of Object.keys(facts.builtinAgents)) {
    assert.ok(configuration.includes(`| \`${kind}\` |`), `configuration.md has a row for agent kind ${kind}`);
  }
  for (const kind of facts.errorKinds) {
    assert.ok(actions.includes(`| \`${kind}\` |`), `actions.md has a row for error kind ${kind}`);
  }
  assert.ok(actions.includes(facts.resultMarker));
});

test("the docs cover the hook, the environment overrides, the profiles and every module", () => {
  for (const token of ["worktree.removed", facts.nonoBinEnv, "HERDR_AGENT", "HERDR_NONO_BINARY", "--listen-port", "--open-port", "OPENCODE_PASSWORD", "profiles/herdr-opencode-client.json", "profiles/herdr-opencode-server.json", "bin/run.sh", "scripts/install-binary.sh", "scripts/install-keybindings.sh", "scripts/run-action.sh"]) {
    assert.ok(allDocs.includes(token), `docs mention ${token}`);
  }
  const layout = doc("development.md");
  const sources = readdirSync(path.join(ROOT, "rust", "src"), { recursive: true }).map(String).filter((name) => name.endsWith(".rs")).map((name) => name.split(path.sep).join("/"));
  assert.ok(sources.length > 20, "found the crate's modules");
  for (const file of sources) {
    assert.ok(layout.includes(`rust/src/${file}`), `development.md lists rust/src/${file}`);
  }
});

test("every docs page is in the site nav and linked from the README, which links the site first", () => {
  const nav = readFileSync(path.join(ROOT, "mkdocs.yml"), "utf8");
  for (const page of docPages) {
    assert.ok(nav.includes(`: ${page}\n`), `mkdocs.yml nav lists ${page}`);
    assert.ok(readme.includes(`](docs/${page})`), `README links docs/${page}`);
  }
  const firstParagraph = readme.split("\n\n")[1];
  assert.match(firstParagraph, /schemaitat\.github\.io\/herdr-nono-plugin/, "the docs link comes right after the title");
});

test("the README and the key bindings page list every installed chord", () => {
  const installer = readFileSync(path.join(ROOT, "scripts", "install-keybindings.sh"), "utf8");
  const chords = [...installer.matchAll(/^add_binding "([^"]+)" "([^"]+)"/gm)];
  assert.ok(chords.length > 0);
  for (const [, chord, action] of chords) {
    for (const [name, text] of [["README.md", readme], ["docs/key-bindings.md", doc("key-bindings.md")]]) {
      assert.ok(text.includes(`| \`${chord}\` | \`${action}\` |`), `${name} lists ${chord} -> ${action}`);
    }
  }
});

test("the shim's startup result line keeps the marker and the plugin id of the binary", () => {
  const shim = readFileSync(path.join(ROOT, "bin", "run.sh"), "utf8");
  assert.ok(shim.includes(`${facts.resultMarker} {`));
  assert.ok(shim.includes(`"plugin":"${facts.pluginId}"`));
  assert.ok(shim.includes(`"schemaVersion":${facts.resultSchemaVersion}`));
});

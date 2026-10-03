import assert from "node:assert/strict";
import { test } from "node:test";
import { SESSION_NAME_PATTERN, assertSessionName, sessionNameFor, shellSessionName, slugify } from "../src/naming.mjs";
import { buildPaneCommand, shellQuote } from "../src/shell.mjs";

test("sessionNameFor produces valid, unique names", () => {
  const first = sessionNameFor({ agentKind: "opencode", localPath: "/tmp/repo", paneId: "pane-1" });
  const second = sessionNameFor({ agentKind: "opencode", localPath: "/tmp/repo", paneId: "pane-1" });
  assert.match(first, /^herdr-opencode-[a-f0-9]{12}$/);
  assert.match(first, SESSION_NAME_PATTERN);
  assert.notEqual(first, second);
});

test("sessionNameFor honors the prefix and slugs odd agent kinds", () => {
  assert.match(sessionNameFor({ prefix: "Team", agentKind: "My Agent!!", localPath: "/x", paneId: null }), /^team-my-agent-[a-f0-9]{12}$/);
});

test("shell sessions carry the agent's name", () => {
  assert.equal(shellSessionName("herdr-opencode-abc"), "herdr-opencode-abc-shell");
});

const slugCases = [
  ["Claude Code", "claude-code"],
  ["--weird--", "weird"],
  ["", "agent"],
  ["UPPER_case.mixed", "upper-case-mixed"],
];
for (const [input, expected] of slugCases) {
  test(`slugify(${JSON.stringify(input)}) === ${expected}`, () => {
    assert.equal(slugify(input), expected);
  });
}

test("assertSessionName rejects names that are not plain words", () => {
  for (const bad of ["a", "-abc", "has space", "under_score"]) {
    assert.throws(() => assertSessionName(bad), /Invalid session name/);
  }
  assert.doesNotThrow(() => assertSessionName("ok-name.1"));
});
const quoteCases = [
  ["simple", "simple"],
  ["/abs/path-1.2:x", "/abs/path-1.2:x"],
  ["has space", "'has space'"],
  ["it's", "'it'\\''s'"],
  ["", "''"],
];
for (const [input, expected] of quoteCases) {
  test(`shellQuote(${JSON.stringify(input)})`, () => {
    assert.equal(shellQuote(input), expected);
  });
}

test("shellQuote refuses control characters and backslashes", () => {
  assert.throws(() => shellQuote(`a${String.fromCharCode(10)}b`), (error) => error.errorKind === "target");
  assert.throws(() => shellQuote(`a${String.fromCharCode(92)}b`), (error) => error.errorKind === "target" && /backslash/.test(error.message));
});

test("buildPaneCommand uses env for fish compatibility and quotes words", () => {
  const command = buildPaneCommand({ argv: ["/usr/bin/node", "/plugin root/src/bridge.mjs", "start"], env: { HERDR_AGENT: "opencode" } });
  assert.equal(command, "env HERDR_AGENT=opencode /usr/bin/node '/plugin root/src/bridge.mjs' start");
  assert.equal(buildPaneCommand({ argv: ["ls"] }), "ls");
  assert.throws(() => buildPaneCommand({ argv: ["ls"], env: { "bad-name": "x" } }), /Invalid environment variable name/);
  assert.throws(() => buildPaneCommand({ argv: [] }), /non-empty argv/);
});

test("shellQuote quotes words that zsh or fish would expand when bare", () => {
  assert.equal(shellQuote("=ls"), "'=ls'", "zsh expands a bare =cmd to its path");
  assert.equal(shellQuote("%1"), "'%1'", "fish expands a bare %job");
  assert.equal(shellQuote("a=b"), "a=b", "an = inside a word is fine");
  assert.equal(shellQuote("50%"), "50%");
});

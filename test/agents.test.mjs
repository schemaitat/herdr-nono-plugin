import assert from "node:assert/strict";
import path from "node:path";
import { test } from "node:test";
import { BUILTIN_AGENTS, resolveAgent, resolveProfileRef, validateCustomAgent, withPort, withRequiredArgs } from "../src/agents.mjs";

const ROOT = "/plugin";

test("OpenCode runs a client in one sandbox and its private server in another, joined by one port", () => {
  const agent = resolveAgent({ agentKind: "opencode" }, { pluginRoot: ROOT });
  assert.deepEqual(withPort(agent.launchArgv, 4242), ["opencode", "--server", "http://127.0.0.1:4242"]);
  assert.deepEqual(withPort(agent.resumeArgv, 4242), ["opencode", "--server", "http://127.0.0.1:4242", "--continue"]);
  assert.deepEqual(withPort(agent.server.command, 4242), ["opencode", "serve", "--hostname", "127.0.0.1", "--port", "4242"]);
  assert.equal(agent.profileRef, path.join(ROOT, "profiles", "herdr-opencode-client.json"));
  assert.equal(agent.serverProfileRef, path.join(ROOT, "profiles", "herdr-opencode-server.json"));
  assert.equal(agent.server.passwordEnv, "OPENCODE_PASSWORD");
  assert.equal(agent.herdrDetectionKind, "opencode");
  const pattern = new RegExp(withPort(agent.serverPattern, 4242));
  assert.ok(pattern.test("/home/u/.opencode/bin/opencode serve --hostname 127.0.0.1 --port 4242"));
  assert.ok(!pattern.test("/home/u/.opencode/bin/opencode serve --hostname 127.0.0.1 --port 42420"), "another port is another server");
  assert.ok(!pattern.test("/home/u/.opencode/bin/opencode serve --service"), "the host background service is not the private server");
});

test("agentArgs replace the default arguments but can never point the client at another server", () => {
  const agent = resolveAgent({ agentKind: "opencode", agentArgs: { opencode: ["--auto", "--server", "http://127.0.0.1:4096"] } }, { pluginRoot: ROOT });
  assert.deepEqual(agent.launchArgv, ["opencode", "--server", "http://127.0.0.1:{port}", "--auto"]);
  assert.deepEqual(agent.resumeArgv, ["opencode", "--server", "http://127.0.0.1:{port}", "--auto", "--continue"]);
  const noResume = resolveAgent({ agentKind: "opencode", resumeArgs: { opencode: [] } }, { pluginRoot: ROOT });
  assert.deepEqual(noResume.resumeArgv, ["opencode", "--server", "http://127.0.0.1:{port}"]);
  assert.equal(resolveAgent({ agentKind: "opencode", serverProfile: "my-server" }, { pluginRoot: ROOT }).serverProfileRef, "my-server");
});

test("a configured profile overrides the adapter's, names pass through, relative files resolve against the plugin", () => {
  assert.equal(resolveAgent({ agentKind: "opencode", profile: "opencode" }, { pluginRoot: ROOT }).profileRef, "opencode");
  assert.equal(resolveAgent({ agentKind: "opencode", profile: "/etc/p.json" }, { pluginRoot: ROOT }).profileRef, "/etc/p.json");
  assert.equal(resolveProfileRef("profiles/x.json", ROOT), "/plugin/profiles/x.json");
  assert.equal(resolveProfileRef("nolabs-ai/opencode", ROOT), "nolabs-ai/opencode");
});

test("withRequiredArgs puts the required arguments first and drops configured copies", () => {
  assert.deepEqual(withRequiredArgs(["a"], ["-x"], ["-y", "-x"]), ["a", "-x", "-y"]);
  assert.deepEqual(withRequiredArgs(["o"], ["--server", "U"], ["--auto", "--server", "http://elsewhere", "--server=x", "-c"]), ["o", "--server", "U", "--auto", "-c"]);
  assert.deepEqual(withPort("p {port}", null), "p {port}");
});

test("custom agents need a title, a command and a profile, and may name a server pattern", () => {
  const agent = validateCustomAgent("claude-code", { title: "Claude Code", command: ["claude"], defaultArgs: ["--dangerously-skip-permissions"], profile: "nolabs-ai/claude", herdrDetectionKind: "claude" });
  assert.equal(agent.serverPattern, null);
  assert.equal(agent.hostService, null);
  assert.equal(agent.server, null);
  const resolved = resolveAgent({ agentKind: "claude-code", customAgents: { "claude-code": { title: "Claude Code", command: ["claude"], profile: "nolabs-ai/claude", resumeArgs: ["--continue"] } } }, { pluginRoot: ROOT });
  assert.deepEqual(resolved.resumeArgv, ["claude", "--continue"]);
  const failures = [
    [{ command: ["x"], profile: "p" }, /title/],
    [{ title: "X", profile: "p" }, /command/],
    [{ title: "X", command: ["x"] }, /profile/],
    [{ title: "X", command: ["x"], profile: "p", serverPattern: "(" }, /serverPattern/],
    [{ title: "X", command: ["x"], profile: "p", sbxAgent: "claude" }, /unknown field/],
  ];
  for (const [profile, pattern] of failures) {
    assert.throws(() => validateCustomAgent("x", profile), (error) => error.errorKind === "config" && pattern.test(error.message));
  }
  assert.throws(() => validateCustomAgent("Bad Kind", { title: "X", command: ["x"], profile: "p" }), /invalid kind/);
});

test("an unknown agentKind lists the available ones", () => {
  assert.throws(() => resolveAgent({ agentKind: "nope" }), (error) => error.errorKind === "config" && /Available: opencode/.test(error.message));
  assert.deepEqual(Object.keys(BUILTIN_AGENTS), ["opencode"]);
});

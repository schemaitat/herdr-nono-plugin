import assert from "node:assert/strict";
import { test } from "node:test";
import { allowDomainHost, buildRunArgs, classifyFailure, createNonoClient, loopbackDomains, loopbackReason, normalizeSessionList, summarizeProfile } from "../src/nono.mjs";
import { FAKE_NONO } from "./helpers.mjs";

test("buildRunArgs grants the workspace root explicitly and puts the agent after --", () => {
  assert.deepEqual(
    buildRunArgs({ profile: "/p.json", sessionName: "herdr-opencode-1", workspaceRoot: "/repo", allowPaths: ["/data"], readPaths: ["/ref"], silent: true, extraArgs: ["--memory", "2G"], argv: ["opencode", "--standalone"] }),
    ["run", "--silent", "--profile", "/p.json", "--name", "herdr-opencode-1", "--allow", "/repo", "--allow", "/data", "--read", "/ref", "--memory", "2G", "--", "opencode", "--standalone"],
  );
  const plain = buildRunArgs({ profile: "opencode", sessionName: "s1", workspaceRoot: "/repo", argv: ["bash"] });
  assert.ok(!plain.includes("--allow-cwd"), "the pane's directory is never granted implicitly");
  assert.throws(() => buildRunArgs({ profile: "p", sessionName: "s", workspaceRoot: "/r", argv: [] }), /needs the command/);
});

test("classifyFailure maps nono's wording", () => {
  assert.equal(classifyFailure("nono: Session not found: abc"), "not-found");
  assert.equal(classifyFailure("nono: Profile not found: x"), "not-found");
  assert.equal(classifyFailure("Permission denied"), "permission");
  assert.equal(classifyFailure("error: unexpected argument '--x' found"), "config");
  assert.equal(classifyFailure("something else"), "unknown");
});

test("normalizeSessionList accepts nono's array and refuses other shapes", () => {
  const [session] = normalizeSessionList([{ session_id: "ab12", name: "herdr-opencode-1", supervisor_pid: 10, child_pid: 11, status: "running", command: ["opencode", "--standalone"], profile: "/p.json", workdir: "/repo" }]);
  assert.equal(session.sessionId, "ab12");
  assert.equal(session.supervisorPid, 10);
  assert.deepEqual(session.command, ["opencode", "--standalone"]);
  assert.deepEqual(normalizeSessionList(null), []);
  assert.deepEqual(normalizeSessionList({ sessions: [] }), []);
  assert.throws(() => normalizeSessionList({ weird: true }), /unexpected shape/);
  assert.throws(() => normalizeSessionList([1, 2]), /unexpected shape/);
});

test("the client reads the version, lists sessions and classifies failures", () => {
  const nono = createNonoClient({ bin: FAKE_NONO, env: { ...process.env, FAKE_NONO_VERSION: "0.79.1" } });
  assert.equal(nono.version().version, "0.79.1");
  assert.deepEqual(nono.listSessions(), []);
  assert.throws(() => nono.stop("nothing"), (error) => error.errorKind === "not-found");
  const failing = createNonoClient({ bin: FAKE_NONO, env: { ...process.env, FAKE_NONO_FAIL: "profile:not-found" } });
  assert.throws(() => failing.showProfile("missing"), (error) => error.errorKind === "not-found" && /Profile not found/.test(error.output));
  const missing = createNonoClient({ bin: "/definitely/not/nono" });
  assert.throws(() => missing.version(), (error) => error.errorKind === "startup" && /Install nono/.test(error.message));
});

test("allowDomainHost reads the host of a name, a wildcard, a URL glob, a port and an IPv6 literal", () => {
  assert.equal(allowDomainHost("API.GitHubCopilot.com."), "api.githubcopilot.com");
  assert.equal(allowDomainHost("*.githubcopilot.com"), "*.githubcopilot.com");
  assert.equal(allowDomainHost("https://github.com/org/**"), "github.com");
  assert.equal(allowDomainHost("https://*.wikipedia.org/wiki/**"), "*.wikipedia.org");
  assert.equal(allowDomainHost("localhost:4096"), "localhost");
  assert.equal(allowDomainHost("[::1]:4096"), "::1");
});

// Every entry here reached a listener on the host's 127.0.0.1 through nono
// 0.78's proxy (or is the same class): the proxy connects to whatever an
// allowed name resolves to.
const LOOPBACK_ENTRIES = ["*", "*.*", "localhost", "LOCALHOST", "foo.localhost", "localhost.localdomain", "127.0.0.1", "127.1", "0x7f.1", "2130706433", "0.0.0.0", "::1", "[::1]", "10.0.0.5", "http://127.0.0.1:4096/**", "myhost", "printer.local", "db.internal", "127.0.0.1.nip.io", "*.nip.io", "a.localtest.me", "*.localtest.me", "app.lvh.me", "127-0-0-1.sslip.io", "*.com", "*.io", "api.*.com"];
const PROVIDER_ENTRIES = ["api.githubcopilot.com", "*.githubcopilot.com", "github.com", "api.github.com", "models.opencode.ai", "https://github.com/org/**", "api.anthropic.com"];

for (const entry of LOOPBACK_ENTRIES) {
  test(`loopbackReason flags ${JSON.stringify(entry)} as a way to localhost`, () => {
    assert.equal(typeof loopbackReason(entry), "string");
  });
}

test("loopbackReason passes the provider hosts the shipped server profile allows", () => {
  for (const entry of PROVIDER_ENTRIES) assert.equal(loopbackReason(entry), null, entry);
  assert.deepEqual(loopbackDomains(["github.com", "*", "localhost"]).map((item) => item.domain), ["*", "localhost"]);
});

test("summarizeProfile says whether a sandbox can reach localhost", () => {
  assert.equal(summarizeProfile({ network: { block: true } }).loopback, false);
  assert.equal(summarizeProfile({ network: {} }).loopback, true, "open egress connects directly");
  const wildcard = summarizeProfile({ network: { allow_domain: ["*"] } });
  assert.equal(wildcard.egress, "allowlist");
  assert.equal(wildcard.loopback, true, "proxy mode with every domain allowed forwards to localhost");
  assert.equal(wildcard.loopbackDomains[0].domain, "*");
  const copilot = summarizeProfile({ network: { allow_domain: ["models.opencode.ai", { domain: "*.githubcopilot.com" }] } });
  assert.equal(copilot.loopback, false);
  assert.deepEqual(copilot.loopbackDomains, []);
});

/**
 * Integration tests for the egress lockdown against the real nono CLI: the
 * agent's tools (the OpenCode server sandbox) must not reach any service on
 * the host's loopback interface, such as the unsandboxed OpenCode host
 * service on 127.0.0.1:4096, while the LLM provider stays reachable.
 *
 * They start real sandboxes with the shipped server profile, so they need
 * nono (HERDR_NONO_BIN, else `nono` on PATH) with the `nolabs-ai/opencode`
 * pack and a kernel with Landlock ABI v4. Without them every test is skipped;
 * HERDR_NONO_INTEGRATION=0 skips them too. The provider test also needs
 * internet access and is skipped without it.
 */
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { lookup } from "node:dns/promises";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { createServer } from "node:net";
import { SHIPPED_PROFILES, createFixture, describeBinary, mappingFor, runBridge, runProbes, summarizeProfile } from "./helpers.mjs";

const LOOPBACK_NAMES = describeBinary().loopbackNames;

/** Listens on an ephemeral port of the host's 127.0.0.1 until closed: the target of the loopback checks. */
async function startCanary() {
  const server = createServer((socket) => socket.destroy());
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve(undefined));
  });
  const { port } = /** @type {import("node:net").AddressInfo} */ (server.address());
  return { port, close: () => new Promise((resolve) => server.close(() => resolve(undefined))) };
}

/** The real nono as an absolute path: the bridge runs with the fixture's PATH. */
const NONO = (() => {
  const bin = process.env.HERDR_NONO_BIN || "nono";
  if (path.isAbsolute(bin)) return bin;
  const found = (process.env.PATH ?? "").split(path.delimiter).map((dir) => path.join(dir, bin)).find((file) => existsSync(file));
  return found ?? bin;
})();
const SERVER_PROFILE = SHIPPED_PROFILES.server;
const PROVIDER_HOSTS = ["api.githubcopilot.com", "api.individual.githubcopilot.com", "api.github.com", "github.com", "models.opencode.ai"];
const OTHER_HOSTS = ["www.google.com", "en.wikipedia.org", "registry.npmjs.org", "api.openai.com"];

/** Why the real-nono tests cannot run here, or null when they can. */
function skipReason() {
  if (process.env.HERDR_NONO_INTEGRATION === "0") return "HERDR_NONO_INTEGRATION=0";
  const version = spawnSync(NONO, ["--version"], { encoding: "utf8", timeout: 10_000 });
  if (version.error || version.status !== 0 || !/^nono \d/m.test(version.stdout)) return `no real nono at "${NONO}"`;
  const pack = spawnSync(NONO, ["profile", "show", "--json", SERVER_PROFILE], { encoding: "utf8", timeout: 20_000 });
  if (pack.status !== 0) return "nono cannot resolve the shipped server profile (is the nolabs-ai/opencode pack installed?)";
  return null;
}

const SKIP = skipReason();

/**
 * Runs inside the sandbox: for each target, either a direct TCP connect
 * (`direct:<port>`) or an HTTP CONNECT through nono's proxy (`host:port`),
 * spaced out because nono rate-limits connects in proxy mode. Prints one JSON
 * line mapping each target to the proxy's status code or the error code.
 */
const SANDBOX_SCRIPT = String.raw`
const net = require("node:net");
const targets = JSON.parse(process.argv[1]);
const raw = process.env.HTTPS_PROXY || process.env.HTTP_PROXY || "";
const proxy = raw ? new URL(raw) : null;
const auth = proxy && proxy.username ? "Basic " + Buffer.from(decodeURIComponent(proxy.username) + ":" + decodeURIComponent(proxy.password)).toString("base64") : null;
function attempt(target) {
  return new Promise((resolve) => {
    let settled = false;
    let timer = null;
    const done = (value) => { if (!settled) { settled = true; clearTimeout(timer); socket.destroy(); resolve(value); } };
    const direct = target.startsWith("direct:");
    if (!direct && !proxy) return resolve("no-proxy");
    const socket = direct ? net.connect({ host: "127.0.0.1", port: Number(target.slice(7)) }) : net.connect({ host: proxy.hostname, port: Number(proxy.port) });
    let buffer = "";
    socket.on("connect", () => {
      if (direct) return done("connected");
      socket.write("CONNECT " + target + " HTTP/1.1\r\nHost: " + target + "\r\n" + (auth ? "Proxy-Authorization: " + auth + "\r\n" : "") + "\r\n");
    });
    socket.on("data", (chunk) => {
      buffer += chunk;
      const match = buffer.match(/^HTTP\/1\.[01] (\d{3})/);
      if (match) done(Number(match[1]));
    });
    socket.on("error", (error) => done(error.code || "error"));
    timer = setTimeout(() => done("timeout"), 8000);
  });
}
(async () => {
  const results = {};
  for (const target of targets) {
    await new Promise((resolve) => setTimeout(resolve, 150));
    results[target] = await attempt(target);
  }
  process.stdout.write("RESULTS " + JSON.stringify(results) + "\n");
})();
`;

/**
 * Runs SANDBOX_SCRIPT under a profile in a throwaway nono sandbox.
 * @param {string} profile
 * @param {string[]} targets
 * @returns {Promise<Record<string, number|string>>}
 */
async function attemptInSandbox(profile, targets) {
  const workspace = mkdtempSync(path.join(tmpdir(), "herdr-nono-egress-"));
  try {
    const child = spawn(NONO, ["run", "--silent", "--profile", profile, "--name", "herdr-nono-egress-test", "--allow", workspace, "--", process.execPath, "-e", SANDBOX_SCRIPT, JSON.stringify(targets)], { cwd: workspace, stdio: ["ignore", "pipe", "pipe"] });
    let output = "";
    child.stdout.on("data", (chunk) => { output += chunk; });
    child.stderr.on("data", (chunk) => { output += chunk; });
    const killer = setTimeout(() => child.kill("SIGKILL"), 60_000);
    await new Promise((resolve) => child.once("close", resolve));
    clearTimeout(killer);
    const line = output.split("\n").find((item) => item.startsWith("RESULTS "));
    assert.ok(line, `the sandboxed script did not report:\n${output}`);
    return JSON.parse(line.slice("RESULTS ".length));
  } finally {
    rmSync(workspace, { recursive: true, force: true });
  }
}

/**
 * Writes a copy of the shipped server profile with another domain allowlist.
 * @param {string} dir
 * @param {string} name
 * @param {string[]} allowDomain
 */
function variantProfile(dir, name, allowDomain) {
  const profile = JSON.parse(readFileSync(SERVER_PROFILE, "utf8"));
  profile.meta.name = name;
  profile.network = { allow_domain: allowDomain };
  const file = path.join(dir, `${name}.json`);
  writeFileSync(file, JSON.stringify(profile, null, 2));
  return file;
}

describe("egress lockdown with the real nono", { skip: SKIP ?? false, concurrency: false, timeout: 180_000 }, () => {
  /** @type {{port: number, close: () => Promise<void>}} */
  let canary;
  let dir;
  before(async () => {
    canary = await startCanary();
    dir = mkdtempSync(path.join(tmpdir(), "herdr-nono-profiles-"));
  });
  after(async () => {
    await canary?.close();
    if (dir) rmSync(dir, { recursive: true, force: true });
  });

  test("the shipped server sandbox reaches no service on localhost, directly or through nono's proxy", async () => {
    const targets = [`direct:${canary.port}`, ...LOOPBACK_NAMES.map((name) => `${name}:${canary.port}`)];
    const results = await attemptInSandbox(SERVER_PROFILE, targets);
    assert.notEqual(results[`direct:${canary.port}`], "connected", "a direct connect to the host's 127.0.0.1 must be denied");
    assert.match(String(results[`direct:${canary.port}`]), /EPERM|EACCES/);
    for (const name of LOOPBACK_NAMES) {
      assert.equal(results[`${name}:${canary.port}`], 403, `nono's proxy must refuse ${name}: ${JSON.stringify(results)}`);
    }
  });

  test("the shipped server sandbox cannot reach the OpenCode host service port through the proxy", async () => {
    const results = await attemptInSandbox(SERVER_PROFILE, ["127.0.0.1:4096", "localhost:4096", "direct:4096"]);
    assert.equal(results["127.0.0.1:4096"], 403);
    assert.equal(results["localhost:4096"], 403);
    assert.match(String(results["direct:4096"]), /EPERM|EACCES/);
  });

  test("the shipped server sandbox reaches GitHub Copilot and OpenCode's model catalog, and no other host", async (t) => {
    try {
      await lookup("api.githubcopilot.com");
    } catch {
      t.skip("no DNS / internet on this host");
      return;
    }
    const results = await attemptInSandbox(SERVER_PROFILE, [...PROVIDER_HOSTS, ...OTHER_HOSTS].map((host) => `${host}:443`));
    for (const host of PROVIDER_HOSTS) assert.equal(results[`${host}:443`], 200, `${host} must be allowed: ${JSON.stringify(results)}`);
    for (const host of OTHER_HOSTS) assert.equal(results[`${host}:443`], 403, `${host} must be refused: ${JSON.stringify(results)}`);
  });

  test("control: with every domain allowed, or localhost allowed, the proxy reaches the canary", async () => {
    // Proves the tests above detect the hole they guard against: this is the
    // previous shipped profile ("*") and an allowlist naming localhost.
    const wildcard = variantProfile(dir, "herdr-test-wildcard", ["*"]);
    const named = variantProfile(dir, "herdr-test-localhost", ["api.githubcopilot.com", "localhost"]);
    const open = await attemptInSandbox(wildcard, [`127.0.0.1:${canary.port}`, `localhost:${canary.port}`]);
    assert.equal(open[`127.0.0.1:${canary.port}`], 200, JSON.stringify(open));
    assert.equal(open[`localhost:${canary.port}`], 200, JSON.stringify(open));
    const viaName = await attemptInSandbox(named, [`localhost:${canary.port}`, `127.0.0.1:${canary.port}`]);
    assert.equal(viaName[`localhost:${canary.port}`], 200, "nono does not filter what an allowed name resolves to");
    assert.equal(viaName[`127.0.0.1:${canary.port}`], 403);
  });

  test("the plugin classifies the profiles nono resolves: shipped is locked down, the variants are not", () => {
    const shipped = summarizeProfile(NONO, SERVER_PROFILE);
    assert.equal(shipped.egress, "allowlist");
    assert.equal(shipped.loopback, false, JSON.stringify(shipped.loopbackDomains));
    assert.ok(shipped.allowDomains.includes("api.githubcopilot.com"));
    const wildcard = summarizeProfile(NONO, variantProfile(dir, "herdr-test-wildcard", ["*"]));
    assert.equal(wildcard.loopback, true);
    const nip = summarizeProfile(NONO, variantProfile(dir, "herdr-test-nip", ["github.com", "127.0.0.1.nip.io"]));
    assert.deepEqual(nip.loopbackDomains.map((item) => item.domain), ["127.0.0.1.nip.io"]);
  });

  test("doctor's escape probe passes with the shipped server profile and fails with every domain allowed", async () => {
    const shipped = Object.fromEntries((await runProbes({ nonoBin: NONO, profile: SERVER_PROFILE })).checks.map((check) => [check.check, check]));
    assert.equal(shipped.loopbackCanary.result, "denied");
    assert.equal(shipped.loopbackViaProxy.result, "denied");
    assert.equal(shipped.opencodeServicePort.ok, true);
    assert.equal(shipped.marker.ok, true);
    const wildcard = Object.fromEntries((await runProbes({ nonoBin: NONO, profile: variantProfile(dir, "herdr-test-wildcard", ["*"]) })).checks.map((check) => [check.check, check]));
    assert.equal(wildcard.loopbackViaProxy.ok, false);
    assert.equal(wildcard.loopbackViaProxy.severity, "critical");
    assert.match(wildcard.loopbackViaProxy.result, /^allowed via 127\.0\.0\.1, localhost/);
  });

  test("start-agent refuses a server profile that allows every domain before any sandbox starts", () => {
    const f = createFixture({ config: { nonoBin: NONO, verifyAfterStart: false, serverProfile: variantProfile(dir, "herdr-test-wildcard", ["*"]) }, panes: ({ worktree }) => ({ "pane-1": mappingFor({ worktree }, { lifecycleState: "provisional", launchCount: 0 }) }) });
    try {
      // The real HOME, so nono finds the OpenCode pack; a state dir without
      // OpenCode's service file, so the refusal cannot come from a host
      // service that happens to run on this machine.
      const { status, stdout } = runBridge(f, "start", "pane-1", { env: { HERDR_NONO_BIN: NONO, HOME: process.env.HOME, XDG_STATE_HOME: path.join(dir, "state") } });
      assert.equal(status, 1, stdout);
      assert.doesNotMatch(stdout, /could not resolve/, "the refusal comes from the resolved allowlist, not from a failed lookup");
      assert.match(stdout, /Not started: the agent's tools can reach localhost \(profile \S+ allows "\*"/);
      assert.equal(f.mappings().panes["pane-1"].lastError.kind, "unconfined");
    } finally {
      f.cleanup();
    }
  });
});

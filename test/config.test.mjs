import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { CONFIG_DEFAULTS, loadConfig, validateConfig } from "../src/config.mjs";

test("defaults run OpenCode, refuse to start next to the host service and verify fail-closed", () => {
  const config = validateConfig({});
  assert.equal(config.agentKind, "opencode");
  assert.equal(config.hostServiceCheck, "refuse");
  assert.equal(config.verifyAfterStart, true);
  assert.equal(config.onVerificationFailure, "stop");
  assert.deepEqual(Object.keys(config).sort(), Object.keys(CONFIG_DEFAULTS).sort());
});

test("unknown keys are rejected with the list of supported keys", () => {
  assert.throws(() => validateConfig({ agentKnd: "opencode" }), (error) => error.errorKind === "config" && /Unknown config key\(s\): agentKnd/.test(error.message));
});

const invalid = [
  [{ agentKind: "" }, /agentKind/],
  [{ agentArgs: { opencode: "x" } }, /agentArgs/],
  [{ resumeArgs: { opencode: [1] } }, /resumeArgs/],
  [{ allowPaths: ["relative/dir"] }, /allowPaths/],
  [{ readPaths: [""] }, /readPaths/],
  [{ agentEnv: ["NOVALUE"] }, /agentEnv/],
  [{ nonoArgs: ["--", "x"] }, /nonoArgs/],
  [{ nonoArgs: ["--allow-domain", "localhost"] }, /nonoArgs.*--allow-domain/],
  [{ nonoArgs: ["--allow-domain=*"] }, /nonoArgs.*--allow-domain=\*/],
  [{ nonoArgs: ["--allow-connect-port", "4096"] }, /nonoArgs/],
  [{ nonoArgs: ["--open-port", "4096"] }, /nonoArgs/],
  [{ nonoArgs: ["--upstream-proxy", "127.0.0.1:4096"] }, /nonoArgs/],
  [{ nonoArgs: ["--network-profile", "developer"] }, /nonoArgs/],
  [{ agentEnv: ["NONO_ALLOW_DOMAIN=localhost"] }, /agentEnv.*NONO_ALLOW_DOMAIN/],
  [{ profile: "" }, /profile/],
  [{ silent: "yes" }, /silent/],
  [{ paneDirection: "left" }, /paneDirection/],
  [{ paneRatio: 1 }, /paneRatio/],
  [{ openIn: "window" }, /openIn/],
  [{ hostServiceCheck: "maybe" }, /hostServiceCheck/],
  [{ onVerificationFailure: "kill" }, /onVerificationFailure/],
  [{ sessionNamePrefix: "-x" }, /sessionNamePrefix/],
];
for (const [raw, pattern] of invalid) {
  test(`validateConfig rejects ${JSON.stringify(raw)}`, () => {
    assert.throws(() => validateConfig(raw), (error) => error.errorKind === "config" && pattern.test(error.message));
  });
}

test("loadConfig resolves the nono executable from config, then HERDR_NONO_BIN, then PATH", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "herdr-nono-config-"));
  assert.equal(loadConfig(dir, {}).nonoBin, "nono");
  assert.equal(loadConfig(dir, { HERDR_NONO_BIN: "/opt/nono" }).nonoBin, "/opt/nono");
  writeFileSync(path.join(dir, "config.json"), JSON.stringify({ nonoBin: "/usr/local/bin/nono" }));
  assert.equal(loadConfig(dir, { HERDR_NONO_BIN: "/opt/nono" }).nonoBin, "/usr/local/bin/nono");
  writeFileSync(path.join(dir, "config.json"), "{ not json");
  assert.throws(() => loadConfig(dir, {}), (error) => error.errorKind === "config" && /not valid JSON/.test(error.message));
  writeFileSync(path.join(dir, "config.json"), "  ");
  assert.equal(loadConfig(dir, {}).agentKind, "opencode", "an empty file means defaults");
});

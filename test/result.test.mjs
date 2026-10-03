import assert from "node:assert/strict";
import { test } from "node:test";
import { RESULT_MARKER } from "../src/constants.mjs";
import { PluginError } from "../src/errors.mjs";
import { failurePayload, formatResultLine, parseResultLine } from "../src/result.mjs";

test("formatResultLine and parseResultLine round-trip", () => {
  const line = formatResultLine({ action: "info", ok: true, sessionName: "s" });
  assert.ok(line.startsWith(`${RESULT_MARKER} {`));
  assert.deepEqual(parseResultLine(`noise\n${line}\nmore`), { schemaVersion: 1, plugin: "nono.sandbox", action: "info", ok: true, sessionName: "s" });
  assert.equal(parseResultLine("nothing here"), null);
});

test("failurePayload carries the error kind, trimmed output and extra payload fields", () => {
  const payload = failurePayload("stop", new PluginError("not-found", "gone", { output: "  nono: Session not found  " }));
  assert.deepEqual(payload, { action: "stop", ok: false, errorKind: "not-found", message: "gone", output: "nono: Session not found" });
  assert.deepEqual(failurePayload(null, new Error("boom")), { action: null, ok: false, errorKind: "unknown", message: "boom" });
  const withReport = failurePayload("verify-sandbox", new PluginError("unconfined", "bad", { payload: { verified: false, ok: true } }));
  assert.equal(withReport.verified, false);
  assert.equal(withReport.ok, false, "the payload cannot override ok");
  assert.equal(withReport.errorKind, "unconfined");
});

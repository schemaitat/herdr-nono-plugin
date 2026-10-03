/**
 * Runs inside the Herdr pane. Modes: `start` (first launch of the agent),
 * `connect` (launch it again with the adapter's resume arguments) and `shell`
 * (a shell in a sandbox with the agent's policy).
 * Invoked through `src/bridge.mjs <mode> --state-dir DIR --config-dir DIR --pane-id ID --plugin-root DIR [--herdr-bin BIN] [--nono-bin BIN] [--launch-id ID]`.
 * @module bridge-main
 */
import { loadConfig } from "./config.mjs";
import { PluginError, errorMessageOf } from "./errors.mjs";
import { createHerdrClient } from "./herdr.mjs";
import { createLifecycle } from "./lifecycle.mjs";
import { createNonoClient } from "./nono.mjs";

const MODES = new Set(["start", "connect", "shell"]);

/**
 * Parses the bridge argv.
 * @param {string[]} argv
 * @returns {{mode: string, stateDir: string, configDir: string, paneId: string, pluginRoot: string, herdrBin: string, nonoBin: string|null, launchId: string|null}}
 */
export function parseBridgeArgs(argv) {
  const [mode, ...rest] = argv;
  if (!MODES.has(mode)) {
    throw new PluginError("startup", `Unknown bridge mode "${mode}". Expected one of ${[...MODES].join(", ")}.`);
  }
  const options = { stateDir: null, configDir: null, paneId: null, pluginRoot: null, herdrBin: "herdr", nonoBin: null, launchId: null };
  const names = { "--state-dir": "stateDir", "--config-dir": "configDir", "--pane-id": "paneId", "--plugin-root": "pluginRoot", "--herdr-bin": "herdrBin", "--nono-bin": "nonoBin", "--launch-id": "launchId" };
  for (let index = 0; index < rest.length; index += 2) {
    const key = names[rest[index]];
    const value = rest[index + 1];
    if (!key || value === undefined) {
      throw new PluginError("startup", `Unexpected bridge argument "${rest[index]}".`);
    }
    options[key] = value;
  }
  for (const key of ["stateDir", "configDir", "paneId", "pluginRoot"]) {
    if (!options[key]) {
      throw new PluginError("startup", `Missing bridge option for ${key}.`);
    }
  }
  return { mode, ...options };
}

/**
 * Entry point; resolves to the process exit code.
 * @param {string[]} argv
 * @param {NodeJS.ProcessEnv} [env]
 * @returns {Promise<number>}
 */
export async function runBridge(argv, env = process.env) {
  const log = (line) => process.stdout.write(`${line}\n`);
  try {
    const { mode, stateDir, configDir, paneId, pluginRoot, herdrBin, nonoBin, launchId } = parseBridgeArgs(argv);
    const config = loadConfig(configDir, env);
    if (nonoBin) {
      // The action already resolved the executable; the pane's shell may not have the same environment.
      config.nonoBin = nonoBin;
    }
    const herdr = createHerdrClient({ bin: herdrBin, env });
    const nono = createNonoClient({ bin: config.nonoBin, env });
    const lifecycle = createLifecycle({ stateDir, config, pluginRoot, nono, log, herdr, env });
    if (mode === "shell") {
      return (await lifecycle.shell(paneId)).exitCode;
    }
    // Actions wait for this acknowledgement, and the pid it records marks the
    // mapping busy until this process gives it back on the way out.
    lifecycle.acknowledgeBridge(paneId, launchId);
    try {
      return (await lifecycle.launch(paneId, { mode: mode === "connect" ? "connect" : "start" })).exitCode;
    } finally {
      lifecycle.releaseBridge(paneId);
    }
  } catch (error) {
    log(`error: ${errorMessageOf(error)}`);
    const output = /** @type {any} */ (error)?.output;
    if (typeof output === "string" && output.trim() !== "") {
      log(output.trim());
    }
    return 1;
  }
}

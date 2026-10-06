/**
 * Shared fixture helpers: temp state/config/worktree directories, fake CLIs,
 * and runners that execute the plugin scripts as real child processes.
 */
import { spawn, spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { loadState, parseResultLine, savePaneEntry } from "./support/state.mjs";

/** Absolute plugin root. */
export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/**
 * The plugin binary the black-box runners start: HERDR_NONO_BINARY, else the
 * debug build (cargo build).
 */
export const BINARY = process.env.HERDR_NONO_BINARY || path.join(ROOT, "target", "debug", "herdr-nono");

/**
 * The command and arguments that run one entry point of the plugin.
 * @param {"action"|"bridge"|"events"|"pane"} entry
 * @param {string[]} [args]
 * @returns {[string, string[]]}
 */
export function entryCommand(entry, args = []) {
  return [BINARY, [entry, ...args]];
}

/**
 * The command and arguments that run the overlay pane entry point.
 * @param {string[]} [args]
 * @returns {[string, string[]]}
 */
export function paneCommand(args = []) {
  return entryCommand("pane", args);
}

/**
 * The words a typed bridge command starts with, for a mode.
 * @param {string} mode
 */
export function bridgeInvocation(mode) {
  return `${BINARY} bridge ${mode}`;
}

export const FAKE_NONO = path.join(ROOT, "test", "fakes", "nono.mjs");
export const FAKE_HERDR = path.join(ROOT, "test", "fakes", "herdr.mjs");
export const FAKE_BIN = path.join(ROOT, "test", "fakes", "bin");

for (const fake of [FAKE_NONO, FAKE_HERDR, path.join(FAKE_BIN, "opencode")]) {
  try {
    chmodSync(fake, 0o755);
  } catch (error) {
    // A read-only checkout cannot change modes; git already stores the fakes as executable.
    if (error.code !== "EROFS" && error.code !== "EPERM") {
      throw error;
    }
  }
}

/**
 * Runs git in a directory and throws on failure.
 * @param {string} cwd
 * @param {string[]} args
 */
export function git(cwd, args) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", env: { ...process.env, GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@example.com", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@example.com" } });
  if (result.status !== 0) {
    throw new Error(`git ${args.join(" ")} failed: ${result.stderr}`);
  }
  return result.stdout;
}

/**
 * Reads a JSON-lines log, returning [] when the file does not exist.
 * @param {string} file
 */
export function readJsonLines(file) {
  if (!existsSync(file)) {
    return [];
  }
  return readFileSync(file, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
}

/**
 * Creates an isolated fixture. The fake home directory has no OpenCode
 * service file, so no host service is detected unless a test writes one.
 * @param {{config?: Record<string, unknown>|null, sessions?: Array<Record<string, unknown>>, panes?: Record<string, Record<string, unknown>>|((paths: {root: string, stateDir: string, configDir: string, worktree: string}) => Record<string, Record<string, unknown>>)}} [options]
 */
export function createFixture({ config = {}, sessions = [], panes = {} } = {}) {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "herdr-nono-test-")));
  const stateDir = path.join(root, "state");
  const configDir = path.join(root, "config");
  const worktree = path.join(root, "worktree");
  const home = path.join(root, "home");
  for (const dir of [stateDir, configDir, worktree, home]) {
    mkdirSync(dir, { recursive: true });
  }
  git(worktree, ["init", "-q"]);
  writeFileSync(path.join(worktree, "README.md"), "fixture\n");
  git(worktree, ["add", "README.md"]);
  git(worktree, ["commit", "-q", "-m", "init"]);
  if (config !== null) {
    writeFileSync(path.join(configDir, "config.json"), JSON.stringify(config, null, 2));
  }
  const nonoState = path.join(root, "nono-state.json");
  writeFileSync(nonoState, JSON.stringify({ sessions }, null, 2));
  const paneEntries = typeof panes === "function" ? panes({ root, stateDir, configDir, worktree }) : panes;
  for (const [paneId, entry] of Object.entries(paneEntries)) {
    savePaneEntry(stateDir, paneId, entry);
  }
  const nonoLog = path.join(root, "nono.log");
  const herdrLog = path.join(root, "herdr.log");
  const agentLog = path.join(root, "agent.log");
  return {
    root,
    home,
    stateDir,
    configDir,
    worktree,
    nonoState,
    env(overrides = {}) {
      return {
        PATH: `${FAKE_BIN}:${path.dirname(process.execPath)}:/usr/bin:/bin`,
        HOME: home,
        XDG_RUNTIME_DIR: path.join(root, "runtime"),
        HERDR_PLUGIN_ROOT: ROOT,
        HERDR_PLUGIN_STATE_DIR: stateDir,
        HERDR_PLUGIN_CONFIG_DIR: configDir,
        HERDR_PLUGIN_ID: "nono.sandbox",
        HERDR_BIN_PATH: FAKE_HERDR,
        HERDR_SOCKET_PATH: path.join(root, "herdr.sock"),
        HERDR_NONO_BIN: FAKE_NONO,
        FAKE_NONO_LOG: nonoLog,
        FAKE_NONO_STATE: nonoState,
        FAKE_HERDR_LOG: herdrLog,
        FAKE_AGENT_LOG: agentLog,
        HERDR_NONO_BRIDGE_START_TIMEOUT_MS: "300",
        HERDR_NONO_VERIFY_WINDOW_MS: "1500",
        FAKE_HERDR_BRIDGE_STARTS: "1",
        ...overrides,
      };
    },
    nonoCalls() {
      return readJsonLines(nonoLog);
    },
    nonoRuns() {
      return readJsonLines(nonoLog).filter((call) => call.argv[0] === "run");
    },
    herdrCalls() {
      return readJsonLines(herdrLog).map((entry) => entry.argv);
    },
    agentRuns() {
      return readJsonLines(agentLog);
    },
    sessions() {
      return JSON.parse(readFileSync(nonoState, "utf8")).sessions;
    },
    mappings() {
      return loadState(stateDir);
    },
    cleanup() {
      rmSync(root, { recursive: true, force: true });
    },
  };
}

/**
 * Runs `herdr-nono action` for an action id.
 * @param {ReturnType<typeof createFixture>} fixture
 * @param {string} actionId
 * @param {{context?: Record<string, unknown>, env?: Record<string, string>}} [options]
 */
export function runAction(fixture, actionId, { context = {}, env = {} } = {}) {
  const [command, args] = entryCommand("action");
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: "utf8",
    env: fixture.env({ HERDR_PLUGIN_ACTION_ID: actionId, HERDR_PLUGIN_CONTEXT_JSON: JSON.stringify(context), ...env }),
  });
  return { status: result.status, stdout: result.stdout, stderr: result.stderr, result: parseResultLine(result.stdout) };
}

/**
 * Runs `herdr-nono bridge` in a mode for a pane.
 * @param {ReturnType<typeof createFixture>} fixture
 * @param {string} mode
 * @param {string} paneId
 * @param {{env?: Record<string, string>, args?: string[]}} [options]
 */
export function runBridge(fixture, mode, paneId, { env = {}, args = [] } = {}) {
  const [command, commandArgs] = entryCommand("bridge", [mode, "--state-dir", fixture.stateDir, "--config-dir", fixture.configDir, "--pane-id", paneId, "--plugin-root", ROOT, "--herdr-bin", FAKE_HERDR, ...args]);
  const result = spawnSync(command, commandArgs, {
    cwd: ROOT,
    encoding: "utf8",
    env: fixture.env(env),
    timeout: 30_000,
  });
  return { status: result.status, stdout: result.stdout, stderr: result.stderr };
}

/**
 * Runs `herdr-nono events` with an event payload.
 * @param {ReturnType<typeof createFixture>} fixture
 * @param {string} eventName
 * @param {Record<string, unknown>} payload
 * @param {{env?: Record<string, string>}} [options]
 */
export function runEvent(fixture, eventName, payload, { env = {} } = {}) {
  const [command, args] = entryCommand("events");
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: "utf8",
    env: fixture.env({ HERDR_PLUGIN_EVENT: eventName, HERDR_PLUGIN_EVENT_JSON: JSON.stringify(payload), ...env }),
  });
  return { status: result.status, stdout: result.stdout, stderr: result.stderr };
}

function fakeProcess(words) {
  const child = spawn(process.execPath, ["-e", "setTimeout(() => {}, 60000)", ...words], { cwd: ROOT, stdio: "ignore" });
  child.unref();
  return { pid: /** @type {number} */ (child.pid), stop: () => { try { child.kill("SIGKILL"); } catch { /* already gone */ } } };
}

/**
 * Starts a process whose command line looks like a bridge for `paneId` (it
 * only sleeps), so tests can mark a mapping busy with a pid that passes the
 * identity check. Call `stop()` when done.
 * @param {string} paneId
 */
export function fakeBridgeProcess(paneId) {
  return fakeProcess([path.join(ROOT, "bin", "herdr-nono"), "bridge", "connect", "--pane-id", paneId]);
}

/**
 * Starts a process whose command line looks like an open-shell bridge for `paneId`.
 * @param {string} paneId
 */
export function fakeShellProcess(paneId) {
  return fakeProcess([path.join(ROOT, "bin", "herdr-nono"), "bridge", "shell", "--pane-id", paneId]);
}

/**
 * A mapping entry with sensible defaults for tests.
 * @param {ReturnType<typeof createFixture>} fixture
 * @param {Record<string, unknown>} [overrides]
 */
export function mappingFor(fixture, overrides = {}) {
  return {
    paneId: "pane-1",
    sessionName: "herdr-opencode-abc123def456",
    workspaceId: "w1",
    agentKind: "opencode",
    localPath: fixture.worktree,
    workdir: fixture.worktree,
    profile: path.join(ROOT, "profiles", "herdr-opencode-client.json"),
    serverProfile: path.join(ROOT, "profiles", "herdr-opencode-server.json"),
    lifecycleState: "exited",
    createdAt: "2026-09-29T00:00:00.000Z",
    sourcePaneId: "pane-0",
    launchCount: 1,
    lastError: null,
    verification: null,
    ...overrides,
  };
}

/** The shipped nono profiles. */
export const SHIPPED_PROFILES = {
  client: path.join(ROOT, "profiles", "herdr-opencode-client.json"),
  server: path.join(ROOT, "profiles", "herdr-opencode-server.json"),
};

let described = null;

/**
 * The facts `herdr-nono describe` prints: action ids, config keys, error kinds,
 * the version, the loopback names the probe tries and the built-in agents.
 * @returns {Record<string, any>}
 */
export function describeBinary() {
  if (described === null) {
    const result = spawnSync(BINARY, ["describe"], { encoding: "utf8" });
    if (result.status !== 0) throw new Error(`herdr-nono describe failed (is the binary built? cargo build): ${result.stderr}`);
    described = JSON.parse(result.stdout);
  }
  return described;
}

/**
 * Runs the escape probe in a throwaway sandbox through the binary
 * (`herdr-nono probe-run`) and returns `{targets, checks}`.
 * @param {{nonoBin: string, profile: string, env?: NodeJS.ProcessEnv}} input
 * @returns {Promise<{targets: Record<string, any>, checks: Array<{check: string, result: string, ok: boolean, severity: string, why: string}>}>}
 */
export function runProbes({ nonoBin, profile, env = process.env }) {
  return new Promise((resolve, reject) => {
    const child = spawn(BINARY, ["probe-run", "--nono-bin", nonoBin, "--profile", profile], { env, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("close", (status) => (status === 0 ? resolve(JSON.parse(stdout)) : reject(new Error(stderr.trim() || `probe-run exited ${status}`))));
  });
}

/**
 * What the plugin makes of a profile nono resolves (`herdr-nono profile-summary`).
 * @param {string} nonoBin
 * @param {string} profile
 * @returns {Record<string, any>}
 */
export function summarizeProfile(nonoBin, profile) {
  const result = spawnSync(BINARY, ["profile-summary", "--nono-bin", nonoBin, profile], { encoding: "utf8" });
  if (result.status !== 0) throw new Error(`profile-summary failed: ${result.stderr}`);
  return JSON.parse(result.stdout);
}

/** What the binary puts before every sandboxed command: nono appends the pack profile's `--standalone`, and this drops it again (see rust/src/nono.rs). */
export const WRAP = ["sh", "-c", 'for a in "$@"; do shift; [ "$a" = "--standalone" ] || set -- "$@" "$a"; done; exec "$@"', "herdr-nono"];

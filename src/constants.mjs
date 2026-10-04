/**
 * Shared constants for the nono.sandbox plugin.
 * @module constants
 */

/** Plugin id declared in herdr-plugin.toml; used when HERDR_PLUGIN_ID is absent. */
export const PLUGIN_ID = "nono.sandbox";

/**
 * Prefix of the machine-readable first stdout line every action prints. It is
 * the same marker the Docker Sandboxes and Vercel plugins print, so an
 * orchestrator can switch backends without changing its parser.
 */
export const RESULT_MARKER = "HERDR_SANDBOX_RESULT:";

/** Schema version of the result line payload. */
export const RESULT_SCHEMA_VERSION = 1;

/** Directory inside HERDR_PLUGIN_STATE_DIR holding one JSON file per mapped pane. */
export const PANES_DIR = "panes";

/** Schema version written into every pane mapping file. */
export const STATE_VERSION = 1;

/** File name of the user configuration inside HERDR_PLUGIN_CONFIG_DIR. */
export const CONFIG_FILE = "config.json";

/** Source id used when the plugin reports agent state to Herdr on behalf of an agent Herdr cannot detect. */
export const AGENT_REPORT_SOURCE = "nono.sandbox";

/** Environment variable that overrides the nono executable. */
export const NONO_BIN_ENV = "HERDR_NONO_BIN";

/** Oldest nono release whose CLI surface this plugin was written against. */
export const MIN_NONO_VERSION = "0.78.0";

/** How long a captured nono call (version, ps, stop, profile show) may run before it is killed. */
export const NONO_CALL_TIMEOUT_MS = 30_000;

/** Environment variable overriding {@link NONO_CALL_TIMEOUT_MS} (milliseconds). */
export const NONO_CALL_TIMEOUT_ENV = "HERDR_NONO_TIMEOUT_MS";

/** How long an action waits for the bridge it typed into a pane to update the mapping before it opens a new pane instead. */
export const BRIDGE_START_TIMEOUT_MS = 4000;

/** Environment variable overriding {@link BRIDGE_START_TIMEOUT_MS}; used by tests. */
export const BRIDGE_START_TIMEOUT_ENV = "HERDR_NONO_BRIDGE_START_TIMEOUT_MS";

/** How long a process waits for another live process to release a mapping lock. */
export const LOCK_WAIT_MS = 5000;

/** Environment variable overriding {@link LOCK_WAIT_MS} (milliseconds); used by tests. */
export const LOCK_WAIT_ENV = "HERDR_NONO_LOCK_WAIT_MS";

/** How long the key binding installer may take, including Herdr's config check and reload. */
export const KEYBINDING_INSTALL_TIMEOUT_MS = 30_000;

/** How long the bridge keeps checking a freshly launched session before it records the verification. */
export const VERIFY_WINDOW_MS = 20_000;

/** Environment variable overriding {@link VERIFY_WINDOW_MS}; used by tests. */
export const VERIFY_WINDOW_ENV = "HERDR_NONO_VERIFY_WINDOW_MS";

/** Pause between two verification attempts while the agent is still starting its server. */
export const VERIFY_INTERVAL_MS = 1000;

/** How long the bridge waits for an agent's private server to answer before it gives up. */
export const SERVER_READY_TIMEOUT_MS = 30_000;

/** Environment variable overriding {@link SERVER_READY_TIMEOUT_MS}; used by tests. */
export const SERVER_READY_TIMEOUT_ENV = "HERDR_NONO_SERVER_READY_TIMEOUT_MS";

/** How long a stopped server gets to exit on SIGTERM before it is killed. */
export const SERVER_STOP_GRACE_MS = 5000;

/** Directory inside HERDR_PLUGIN_STATE_DIR holding the private servers' logs. */
export const LOGS_DIR = "logs";

/** Suffix of the nono session name an agent's private server runs under. */
export const SERVER_SESSION_SUFFIX = "-server";

/** How long `stop` waits for the bridge to notice that its session ended. */
export const STOP_WAIT_MS = 15_000;

/**
 * Environment variables `nono run` reads as flags that grant network or
 * filesystem access beyond the profile. `agentEnv` may not set them either.
 */
export const NONO_RUN_ENV_PATTERNS = Object.freeze([
  /^NONO_ALLOW$/,
  /^NONO_ALLOW_DOMAIN$/,
  /^NONO_NETWORK_PROFILE$/,
  /^NONO_UPSTREAM_PROXY$/,
  /^NONO_UPSTREAM_BYPASS$/,
  /^NONO_CAPABILITY_ELEVATION$/,
  /^NONO_TRUST_OVERRIDE$/,
  /^NONO_PROFILE$/,
]);

/**
 * `nono run` flags that `nonoArgs` may not contain: they open network paths
 * the profile does not grant (localhost ports, more domains, another proxy) or
 * grant the current directory. Network access belongs in the server profile,
 * where `doctor` checks it.
 */
export const FORBIDDEN_NONO_ARGS = Object.freeze(["--allow-domain", "--network-profile", "--allow-connect-port", "--open-port", "--listen-port", "--upstream-proxy", "--upstream-bypass", "--allow-net", "--allow-cwd", "--profile"]);

/**
 * Environment variables the bridge never hands to `nono run`. The shipped
 * profiles strip them inside the sandbox as well; removing them before nono
 * starts keeps them out even when a user profile does not.
 * `HERDR_*` names the Herdr control socket, which drives every pane on the
 * host; the rest name sockets of agents and buses that run outside the sandbox.
 * The `NONO_*` entries are read by `nono run` itself as flags
 * (`NONO_ALLOW_DOMAIN` is `--allow-domain`) and would widen the profile, for
 * example to an allowed domain that resolves to localhost.
 */
export const STRIPPED_ENV_PATTERNS = Object.freeze([
  /^HERDR_/,
  /^SSH_AUTH_SOCK$/,
  /^SSH_AGENT_PID$/,
  /^GPG_AGENT_INFO$/,
  /^DBUS_SESSION_BUS_ADDRESS$/,
  /^TMUX$/,
  /^TMUX_PANE$/,
  ...NONO_RUN_ENV_PATTERNS,
]);

/** Lifecycle states a pane mapping moves through. */
export const LIFECYCLE_STATES = Object.freeze([
  "provisional",
  "starting",
  "running",
  "exited",
  "failed",
]);

const ESC = String.fromCharCode(27);

/**
 * Escape sequences written after an interactive agent exits so a crashed TUI
 * cannot leave the Herdr pane in mouse-tracking or alternate-screen mode.
 */
export const TERMINAL_RESTORE_SEQUENCE = [
  "[?1000l",
  "[?1002l",
  "[?1003l",
  "[?1004l",
  "[?1006l",
  "[?2004l",
  "[?1049l",
  "[?25h",
  "[0m",
].map((sequence) => ESC + sequence).join("");

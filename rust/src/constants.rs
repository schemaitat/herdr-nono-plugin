//! Shared constants for the nono.sandbox plugin.

/// Plugin id declared in herdr-plugin.toml; used when HERDR_PLUGIN_ID is absent.
pub const PLUGIN_ID: &str = "nono.sandbox";

/// Prefix of the machine-readable first stdout line every action prints. It is
/// the same marker the Docker Sandboxes and Vercel plugins print, so an
/// orchestrator can switch backends without changing its parser.
pub const RESULT_MARKER: &str = "HERDR_SANDBOX_RESULT:";

/// Schema version of the result line payload.
pub const RESULT_SCHEMA_VERSION: u32 = 1;

/// Action ids in the order herdr-plugin.toml declares them.
pub const ACTION_IDS: [&str; 12] = [
    "doctor",
    "install-keybindings",
    "start-agent",
    "reconnect",
    "open-shell",
    "stop",
    "info",
    "verify-sandbox",
    "prune-mappings",
    "sandboxes",
    "list-sandboxes",
    "forget-mapping",
];

/// Directory inside HERDR_PLUGIN_STATE_DIR holding one JSON file per mapped pane.
pub const PANES_DIR: &str = "panes";

/// Schema version written into every pane mapping file.
pub const STATE_VERSION: u32 = 1;

/// File name of the user configuration inside HERDR_PLUGIN_CONFIG_DIR.
pub const CONFIG_FILE: &str = "config.json";

/// Source id used when the plugin reports agent state to Herdr on behalf of an agent Herdr cannot detect.
pub const AGENT_REPORT_SOURCE: &str = "nono.sandbox";

/// Environment variable that overrides the nono executable.
pub const NONO_BIN_ENV: &str = "HERDR_NONO_BIN";

/// Oldest nono release whose CLI surface this plugin was written against.
pub const MIN_NONO_VERSION: &str = "0.78.0";

/// How long a captured nono call (version, ps, stop, profile show) may run before it is killed.
pub const NONO_CALL_TIMEOUT_MS: u64 = 30_000;

/// Environment variable overriding [`NONO_CALL_TIMEOUT_MS`] (milliseconds).
pub const NONO_CALL_TIMEOUT_ENV: &str = "HERDR_NONO_TIMEOUT_MS";

/// How long an action waits for the bridge it typed into a pane to update the mapping before it opens a new pane instead.
pub const BRIDGE_START_TIMEOUT_MS: u64 = 4000;

/// Environment variable overriding [`BRIDGE_START_TIMEOUT_MS`]; used by tests.
pub const BRIDGE_START_TIMEOUT_ENV: &str = "HERDR_NONO_BRIDGE_START_TIMEOUT_MS";

/// How long a process waits for another live process to release a mapping lock.
pub const LOCK_WAIT_MS: u64 = 5000;

/// Environment variable overriding [`LOCK_WAIT_MS`] (milliseconds); used by tests.
pub const LOCK_WAIT_ENV: &str = "HERDR_NONO_LOCK_WAIT_MS";

/// How long the key binding installer may take, including Herdr's config check and reload.
pub const KEYBINDING_INSTALL_TIMEOUT_MS: u64 = 30_000;

/// How long the bridge keeps checking a freshly launched session before it records the verification.
pub const VERIFY_WINDOW_MS: u64 = 20_000;

/// Environment variable overriding [`VERIFY_WINDOW_MS`]; used by tests.
pub const VERIFY_WINDOW_ENV: &str = "HERDR_NONO_VERIFY_WINDOW_MS";

/// Pause between two verification attempts while the agent is still starting its server.
pub const VERIFY_INTERVAL_MS: u64 = 1000;

/// How long the bridge waits for an agent's private server to answer before it gives up.
pub const SERVER_READY_TIMEOUT_MS: u64 = 30_000;

/// Environment variable overriding [`SERVER_READY_TIMEOUT_MS`]; used by tests.
pub const SERVER_READY_TIMEOUT_ENV: &str = "HERDR_NONO_SERVER_READY_TIMEOUT_MS";

/// How long a stopped server gets to exit on SIGTERM before it is killed.
pub const SERVER_STOP_GRACE_MS: u64 = 5000;

/// Directory inside HERDR_PLUGIN_STATE_DIR holding the private servers' logs.
pub const LOGS_DIR: &str = "logs";

/// Suffix of the nono session name an agent's private server runs under.
pub const SERVER_SESSION_SUFFIX: &str = "-server";

/// How long `stop` waits for the bridge to notice that its session ended.
pub const STOP_WAIT_MS: u64 = 15_000;

/// Environment variables `nono run` reads as flags that grant network or
/// filesystem access beyond the profile. `agentEnv` may not set them either.
pub const NONO_RUN_ENV_NAMES: [&str; 8] = [
    "NONO_ALLOW",
    "NONO_ALLOW_DOMAIN",
    "NONO_NETWORK_PROFILE",
    "NONO_UPSTREAM_PROXY",
    "NONO_UPSTREAM_BYPASS",
    "NONO_CAPABILITY_ELEVATION",
    "NONO_TRUST_OVERRIDE",
    "NONO_PROFILE",
];

/// Whether `name` is one of [`NONO_RUN_ENV_NAMES`].
pub fn is_nono_run_env(name: &str) -> bool {
    NONO_RUN_ENV_NAMES.contains(&name)
}

/// `nono run` flags that `nonoArgs` may not contain: they open network paths
/// the profile does not grant (localhost ports, more domains, another proxy) or
/// grant the current directory. Network access belongs in the server profile,
/// where `doctor` checks it.
pub const FORBIDDEN_NONO_ARGS: [&str; 10] = [
    "--allow-domain",
    "--network-profile",
    "--allow-connect-port",
    "--open-port",
    "--listen-port",
    "--upstream-proxy",
    "--upstream-bypass",
    "--allow-net",
    "--allow-cwd",
    "--profile",
];

/// Whether the bridge strips this environment variable before it hands the
/// environment to `nono run`. The shipped profiles strip them inside the
/// sandbox as well; removing them before nono starts keeps them out even when
/// a user profile does not. `HERDR_*` names the Herdr control socket, which
/// drives every pane on the host; the rest name sockets of agents and buses
/// that run outside the sandbox. The `NONO_*` entries are read by `nono run`
/// itself as flags (`NONO_ALLOW_DOMAIN` is `--allow-domain`) and would widen
/// the profile, for example to an allowed domain that resolves to localhost.
pub fn is_stripped_env(name: &str) -> bool {
    name.starts_with("HERDR_")
        || matches!(
            name,
            "SSH_AUTH_SOCK"
                | "SSH_AGENT_PID"
                | "GPG_AGENT_INFO"
                | "DBUS_SESSION_BUS_ADDRESS"
                | "TMUX"
                | "TMUX_PANE"
        )
        || is_nono_run_env(name)
}

/// Lifecycle states a pane mapping moves through.
pub const LIFECYCLE_STATES: [&str; 5] = ["provisional", "starting", "running", "exited", "failed"];

/// Escape sequences written after an interactive agent exits so a crashed TUI
/// cannot leave the Herdr pane in mouse-tracking or alternate-screen mode.
pub const TERMINAL_RESTORE_SEQUENCE: &str = concat!(
    "\x1b[?1000l",
    "\x1b[?1002l",
    "\x1b[?1003l",
    "\x1b[?1004l",
    "\x1b[?1006l",
    "\x1b[?2004l",
    "\x1b[?1049l",
    "\x1b[?25h",
    "\x1b[0m",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stripped_env_covers_herdr_sockets_agents_and_nono_flags() {
        for name in [
            "HERDR_SOCKET_PATH",
            "HERDR_",
            "SSH_AUTH_SOCK",
            "TMUX",
            "TMUX_PANE",
            "NONO_ALLOW_DOMAIN",
            "NONO_PROFILE",
        ] {
            assert!(is_stripped_env(name), "{name}");
        }
        for name in [
            "HOME",
            "PATH",
            "MYHERDR_X",
            "TMUXX",
            "NONO_CAP_FILE",
            "XSSH_AUTH_SOCK",
        ] {
            assert!(!is_stripped_env(name), "{name}");
        }
    }

    #[test]
    fn terminal_restore_sequence_matches_the_js_bytes() {
        assert_eq!(
            TERMINAL_RESTORE_SEQUENCE,
            "\u{1b}[?1000l\u{1b}[?1002l\u{1b}[?1003l\u{1b}[?1004l\u{1b}[?1006l\u{1b}[?2004l\u{1b}[?1049l\u{1b}[?25h\u{1b}[0m"
        );
    }
}

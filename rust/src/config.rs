//! User configuration stored as JSON in HERDR_PLUGIN_CONFIG_DIR/config.json.
//! Unknown keys are rejected so typos never silently fall back to defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::constants::{is_nono_run_env, CONFIG_FILE, FORBIDDEN_NONO_ARGS, NONO_BIN_ENV};
use crate::context::Env;
use crate::errors::{ErrorKind, PluginError, Result};

/// Directions accepted for the agent pane split.
pub const PANE_DIRECTIONS: [&str; 2] = ["right", "down"];

/// Where start-agent puts the agent: a split next to the focused pane or a new tab.
pub const OPEN_MODES: [&str; 2] = ["split", "tab"];

/// What the bridge does when an unsandboxed OpenCode background service is running on the host.
pub const HOST_SERVICE_CHECKS: [&str; 3] = ["warn", "refuse", "off"];

/// What the bridge does when the verification of a freshly started session fails.
pub const VERIFICATION_FAILURE_ACTIONS: [&str; 2] = ["stop", "warn"];

/// Every supported key, in the order docs and error messages list them.
pub const CONFIG_KEYS: [&str; 22] = [
    "agentKind",
    "agentArgs",
    "resumeArgs",
    "agentEnv",
    "customAgents",
    "profile",
    "serverProfile",
    "allowPaths",
    "readPaths",
    "nonoArgs",
    "nonoBin",
    "silent",
    "shell",
    "paneDirection",
    "paneRatio",
    "openIn",
    "reportAgentStatus",
    "sessionNamePrefix",
    "cleanupOnWorktreeRemoved",
    "hostServiceCheck",
    "verifyAfterStart",
    "onVerificationFailure",
];

/// The validated user configuration: every key of [`CONFIG_KEYS`] with the type
/// docs/reference/configuration.md documents for it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginConfig {
    pub agent_kind: String,
    pub agent_args: BTreeMap<String, Vec<String>>,
    pub resume_args: BTreeMap<String, Vec<String>>,
    pub agent_env: Vec<String>,
    pub custom_agents: Map<String, Value>,
    pub profile: Option<String>,
    pub server_profile: Option<String>,
    pub allow_paths: Vec<String>,
    pub read_paths: Vec<String>,
    pub nono_args: Vec<String>,
    /// `None` until [`load_config`] resolves the executable.
    pub nono_bin: Option<String>,
    pub silent: bool,
    pub shell: String,
    pub pane_direction: String,
    pub pane_ratio: f64,
    pub open_in: String,
    pub report_agent_status: bool,
    pub session_name_prefix: String,
    pub cleanup_on_worktree_removed: bool,
    pub host_service_check: String,
    pub verify_after_start: bool,
    pub on_verification_failure: String,
}

/// Every supported key with its default value, in [`CONFIG_KEYS`] order.
pub fn config_defaults() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "agentKind": "opencode",
        "agentArgs": {},
        "resumeArgs": {},
        "agentEnv": [],
        "customAgents": {},
        "profile": null,
        "serverProfile": null,
        "allowPaths": [],
        "readPaths": [],
        "nonoArgs": [],
        "nonoBin": null,
        "silent": false,
        "shell": "bash",
        "paneDirection": "right",
        "paneRatio": 0.5,
        "openIn": "split",
        "reportAgentStatus": true,
        "sessionNamePrefix": "herdr",
        "cleanupOnWorktreeRemoved": true,
        "hostServiceCheck": "refuse",
        "verifyAfterStart": true,
        "onVerificationFailure": "stop",
    }) else {
        unreachable!("a JSON object literal")
    };
    map
}

impl Default for PluginConfig {
    fn default() -> Self {
        validate_config(&Value::Object(Map::new())).expect("the defaults are valid")
    }
}

/// Returns the config file path for a config directory.
pub fn config_path(config_dir: &Path) -> PathBuf {
    config_dir.join(CONFIG_FILE)
}

fn config_error(message: String) -> PluginError {
    PluginError::new(ErrorKind::Config, message)
}

fn fail(key: &str, expectation: &str) -> PluginError {
    config_error(format!("Config key \"{key}\" {expectation}."))
}

fn string_array(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| {
            item.as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        })
        .collect()
}

/// `/^[A-Za-z_][A-Za-z0-9_]*=.*$/`: `.` does not match line terminators.
fn is_env_entry(entry: &str) -> bool {
    let Some((name, value)) = entry.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !value.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
}

fn args_by_kind(value: &Value) -> Option<BTreeMap<String, Vec<String>>> {
    value
        .as_object()?
        .iter()
        .map(|(kind, args)| {
            let words: Option<Vec<String>> = args
                .as_array()?
                .iter()
                .map(|word| word.as_str().map(str::to_string))
                .collect();
            Some((kind.clone(), words?))
        })
        .collect()
}

fn enum_value(key: &str, value: &Value, allowed: &[&str]) -> Result<String> {
    match value.as_str() {
        Some(text) if allowed.contains(&text) => Ok(text.to_string()),
        _ => Err(fail(key, &format!("must be one of {}", allowed.join(", ")))),
    }
}

fn boolean(key: &str, value: &Value) -> Result<bool> {
    value
        .as_bool()
        .ok_or_else(|| fail(key, "must be true or false"))
}

fn optional_string(key: &str, value: &Value) -> Result<Option<String>> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) if !text.is_empty() => Ok(Some(text.clone())),
        _ => Err(fail(key, "must be a non-empty string or null")),
    }
}

/// Validates a raw config object and merges it over the defaults.
pub fn validate_config(raw: &Value) -> Result<PluginConfig> {
    let Some(raw) = raw.as_object() else {
        return Err(config_error(
            "config.json must contain a JSON object.".into(),
        ));
    };
    let unknown: Vec<&str> = raw
        .keys()
        .map(String::as_str)
        .filter(|key| !CONFIG_KEYS.contains(key))
        .collect();
    if !unknown.is_empty() {
        return Err(config_error(format!(
            "Unknown config key(s): {}. Supported keys: {}.",
            unknown.join(", "),
            CONFIG_KEYS.join(", ")
        )));
    }
    let mut merged = config_defaults();
    for (key, value) in raw {
        merged.insert(key.clone(), value.clone());
    }
    let get = |key: &str| merged.get(key).expect("every key is merged");

    let agent_kind = match get("agentKind").as_str() {
        Some(kind) if !kind.is_empty() => kind.to_string(),
        _ => return Err(fail("agentKind", "must be a non-empty string")),
    };
    let mut kind_args = Vec::new();
    for key in ["agentArgs", "resumeArgs"] {
        kind_args.push(
            args_by_kind(get(key))
                .ok_or_else(|| fail(key, "must map agent kinds to arrays of strings"))?,
        );
    }
    let resume_args = kind_args.pop().expect("two entries");
    let agent_args = kind_args.pop().expect("two entries");
    let custom_agents = get("customAgents")
        .as_object()
        .cloned()
        .ok_or_else(|| fail("customAgents", "must be an object keyed by agent kind"))?;
    let nono_args = string_array(get("nonoArgs"))
        .ok_or_else(|| fail("nonoArgs", "must be an array of non-empty strings"))?;
    let mut paths = Vec::new();
    for key in ["allowPaths", "readPaths"] {
        match string_array(get(key)) {
            Some(list) if list.iter().all(|item| Path::new(item).is_absolute()) => paths.push(list),
            _ => return Err(fail(key, "must be an array of absolute paths")),
        }
    }
    let read_paths = paths.pop().expect("two entries");
    let allow_paths = paths.pop().expect("two entries");
    let agent_env = match string_array(get("agentEnv")) {
        Some(list) if list.iter().all(|item| is_env_entry(item)) => list,
        _ => return Err(fail("agentEnv", "must be an array of KEY=VALUE strings")),
    };
    let nono_env: Vec<&str> = agent_env
        .iter()
        .filter_map(|item| item.split_once('=').map(|(name, _)| name))
        .filter(|name| is_nono_run_env(name))
        .collect();
    if !nono_env.is_empty() {
        return Err(fail(
            "agentEnv",
            &format!(
                "must not set {}: nono reads it as a flag that widens the profile",
                nono_env.join(", ")
            ),
        ));
    }
    let profile = optional_string("profile", get("profile"))?;
    let server_profile = optional_string("serverProfile", get("serverProfile"))?;
    let nono_bin = optional_string("nonoBin", get("nonoBin"))?;
    if nono_args.iter().any(|arg| arg == "--") {
        return Err(fail(
            "nonoArgs",
            "must not contain \"--\"; the plugin adds it before the agent command",
        ));
    }
    let forbidden: Vec<&str> = nono_args
        .iter()
        .map(String::as_str)
        .filter(|arg| FORBIDDEN_NONO_ARGS.contains(&arg.split('=').next().unwrap_or("")))
        .collect();
    if !forbidden.is_empty() {
        return Err(fail(
            "nonoArgs",
            &format!(
                "must not contain {}; network access and the profile belong in the profile files, where doctor checks them",
                forbidden.join(", ")
            ),
        ));
    }
    let silent = boolean("silent", get("silent"))?;
    let report_agent_status = boolean("reportAgentStatus", get("reportAgentStatus"))?;
    let cleanup_on_worktree_removed =
        boolean("cleanupOnWorktreeRemoved", get("cleanupOnWorktreeRemoved"))?;
    let verify_after_start = boolean("verifyAfterStart", get("verifyAfterStart"))?;
    let shell = match get("shell").as_str() {
        Some(shell) if !shell.is_empty() => shell.to_string(),
        _ => return Err(fail("shell", "must be a non-empty string")),
    };
    let pane_direction = enum_value("paneDirection", get("paneDirection"), &PANE_DIRECTIONS)?;
    let open_in = enum_value("openIn", get("openIn"), &OPEN_MODES)?;
    let host_service_check = enum_value(
        "hostServiceCheck",
        get("hostServiceCheck"),
        &HOST_SERVICE_CHECKS,
    )?;
    let on_verification_failure = enum_value(
        "onVerificationFailure",
        get("onVerificationFailure"),
        &VERIFICATION_FAILURE_ACTIONS,
    )?;
    let pane_ratio = match get("paneRatio").as_f64() {
        Some(ratio) if ratio > 0.0 && ratio < 1.0 => ratio,
        _ => {
            return Err(fail(
                "paneRatio",
                "must be a number between 0 and 1 (exclusive)",
            ))
        }
    };
    let session_name_prefix =
        match get("sessionNamePrefix").as_str() {
            Some(prefix) if is_prefix(prefix) => prefix.to_string(),
            _ => return Err(fail(
                "sessionNamePrefix",
                "must start with a letter or digit and contain only letters, digits and hyphens",
            )),
        };
    Ok(PluginConfig {
        agent_kind,
        agent_args,
        resume_args,
        agent_env,
        custom_agents,
        profile,
        server_profile,
        allow_paths,
        read_paths,
        nono_args,
        nono_bin,
        silent,
        shell,
        pane_direction,
        pane_ratio,
        open_in,
        report_agent_status,
        session_name_prefix,
        cleanup_on_worktree_removed,
        host_service_check,
        verify_after_start,
        on_verification_failure,
    })
}

/// `/^[A-Za-z0-9][A-Za-z0-9-]*$/`
fn is_prefix(prefix: &str) -> bool {
    let mut chars = prefix.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// A configuration whose nono executable is resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    pub config: PluginConfig,
    pub nono_bin: String,
}

/// Loads and validates the configuration, resolving the nono executable:
/// `nonoBin` from the file, then `HERDR_NONO_BIN`, then `nono` on PATH.
pub fn load_config(config_dir: &Path, env: &Env) -> Result<LoadedConfig> {
    let file = config_path(config_dir);
    let mut raw = Value::Object(Map::new());
    if file.exists() {
        let text = std::fs::read_to_string(&file).map_err(|error| {
            config_error(format!("Could not read {}: {error}", file.display())).with_cause(error)
        })?;
        if !text.trim().is_empty() {
            raw = serde_json::from_str(&text).map_err(|error| {
                config_error(format!("{} is not valid JSON: {error}", file.display()))
                    .with_cause(error)
            })?;
        }
    }
    let mut config = validate_config(&raw)?;
    let nono_bin = config
        .nono_bin
        .clone()
        .or_else(|| env.get(NONO_BIN_ENV).cloned())
        .unwrap_or_else(|| "nono".to_string());
    config.nono_bin = Some(nono_bin.clone());
    Ok(LoadedConfig { config, nono_bin })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_run_opencode_refuse_to_start_next_to_the_host_service_and_verify_fail_closed() {
        let config = validate_config(&json!({})).unwrap();
        assert_eq!(config.agent_kind, "opencode");
        assert_eq!(config.host_service_check, "refuse");
        assert!(config.verify_after_start);
        assert_eq!(config.on_verification_failure, "stop");
        assert_eq!(config.pane_ratio, 0.5);
        assert_eq!(config, PluginConfig::default());
        let serialized = serde_json::to_value(&config).unwrap();
        let mut keys: Vec<&String> = serialized.as_object().unwrap().keys().collect();
        let mut expected: Vec<String> = CONFIG_KEYS.iter().map(|key| key.to_string()).collect();
        keys.sort();
        expected.sort();
        assert_eq!(keys, expected.iter().collect::<Vec<_>>());
        assert_eq!(
            config_defaults()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            CONFIG_KEYS
        );
    }

    #[test]
    fn unknown_keys_are_rejected_with_the_list_of_supported_keys() {
        let error = validate_config(&json!({"agentKnd": "opencode"})).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Config);
        assert!(
            error.message.contains("Unknown config key(s): agentKnd"),
            "{}",
            error.message
        );
        assert!(error
            .message
            .ends_with(&format!("Supported keys: {}.", CONFIG_KEYS.join(", "))));
    }

    #[test]
    fn a_non_object_is_rejected() {
        assert_eq!(
            validate_config(&json!([])).unwrap_err().message,
            "config.json must contain a JSON object."
        );
    }

    #[test]
    fn invalid_values_are_rejected_naming_the_key() {
        let cases = [
            (json!({"agentKind": ""}), "agentKind"),
            (json!({"agentArgs": {"opencode": "x"}}), "agentArgs"),
            (json!({"resumeArgs": {"opencode": [1]}}), "resumeArgs"),
            (json!({"allowPaths": ["relative/dir"]}), "allowPaths"),
            (json!({"readPaths": [""]}), "readPaths"),
            (json!({"agentEnv": ["NOVALUE"]}), "agentEnv"),
            (json!({"nonoArgs": ["--", "x"]}), "nonoArgs"),
            (
                json!({"nonoArgs": ["--allow-domain", "localhost"]}),
                "nonoArgs\" must not contain --allow-domain",
            ),
            (
                json!({"nonoArgs": ["--allow-domain=*"]}),
                "nonoArgs\" must not contain --allow-domain=*",
            ),
            (
                json!({"nonoArgs": ["--allow-connect-port", "4096"]}),
                "nonoArgs",
            ),
            (json!({"nonoArgs": ["--open-port", "4096"]}), "nonoArgs"),
            (
                json!({"nonoArgs": ["--upstream-proxy", "127.0.0.1:4096"]}),
                "nonoArgs",
            ),
            (
                json!({"nonoArgs": ["--network-profile", "developer"]}),
                "nonoArgs",
            ),
            (
                json!({"agentEnv": ["NONO_ALLOW_DOMAIN=localhost"]}),
                "agentEnv\" must not set NONO_ALLOW_DOMAIN",
            ),
            (json!({"profile": ""}), "profile"),
            (json!({"silent": "yes"}), "silent"),
            (json!({"paneDirection": "left"}), "paneDirection"),
            (json!({"paneRatio": 1}), "paneRatio"),
            (json!({"paneRatio": 0}), "paneRatio"),
            (json!({"paneRatio": "0.5"}), "paneRatio"),
            (json!({"openIn": "window"}), "openIn"),
            (json!({"hostServiceCheck": "maybe"}), "hostServiceCheck"),
            (
                json!({"onVerificationFailure": "kill"}),
                "onVerificationFailure",
            ),
            (json!({"sessionNamePrefix": "-x"}), "sessionNamePrefix"),
            (json!({"agentKind": null}), "agentKind"),
            (json!({"customAgents": []}), "customAgents"),
            (json!({"shell": ""}), "shell"),
            (json!({"agentEnv": ["A=multi\nline"]}), "agentEnv"),
        ];
        for (raw, expected) in cases {
            let error = validate_config(&raw).expect_err(&raw.to_string());
            assert_eq!(error.kind, ErrorKind::Config, "{raw}");
            assert!(error.message.contains(expected), "{raw}: {}", error.message);
        }
    }

    #[test]
    fn messages_match_the_js_wording() {
        let message = |raw: Value| validate_config(&raw).unwrap_err().message.clone();
        assert_eq!(
            message(json!({"paneDirection": "left"})),
            "Config key \"paneDirection\" must be one of right, down."
        );
        assert_eq!(
            message(json!({"profile": ""})),
            "Config key \"profile\" must be a non-empty string or null."
        );
        assert_eq!(
            message(json!({"nonoArgs": ["--"]})),
            "Config key \"nonoArgs\" must not contain \"--\"; the plugin adds it before the agent command."
        );
    }

    #[test]
    fn valid_values_are_accepted() {
        let config = validate_config(&json!({
            "agentKind": "claude",
            "agentArgs": {"claude": ["--x"]},
            "agentEnv": ["A=1", "B="],
            "allowPaths": ["/tmp/x"],
            "profile": "p.json",
            "paneRatio": 0.3,
            "sessionNamePrefix": "team-1",
            "nonoArgs": ["--silent"],
        }))
        .unwrap();
        assert_eq!(config.agent_args["claude"], ["--x"]);
        assert_eq!(config.agent_env, ["A=1", "B="]);
        assert_eq!(config.profile.as_deref(), Some("p.json"));
        assert_eq!(config.pane_ratio, 0.3);
    }

    #[test]
    fn load_config_resolves_the_nono_executable_from_config_then_the_environment_then_path() {
        let dir = tempfile::tempdir().unwrap();
        let with_env = |value: &str| Env::from([(NONO_BIN_ENV.to_string(), value.to_string())]);
        assert_eq!(
            load_config(dir.path(), &Env::new()).unwrap().nono_bin,
            "nono"
        );
        assert_eq!(
            load_config(dir.path(), &with_env("/opt/nono"))
                .unwrap()
                .nono_bin,
            "/opt/nono"
        );
        let file = config_path(dir.path());
        std::fs::write(&file, r#"{"nonoBin":"/usr/local/bin/nono"}"#).unwrap();
        assert_eq!(
            load_config(dir.path(), &with_env("/opt/nono"))
                .unwrap()
                .nono_bin,
            "/usr/local/bin/nono"
        );
        std::fs::write(&file, "{ not json").unwrap();
        let error = load_config(dir.path(), &Env::new()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Config);
        assert!(
            error.message.contains("not valid JSON"),
            "{}",
            error.message
        );
        std::fs::write(&file, "  ").unwrap();
        assert_eq!(
            load_config(dir.path(), &Env::new())
                .unwrap()
                .config
                .agent_kind,
            "opencode",
            "an empty file means defaults"
        );
    }
}

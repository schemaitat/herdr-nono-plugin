//! Agent adapters: which command runs inside the nono sandbox, which nono
//! profile confines it, which label tells Herdr's screen detection what it is
//! looking at, and what the verification looks for in the sandboxed process
//! tree.

use std::ops::Deref;
use std::path::Path;

use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::config::PluginConfig;
use crate::errors::{ErrorKind, PluginError, Result};

/// The private server an agent runs in its own sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSpec {
    pub command: Vec<String>,
    pub profile: String,
    pub password_env: String,
    pub ready_path: String,
    pub user: String,
}

/// One agent adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Adapter {
    pub title: String,
    pub command: Vec<String>,
    pub required_args: Vec<String>,
    pub default_args: Vec<String>,
    pub resume_args: Vec<String>,
    pub herdr_detection_kind: Option<String>,
    pub profile: String,
    pub server_pattern: Option<String>,
    pub host_service: Option<String>,
    pub server: Option<ServerSpec>,
}

fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

/// Built-in adapters keyed by agent kind.
///
/// OpenCode 2.x is a client plus a server; the server runs the model loop and
/// every tool call. Plain `opencode` connects to a background service on the
/// host, which would leave the server, and every tool it runs, outside the
/// sandbox. The plugin therefore starts a private server per pane in its own
/// nono sandbox (`server`), where egress goes through nono's proxy so direct
/// connects to localhost services are denied, and the TUI client in a second,
/// network-blocked sandbox that may reach only that server's port. `{port}` is
/// replaced with the port the plugin picks for each launch. The client's
/// `--server` argument is required, so `agentArgs` cannot point it elsewhere,
/// and the verification looks for the server process inside the server
/// sandbox.
pub fn builtin_agents() -> Vec<(String, Adapter)> {
    vec![(
        "opencode".to_string(),
        Adapter {
            title: "OpenCode".into(),
            command: words(&["opencode"]),
            required_args: words(&["--server", "http://127.0.0.1:{port}"]),
            default_args: Vec::new(),
            resume_args: words(&["--continue"]),
            herdr_detection_kind: Some("opencode".into()),
            profile: "profiles/herdr-opencode-client.json".into(),
            server_pattern: Some("\\bserve\\b.*--port {port}\\b".into()),
            host_service: Some("opencode".into()),
            server: Some(ServerSpec {
                command: words(&[
                    "opencode",
                    "serve",
                    "--hostname",
                    "127.0.0.1",
                    "--port",
                    "{port}",
                ]),
                profile: "profiles/herdr-opencode-server.json".into(),
                password_env: "OPENCODE_PASSWORD".into(),
                ready_path: "/api/info".into(),
                user: "opencode".into(),
            }),
        },
    )]
}

/// Replaces `{port}` in a string; `None` leaves it as it is.
pub fn with_port(value: &str, port: Option<u16>) -> String {
    match port {
        Some(port) => value.replace("{port}", &port.to_string()),
        None => value.to_string(),
    }
}

/// Replaces `{port}` in every word of an argv.
pub fn with_port_all(values: &[String], port: Option<u16>) -> Vec<String> {
    values.iter().map(|value| with_port(value, port)).collect()
}

/// Whether `kind` matches `^[a-z0-9][a-z0-9-]*$`.
fn is_kind(kind: &str) -> bool {
    let mut chars = kind.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Compiles a `serverPattern` with `{port}` filled in. The pattern language is
/// Rust's `regex` syntax: the JavaScript-only features (look-around,
/// backreferences) are not available.
pub fn compile_server_pattern(
    pattern: &str,
    port: Option<u16>,
) -> std::result::Result<Regex, regex::Error> {
    // A placeholder digit keeps `{port}` from reading as a repetition when no port is known.
    Regex::new(&with_port(pattern, Some(port.unwrap_or(0))))
}

/// Validates one custom agent profile from `config.customAgents`.
pub fn validate_custom_agent(kind: &str, profile: &Value) -> Result<Adapter> {
    let fail = |reason: &str| {
        PluginError::new(ErrorKind::Config, format!("customAgents.{kind} {reason}."))
    };
    if !is_kind(kind) {
        return Err(fail(
            "has an invalid kind; use lowercase letters, digits and hyphens",
        ));
    }
    let Some(object) = profile.as_object() else {
        return Err(fail("must be an object"));
    };
    const KNOWN: [&str; 8] = [
        "title",
        "command",
        "requiredArgs",
        "defaultArgs",
        "resumeArgs",
        "herdrDetectionKind",
        "profile",
        "serverPattern",
    ];
    let unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !KNOWN.contains(key))
        .collect();
    if !unknown.is_empty() {
        return Err(fail(&format!(
            "has unknown field(s): {}",
            unknown.join(", ")
        )));
    }
    let title = match object.get("title").and_then(Value::as_str) {
        Some(title) if !title.is_empty() => title.to_string(),
        _ => return Err(fail("needs a non-empty title")),
    };
    let strings = |value: &Value| -> Option<Vec<String>> {
        value
            .as_array()?
            .iter()
            .map(|item| item.as_str().map(str::to_string))
            .collect()
    };
    let command = match object.get("command").and_then(strings) {
        Some(command) if !command.is_empty() && !command[0].is_empty() => command,
        _ => {
            return Err(fail(
                "needs command: a non-empty array of strings launched inside the sandbox",
            ))
        }
    };
    let mut lists = Vec::new();
    for name in ["requiredArgs", "defaultArgs", "resumeArgs"] {
        let list = match object.get(name) {
            None => Vec::new(),
            Some(value) => strings(value)
                .ok_or_else(|| fail(&format!("{name} must be an array of strings")))?,
        };
        lists.push(list);
    }
    let resume_args = lists.pop().expect("three lists");
    let default_args = lists.pop().expect("three lists");
    let required_args = lists.pop().expect("three lists");
    let nono_profile = match object.get("profile").and_then(Value::as_str) {
        Some(profile) if !profile.is_empty() => profile.to_string(),
        _ => {
            return Err(fail(
                "needs profile: a nono profile name or the path of a profile JSON file",
            ))
        }
    };
    let herdr_detection_kind = match object.get("herdrDetectionKind") {
        None | Some(Value::Null) => None,
        Some(Value::String(kind)) if !kind.is_empty() => Some(kind.clone()),
        Some(_) => {
            return Err(fail(
                "herdrDetectionKind must be a non-empty string or null",
            ))
        }
    };
    let server_pattern = match object.get("serverPattern") {
        None | Some(Value::Null) => None,
        Some(Value::String(pattern)) if !pattern.is_empty() => {
            if let Err(error) = compile_server_pattern(pattern, None) {
                return Err(fail(&format!(
                    "serverPattern is not a valid regular expression ({error})"
                )));
            }
            Some(pattern.clone())
        }
        Some(_) => {
            return Err(fail(
                "serverPattern must be a non-empty regular expression string or null",
            ))
        }
    };
    Ok(Adapter {
        title,
        command,
        required_args,
        default_args,
        resume_args,
        herdr_detection_kind,
        profile: nono_profile,
        server_pattern,
        host_service: None,
        server: None,
    })
}

/// Every adapter available for a config: built-ins overlaid by custom profiles,
/// in the order the kinds are listed in error messages.
pub fn available_agents(custom_agents: &Map<String, Value>) -> Result<Vec<(String, Adapter)>> {
    let mut agents = builtin_agents();
    for (kind, profile) in custom_agents {
        let adapter = validate_custom_agent(kind, profile)?;
        match agents.iter_mut().find(|(existing, _)| existing == kind) {
            Some(slot) => slot.1 = adapter,
            None => agents.push((kind.clone(), adapter)),
        }
    }
    Ok(agents)
}

/// Turns a profile reference into what `nono run --profile` takes: a name is
/// passed through, a relative `.json` path is resolved against the plugin root
/// (the shipped profiles live in `profiles/`), an absolute path stays as it is.
pub fn resolve_profile_ref(profile: &str, plugin_root: &str) -> String {
    if profile.ends_with(".json") && !Path::new(profile).is_absolute() {
        return Path::new(plugin_root)
            .join(profile)
            .to_string_lossy()
            .into_owned();
    }
    profile.to_string()
}

/// Puts the adapter's required arguments right after the command and drops any
/// copy of a required flag from the configured arguments, together with its
/// value when the required form has one (`--server <url>`), so configuration
/// can never override them.
pub fn with_required_args(
    command: &[String],
    required_args: &[String],
    args: &[String],
) -> Vec<String> {
    let takes_value = |flag: &str| -> Option<bool> {
        let index = required_args
            .iter()
            .position(|arg| arg == flag && arg.starts_with('-'))?;
        Some(
            required_args
                .get(index + 1)
                .is_some_and(|next| !next.starts_with('-')),
        )
    };
    let mut kept = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].split('=').next().unwrap_or("");
        match takes_value(flag) {
            Some(has_value) => {
                if has_value && !args[index].contains('=') {
                    index += 1;
                }
            }
            None => kept.push(args[index].clone()),
        }
        index += 1;
    }
    command
        .iter()
        .chain(required_args)
        .cloned()
        .chain(kept)
        .collect()
}

/// The configured agent with its nono profiles and launch argv (and the argv
/// `reconnect` uses to resume). The argv may still contain `{port}`;
/// [`with_port_all`] fills it in per launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedAgent {
    #[serde(flatten)]
    pub adapter: Adapter,
    pub kind: String,
    pub launch_argv: Vec<String>,
    pub resume_argv: Vec<String>,
    pub profile_ref: String,
    pub server_profile_ref: Option<String>,
}

impl Deref for ResolvedAgent {
    type Target = Adapter;

    fn deref(&self) -> &Adapter {
        &self.adapter
    }
}

/// Resolves the configured agent.
pub fn resolve_agent(config: &PluginConfig, plugin_root: &str) -> Result<ResolvedAgent> {
    let agents = available_agents(&config.custom_agents)?;
    let Some((_, adapter)) = agents.iter().find(|(kind, _)| *kind == config.agent_kind) else {
        let available: Vec<&str> = agents.iter().map(|(kind, _)| kind.as_str()).collect();
        return Err(PluginError::new(
            ErrorKind::Config,
            format!(
                "Unknown agentKind \"{}\". Available: {}.",
                config.agent_kind,
                available.join(", ")
            ),
        ));
    };
    let args = config
        .agent_args
        .get(&config.agent_kind)
        .unwrap_or(&adapter.default_args);
    let resume = config
        .resume_args
        .get(&config.agent_kind)
        .unwrap_or(&adapter.resume_args);
    let launch_argv = with_required_args(&adapter.command, &adapter.required_args, args);
    let resuming: Vec<String> = args
        .iter()
        .cloned()
        .chain(resume.iter().filter(|arg| !args.contains(arg)).cloned())
        .collect();
    let resume_argv = with_required_args(&adapter.command, &adapter.required_args, &resuming);
    let profile_ref = resolve_profile_ref(
        config.profile.as_deref().unwrap_or(&adapter.profile),
        plugin_root,
    );
    let server_profile_ref = adapter.server.as_ref().map(|server| {
        resolve_profile_ref(
            config.server_profile.as_deref().unwrap_or(&server.profile),
            plugin_root,
        )
    });
    Ok(ResolvedAgent {
        adapter: adapter.clone(),
        kind: config.agent_kind.clone(),
        launch_argv,
        resume_argv,
        profile_ref,
        server_profile_ref,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::validate_config;
    use serde_json::json;

    const ROOT: &str = "/plugin";

    fn config(raw: Value) -> PluginConfig {
        validate_config(&raw).unwrap()
    }

    fn owned(items: &[&str]) -> Vec<String> {
        words(items)
    }

    #[test]
    fn opencode_runs_a_client_in_one_sandbox_and_its_private_server_in_another_joined_by_one_port()
    {
        let agent = resolve_agent(&config(json!({"agentKind": "opencode"})), ROOT).unwrap();
        assert_eq!(
            with_port_all(&agent.launch_argv, Some(4242)),
            ["opencode", "--server", "http://127.0.0.1:4242"]
        );
        assert_eq!(
            with_port_all(&agent.resume_argv, Some(4242)),
            [
                "opencode",
                "--server",
                "http://127.0.0.1:4242",
                "--continue"
            ]
        );
        let server = agent.server.as_ref().unwrap();
        assert_eq!(
            with_port_all(&server.command, Some(4242)),
            [
                "opencode",
                "serve",
                "--hostname",
                "127.0.0.1",
                "--port",
                "4242"
            ]
        );
        assert_eq!(
            agent.profile_ref,
            "/plugin/profiles/herdr-opencode-client.json"
        );
        assert_eq!(
            agent.server_profile_ref.as_deref(),
            Some("/plugin/profiles/herdr-opencode-server.json")
        );
        assert_eq!(server.password_env, "OPENCODE_PASSWORD");
        assert_eq!(agent.herdr_detection_kind.as_deref(), Some("opencode"));
        let pattern =
            compile_server_pattern(agent.server_pattern.as_deref().unwrap(), Some(4242)).unwrap();
        assert!(pattern
            .is_match("/home/u/.opencode/bin/opencode serve --hostname 127.0.0.1 --port 4242"));
        assert!(
            !pattern
                .is_match("/home/u/.opencode/bin/opencode serve --hostname 127.0.0.1 --port 42420"),
            "another port is another server"
        );
        assert!(
            !pattern.is_match("/home/u/.opencode/bin/opencode serve --service"),
            "the host background service is not the private server"
        );
    }

    #[test]
    fn agent_args_replace_the_default_arguments_but_can_never_point_the_client_at_another_server() {
        let raw = json!({"agentKind": "opencode", "agentArgs": {"opencode": ["--auto", "--server", "http://127.0.0.1:4096"]}});
        let agent = resolve_agent(&config(raw), ROOT).unwrap();
        assert_eq!(
            agent.launch_argv,
            ["opencode", "--server", "http://127.0.0.1:{port}", "--auto"]
        );
        assert_eq!(
            agent.resume_argv,
            [
                "opencode",
                "--server",
                "http://127.0.0.1:{port}",
                "--auto",
                "--continue"
            ]
        );
        let no_resume = resolve_agent(
            &config(json!({"agentKind": "opencode", "resumeArgs": {"opencode": []}})),
            ROOT,
        )
        .unwrap();
        assert_eq!(
            no_resume.resume_argv,
            ["opencode", "--server", "http://127.0.0.1:{port}"]
        );
        let server_profile = resolve_agent(
            &config(json!({"agentKind": "opencode", "serverProfile": "my-server"})),
            ROOT,
        )
        .unwrap();
        assert_eq!(
            server_profile.server_profile_ref.as_deref(),
            Some("my-server")
        );
    }

    #[test]
    fn a_configured_profile_overrides_the_adapters_names_pass_through_relative_files_resolve_against_the_plugin(
    ) {
        let profile = |value: &str| {
            resolve_agent(
                &config(json!({"agentKind": "opencode", "profile": value})),
                ROOT,
            )
            .unwrap()
            .profile_ref
        };
        assert_eq!(profile("opencode"), "opencode");
        assert_eq!(profile("/etc/p.json"), "/etc/p.json");
        assert_eq!(
            resolve_profile_ref("profiles/x.json", ROOT),
            "/plugin/profiles/x.json"
        );
        assert_eq!(
            resolve_profile_ref("nolabs-ai/opencode", ROOT),
            "nolabs-ai/opencode"
        );
    }

    #[test]
    fn with_required_args_puts_the_required_arguments_first_and_drops_configured_copies() {
        let run = |command: &[&str], required: &[&str], args: &[&str]| {
            with_required_args(&owned(command), &owned(required), &owned(args))
        };
        assert_eq!(run(&["a"], &["-x"], &["-y", "-x"]), ["a", "-x", "-y"]);
        assert_eq!(
            run(
                &["o"],
                &["--server", "U"],
                &["--auto", "--server", "http://elsewhere", "--server=x", "-c"]
            ),
            ["o", "--server", "U", "--auto", "-c"]
        );
        assert_eq!(with_port("p {port}", None), "p {port}");
        assert_eq!(with_port("p {port}", Some(7)), "p 7");
    }

    #[test]
    fn custom_agents_need_a_title_a_command_and_a_profile_and_may_name_a_server_pattern() {
        let agent = validate_custom_agent("claude-code", &json!({"title": "Claude Code", "command": ["claude"], "defaultArgs": ["--dangerously-skip-permissions"], "profile": "nolabs-ai/claude", "herdrDetectionKind": "claude"})).unwrap();
        assert_eq!(
            (agent.server_pattern, agent.host_service, agent.server),
            (None, None, None)
        );
        let custom = json!({"claude-code": {"title": "Claude Code", "command": ["claude"], "profile": "nolabs-ai/claude", "resumeArgs": ["--continue"]}});
        let resolved = resolve_agent(
            &config(json!({"agentKind": "claude-code", "customAgents": custom})),
            ROOT,
        )
        .unwrap();
        assert_eq!(resolved.resume_argv, ["claude", "--continue"]);
        let failures = [
            (json!({"command": ["x"], "profile": "p"}), "title"),
            (json!({"title": "X", "profile": "p"}), "command"),
            (json!({"title": "X", "command": ["x"]}), "profile"),
            (
                json!({"title": "X", "command": ["x"], "profile": "p", "serverPattern": "("}),
                "serverPattern",
            ),
            (
                json!({"title": "X", "command": ["x"], "profile": "p", "sbxAgent": "claude"}),
                "unknown field",
            ),
            (
                json!({"title": "X", "command": [""], "profile": "p"}),
                "command",
            ),
            (
                json!({"title": "X", "command": ["x"], "profile": "p", "requiredArgs": [1]}),
                "requiredArgs must be an array of strings",
            ),
            (
                json!({"title": "X", "command": ["x"], "profile": "p", "defaultArgs": null}),
                "defaultArgs",
            ),
            (
                json!({"title": "X", "command": ["x"], "profile": "p", "herdrDetectionKind": ""}),
                "herdrDetectionKind",
            ),
            (
                json!({"title": "X", "command": ["x"], "profile": "p", "serverPattern": ""}),
                "serverPattern must be",
            ),
            (json!("text"), "must be an object"),
        ];
        for (profile, expected) in failures {
            let error = validate_custom_agent("x", &profile).expect_err(&profile.to_string());
            assert_eq!(error.kind, ErrorKind::Config);
            assert!(
                error.message.contains(expected),
                "{profile}: {}",
                error.message
            );
        }
        let error = validate_custom_agent(
            "Bad Kind",
            &json!({"title": "X", "command": ["x"], "profile": "p"}),
        )
        .unwrap_err();
        assert!(error.message.contains("invalid kind"));
        assert!(validate_custom_agent("ok", &json!({"title": "X", "command": ["x"], "profile": "p", "serverPattern": "\\bserve\\b.*--port {port}\\b"})).is_ok(), "{{port}} is not read as a repetition");
    }

    #[test]
    fn an_unknown_agent_kind_lists_the_available_ones_and_a_custom_kind_may_replace_a_builtin() {
        let error = resolve_agent(&config(json!({"agentKind": "nope"})), ROOT).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Config);
        assert!(
            error.message.contains("Available: opencode."),
            "{}",
            error.message
        );
        assert_eq!(
            builtin_agents()
                .iter()
                .map(|(kind, _)| kind.as_str())
                .collect::<Vec<_>>(),
            ["opencode"]
        );
        let custom = json!({"opencode": {"title": "Mine", "command": ["mine"], "profile": "p"}, "other": {"title": "O", "command": ["o"], "profile": "p"}});
        let agents = available_agents(custom.as_object().unwrap()).unwrap();
        assert_eq!(
            agents
                .iter()
                .map(|(kind, _)| kind.as_str())
                .collect::<Vec<_>>(),
            ["opencode", "other"]
        );
        assert_eq!(agents[0].1.title, "Mine");
        assert!(
            agents[0].1.server.is_none(),
            "the custom adapter replaced the built-in one"
        );
    }

    #[test]
    fn a_resolved_agent_serialises_with_the_adapter_fields_inline() {
        let agent = resolve_agent(&config(json!({})), ROOT).unwrap();
        let value = serde_json::to_value(&agent).unwrap();
        assert_eq!(value["kind"], "opencode");
        assert_eq!(value["title"], "OpenCode");
        assert_eq!(value["server"]["passwordEnv"], "OPENCODE_PASSWORD");
        assert_eq!(
            value["launchArgv"],
            json!(["opencode", "--server", "http://127.0.0.1:{port}"])
        );
    }
}

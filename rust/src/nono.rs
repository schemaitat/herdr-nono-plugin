//! Thin wrapper around the nono CLI. Every call is a direct argv (never a shell
//! string). Captured calls are killed after a timeout so a wedged nono cannot
//! hang an action; the interactive `nono run` lives in the bridge.

use std::net::IpAddr;
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::constants::{NONO_CALL_TIMEOUT_ENV, NONO_CALL_TIMEOUT_MS};
use crate::context::{process_env, Env};
use crate::errors::{ErrorKind, PluginError, Result};
use crate::exec::{run_cli, CancelToken, ExecError, RunOptions};
use crate::util::{js_string, positive_int};

/// Maps nono's error wording to an error kind. nono's exit codes are not
/// documented, so the output decides; anything unrecognised is `unknown` and
/// carries the output.
pub fn classify_failure(output: &str) -> ErrorKind {
    let text = output.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| text.contains(needle));
    if has(&[
        "session not found",
        "profile not found",
        "profile file not found",
        "no such file",
        "not found",
    ]) {
        ErrorKind::NotFound
    } else if has(&[
        "permission denied",
        "operation not permitted",
        "eacces",
        "eperm",
    ]) {
        ErrorKind::Permission
    } else if has(&[
        "invalid",
        "unexpected argument",
        "unrecognized",
        "parse error",
        "validation",
    ]) {
        ErrorKind::Config
    } else {
        ErrorKind::Unknown
    }
}

/// One `nono ps --json` record, normalised to the fields the plugin uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub session_id: String,
    pub name: Option<String>,
    pub supervisor_pid: Option<u32>,
    pub child_pid: Option<u32>,
    pub status: Option<String>,
    pub attachment: Option<String>,
    pub exit_code: Option<i64>,
    pub command: Vec<String>,
    pub profile: Option<String>,
    pub workdir: Option<String>,
    pub network: Option<String>,
    pub started: Option<String>,
}

fn string_field(raw: &Value, key: &str) -> Option<String> {
    raw.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Normalises one `nono ps --json` record.
pub fn normalize_session(raw: &Value) -> Session {
    let session_id = raw
        .get("session_id")
        .filter(|value| !value.is_null())
        .or_else(|| raw.get("id"))
        .filter(|value| !value.is_null());
    Session {
        session_id: session_id.map(js_string).unwrap_or_default(),
        name: string_field(raw, "name"),
        supervisor_pid: positive_int(raw.get("supervisor_pid")),
        child_pid: positive_int(raw.get("child_pid")),
        status: string_field(raw, "status"),
        attachment: string_field(raw, "attachment"),
        exit_code: raw
            .get("exit_code")
            .and_then(Value::as_f64)
            .filter(|code| code.fract() == 0.0)
            .map(|code| code as i64),
        command: raw
            .get("command")
            .and_then(Value::as_array)
            .map_or_else(Vec::new, |words| words.iter().map(js_string).collect()),
        profile: string_field(raw, "profile"),
        workdir: string_field(raw, "workdir"),
        network: string_field(raw, "network"),
        started: string_field(raw, "started"),
    }
}

/// Normalises parsed `nono ps --json` output. `null` is an empty list; anything
/// that is not a list of objects is an error rather than "no sessions", so a
/// format change cannot make running agents look stopped.
pub fn normalize_session_list(parsed: &Value) -> Result<Vec<Session>> {
    if parsed.is_null() {
        return Ok(Vec::new());
    }
    let list = parsed
        .as_array()
        .or_else(|| parsed.get("sessions").and_then(Value::as_array));
    match list {
        Some(list) if list.iter().all(|item| item.is_object() || item.is_array()) => Ok(list
            .iter()
            .map(normalize_session)
            .filter(|session| !session.session_id.is_empty())
            .collect()),
        _ => {
            let shown: String = parsed.to_string().chars().take(2000).collect();
            Err(PluginError::new(
                ErrorKind::Unknown,
                "nono ps --json printed an unexpected shape.",
            )
            .with_output(shown))
        }
    }
}

/// The inputs of [`build_run_args`].
#[derive(Debug, Clone, Default)]
pub struct RunArgs<'a> {
    pub profile: &'a str,
    pub session_name: &'a str,
    pub workspace_root: &'a str,
    pub allow_paths: &'a [String],
    pub read_paths: &'a [String],
    pub silent: bool,
    pub extra_args: &'a [String],
    pub argv: &'a [String],
}

/// Builds the `nono run` argv for one launch. The workspace root is granted
/// read-write with `--allow` (never `--allow-cwd`, which would grant whatever
/// directory the pane happens to be in); the agent command follows `--`.
pub fn build_run_args(input: &RunArgs) -> Result<Vec<String>> {
    if input.argv.is_empty() {
        return Err(PluginError::new(
            ErrorKind::Startup,
            "buildRunArgs needs the command to run inside the sandbox.",
        ));
    }
    let mut args: Vec<String> = vec!["run".into()];
    if input.silent {
        args.push("--silent".into());
    }
    args.extend(
        [
            "--profile",
            input.profile,
            "--name",
            input.session_name,
            "--allow",
            input.workspace_root,
        ]
        .map(String::from),
    );
    for dir in input.allow_paths {
        args.extend(["--allow".to_string(), dir.clone()]);
    }
    for dir in input.read_paths {
        args.extend(["--read".to_string(), dir.clone()]);
    }
    args.extend(input.extra_args.iter().cloned());
    args.push("--".into());
    args.extend(input.argv.iter().cloned());
    Ok(args)
}

/// Public wildcard DNS services whose names resolve to any address written in
/// them, including 127.0.0.1 (`127.0.0.1.nip.io`, `a.localtest.me`).
pub const WILDCARD_DNS_DOMAINS: [&str; 11] = [
    "nip.io",
    "sslip.io",
    "xip.io",
    "localtest.me",
    "lvh.me",
    "vcap.me",
    "lacolhost.com",
    "localho.st",
    "traefik.me",
    "fuf.me",
    "localhost.direct",
];

/// Name suffixes that resolve on the local network or the host itself.
const LOCAL_SUFFIXES: [&str; 8] = [
    "localhost",
    "localdomain",
    "local",
    "internal",
    "lan",
    "home.arpa",
    "home",
    "corp",
];

/// The host name of one `allow_domain` entry: a hostname, a `*.` wildcard, or
/// a URL with a path glob (`https://github.com/org/**`).
pub fn allow_domain_host(entry: &str) -> String {
    let mut host = entry.trim().to_lowercase();
    if host.contains("://") {
        let placeholder = host.replace("://*.", "://wildcard-placeholder.");
        host = match Url::parse(&placeholder)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
        {
            Some(parsed) => parsed
                .strip_prefix("wildcard-placeholder.")
                .map_or(parsed.clone(), |rest| format!("*.{rest}")),
            None => host
                .split("://")
                .nth(1)
                .map_or(host.clone(), str::to_string),
        };
    }
    host = host.split('/').next().unwrap_or("").to_string();
    if let Some(rest) = host.strip_prefix('[') {
        return rest.find(']').map_or(rest, |end| &rest[..end]).to_string();
    }
    if host.matches(':').count() == 1 {
        host = host.split(':').next().unwrap_or("").to_string();
    }
    host.strip_suffix('.').unwrap_or(&host).to_string()
}

fn is_ip_like(host: &str) -> bool {
    static NUMERIC: OnceLock<Regex> = OnceLock::new();
    let numeric = NUMERIC.get_or_init(|| {
        Regex::new(r"^(0x[0-9a-f]+|\d+)(\.(0x[0-9a-f]+|\d+))*$").expect("a valid pattern")
    });
    // `fe80::1%eth0` is an IP address with a zone, which `IpAddr` does not parse.
    let unzoned = host.split('%').next().unwrap_or(host);
    unzoned.parse::<IpAddr>().is_ok() || numeric.is_match(host)
}

/// Why an `allow_domain` entry can reach services on the host's loopback
/// interface through nono's proxy, or `None` when it cannot. nono's proxy
/// resolves an allowed name and connects to whatever address it gets, and does
/// not filter loopback or private addresses, so a name that resolves to the
/// host is as good as allowing 127.0.0.1. An entry is unsafe when it is a
/// catch-all wildcard, an IP address, a loopback or local-network name, a
/// single-label name (`/etc/hosts` often maps the host name to 127.0.1.1), or
/// a name under a wildcard DNS service.
pub fn loopback_reason(entry: &str) -> Option<&'static str> {
    let host = allow_domain_host(entry);
    if host.is_empty() || host == "*" {
        return Some("allows every host, including localhost");
    }
    // Shorthand forms such as 127.1 or 0x7f.1 are IP addresses to the resolver.
    if is_ip_like(&host) {
        return Some("is an IP address, which may be the host itself");
    }
    let simple_wildcard = host
        .strip_prefix("*.")
        .is_some_and(|rest| !rest.is_empty() && !rest.contains('*'));
    if host.contains('*') && !simple_wildcard {
        return Some("is a wildcard nono's proxy may match against localhost");
    }
    let name = host.strip_prefix("*.").unwrap_or(&host);
    if !name.contains('.') {
        return Some(if host.starts_with("*.") {
            "covers a whole top-level domain, including names that resolve to localhost"
        } else {
            "is a single-label name, which can resolve to the host itself"
        });
    }
    let under = |suffix: &&str| name == *suffix || name.ends_with(&format!(".{suffix}"));
    if LOCAL_SUFFIXES.iter().any(under) || name.starts_with("localhost.") {
        return Some("resolves on the host or the local network");
    }
    if WILDCARD_DNS_DOMAINS.iter().any(under) {
        return Some("is a wildcard DNS service whose names resolve to 127.0.0.1");
    }
    None
}

/// An `allow_domain` entry that lets a sandbox behind nono's proxy reach localhost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoopbackDomain {
    pub domain: String,
    pub reason: String,
}

/// The `allow_domain` entries that let a sandbox behind nono's proxy reach
/// localhost, each with the reason.
pub fn loopback_domains(allow_domains: &[String]) -> Vec<LoopbackDomain> {
    allow_domains
        .iter()
        .filter_map(|domain| {
            loopback_reason(domain).map(|reason| LoopbackDomain {
                domain: domain.clone(),
                reason: reason.to_string(),
            })
        })
        .collect()
}

/// A short description of a resolved nono profile (`nono profile show --json`).
/// `egress` is `open` when the sandbox may open TCP connections directly, which
/// includes connections to services on localhost; `allowlist` routes egress
/// through nono's proxy and denies direct connects; `blocked` allows none.
/// `loopback` says whether the sandbox can reach services on localhost: with
/// open egress directly, behind the proxy when an allowed domain can resolve to
/// the host (`loopback_domains` lists those entries). `read_write_paths` and
/// `read_only_paths` are the profile's directory grants, before the plugin adds
/// the workspace root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSummary {
    pub name: Option<String>,
    pub description: Option<String>,
    pub extends: Vec<String>,
    pub egress: String,
    pub allow_domains: Vec<String>,
    pub loopback: bool,
    pub loopback_domains: Vec<LoopbackDomain>,
    pub af_unix_mediation: String,
    pub workdir_access: Option<String>,
    pub read_write_paths: Vec<String>,
    pub read_only_paths: Vec<String>,
}

/// JavaScript truthiness of a JSON value.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|float| float != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |items| items.iter().map(js_string).collect())
}

pub fn summarize_profile(profile: &Value) -> ProfileSummary {
    let network = profile.get("network");
    let allow_domains: Vec<String> = network
        .and_then(|network| network.get("allow_domain"))
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |items| {
            items
                .iter()
                .map(|item| match item {
                    Value::String(text) => text.clone(),
                    other => other
                        .get("domain")
                        .filter(|domain| !domain.is_null())
                        .map_or_else(|| other.to_string(), js_string),
                })
                .collect()
        });
    let egress = if network.and_then(|network| network.get("block")) == Some(&Value::Bool(true)) {
        "blocked"
    } else if !allow_domains.is_empty()
        || truthy(network.and_then(|network| network.get("network_profile")))
    {
        "allowlist"
    } else {
        "open"
    };
    let unsafe_domains = if egress == "allowlist" {
        loopback_domains(&allow_domains)
    } else {
        Vec::new()
    };
    let description = profile
        .get("description")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let extends = match profile.get("extends") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(js_string).collect(),
        Some(single) => vec![js_string(single)],
    };
    let filesystem = profile.get("filesystem");
    ProfileSummary {
        name: profile
            .get("name")
            .filter(|name| !name.is_null())
            .map(js_string),
        description,
        extends,
        egress: egress.to_string(),
        allow_domains,
        loopback: egress == "open" || !unsafe_domains.is_empty(),
        loopback_domains: unsafe_domains,
        af_unix_mediation: profile
            .get("linux")
            .and_then(|linux| linux.get("af_unix_mediation"))
            .filter(|value| !value.is_null())
            .map_or_else(|| "off".to_string(), js_string),
        workdir_access: profile
            .get("workdir")
            .and_then(|workdir| workdir.get("access"))
            .filter(|value| !value.is_null())
            .map(js_string),
        read_write_paths: string_list(filesystem.and_then(|fs| fs.get("allow"))),
        read_only_paths: string_list(filesystem.and_then(|fs| fs.get("read"))),
    }
}

/// What a captured nono call printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonoOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl NonoOutput {
    pub fn output(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// `nono --version`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonoVersion {
    pub raw: String,
    pub version: Option<String>,
}

/// A client bound to one nono executable.
#[derive(Debug, Clone)]
pub struct NonoClient {
    pub bin: String,
    env: Env,
    timeout: Duration,
    cancel: Option<CancelToken>,
}

impl NonoClient {
    /// A client for `bin`; `HERDR_NONO_TIMEOUT_MS` in `env` overrides the call timeout.
    pub fn new(bin: impl Into<String>, env: Env) -> Self {
        let configured = env
            .get(NONO_CALL_TIMEOUT_ENV)
            .and_then(|raw| raw.trim().parse::<f64>().ok())
            .filter(|ms| ms.is_finite() && *ms > 0.0);
        let timeout =
            Duration::from_millis(configured.map_or(NONO_CALL_TIMEOUT_MS, |ms| ms as u64));
        NonoClient {
            bin: bin.into(),
            env,
            timeout,
            cancel: None,
        }
    }

    /// A client for `bin` with this process's environment.
    #[allow(dead_code)]
    pub fn from_process(bin: impl Into<String>) -> Self {
        Self::new(bin, process_env())
    }

    /// Calls made through this client end when the token is cancelled.
    #[allow(dead_code)]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Overrides the call timeout (the overlay uses a shorter one).
    #[allow(dead_code)]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Runs nono with captured output and returns status and output, failing
    /// only when nono cannot be started or times out.
    pub fn run(&self, args: &[&str]) -> Result<NonoOutput> {
        let options = RunOptions {
            env: Some(&self.env),
            cwd: None,
            timeout: self.timeout,
            cancel: self.cancel.as_ref(),
        };
        let words = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
        match run_cli(&self.bin, args, &options) {
            Ok(output) => Ok(NonoOutput { status: output.status, stdout: output.stdout, stderr: output.stderr }),
            Err(ExecError::TimedOut(partial)) => Err(PluginError::new(
                ErrorKind::Unknown,
                format!(
                    "nono {words} did not finish within {}s and was killed ({NONO_CALL_TIMEOUT_ENV} raises the limit).",
                    self.timeout.as_secs_f64().round()
                ),
            )
            .with_output(partial.output())),
            Err(ExecError::Cancelled(partial)) => Err(PluginError::new(ErrorKind::Unknown, format!("nono {words} was cancelled.")).with_output(partial.output())),
            Err(ExecError::Spawn(error)) => Err(PluginError::new(
                ErrorKind::Startup,
                format!("Could not run nono (\"{}\"): {error}. Install nono or set nonoBin in config.json.", self.bin),
            )
            .with_cause(error)),
        }
    }

    /// Runs nono and fails with a classified error on a non-zero exit.
    pub fn run_checked(&self, args: &[&str], step: &str) -> Result<NonoOutput> {
        let result = self.run(args)?;
        if result.status == Some(0) {
            return Ok(result);
        }
        let exit = result
            .status
            .map_or_else(|| "a signal".to_string(), |code| format!("exit {code}"));
        let output = result.output();
        Err(PluginError::new(
            classify_failure(&output),
            format!("nono failed while {step} ({exit})."),
        )
        .with_output(output))
    }

    /// `nono --version`.
    pub fn version(&self) -> Result<NonoVersion> {
        static VERSION: OnceLock<Regex> = OnceLock::new();
        let pattern =
            VERSION.get_or_init(|| Regex::new(r"(\d+\.\d+\.\d+)").expect("a valid pattern"));
        let output = self.run_checked(&["--version"], "reading its version")?;
        Ok(NonoVersion {
            raw: output.stdout.trim().to_string(),
            version: pattern
                .captures(&output.stdout)
                .map(|found| found[1].to_string()),
        })
    }

    /// Lists sessions (`nono ps --json`), including exited ones with `all`.
    pub fn list_sessions(&self, all: bool) -> Result<Vec<Session>> {
        let args: &[&str] = if all {
            &["ps", "--json", "--all"]
        } else {
            &["ps", "--json"]
        };
        let output = self.run_checked(args, "listing sessions")?;
        let parsed: Value = if output.stdout.trim().is_empty() {
            Value::Array(Vec::new())
        } else {
            serde_json::from_str(&output.stdout).map_err(|error| {
                let shown: String = output.stdout.chars().take(2000).collect();
                PluginError::new(
                    ErrorKind::Unknown,
                    format!("nono ps --json printed invalid JSON: {error}"),
                )
                .with_output(shown)
            })?
        };
        normalize_session_list(&parsed)
    }

    /// Stops a session by id (`nono stop`), SIGTERM first unless `force`.
    pub fn stop(&self, session_id: &str, force: bool) -> Result<NonoOutput> {
        let step = format!("stopping session {session_id}");
        if force {
            self.run_checked(&["stop", "--force", session_id], &step)
        } else {
            self.run_checked(&["stop", session_id], &step)
        }
    }

    /// Resolves a profile (`nono profile show --json`). Fails with `not-found`
    /// when nono does not know it, which is how doctor validates the configuration.
    pub fn show_profile(&self, profile: &str) -> Result<Value> {
        let output = self.run_checked(
            &["profile", "show", "--json", profile],
            &format!("resolving profile {profile}"),
        )?;
        serde_json::from_str(&output.stdout).map_err(|error| {
            let shown: String = output.stdout.chars().take(2000).collect();
            PluginError::new(
                ErrorKind::Unknown,
                format!("nono profile show --json printed invalid JSON: {error}"),
            )
            .with_output(shown)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    fn owned(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn build_run_args_grants_the_workspace_root_explicitly_and_puts_the_agent_after_the_separator()
    {
        let args = build_run_args(&RunArgs {
            profile: "/p.json",
            session_name: "herdr-opencode-1",
            workspace_root: "/repo",
            allow_paths: &owned(&["/data"]),
            read_paths: &owned(&["/ref"]),
            silent: true,
            extra_args: &owned(&["--memory", "2G"]),
            argv: &owned(&["opencode", "--standalone"]),
        })
        .unwrap();
        assert_eq!(
            args,
            [
                "run",
                "--silent",
                "--profile",
                "/p.json",
                "--name",
                "herdr-opencode-1",
                "--allow",
                "/repo",
                "--allow",
                "/data",
                "--read",
                "/ref",
                "--memory",
                "2G",
                "--",
                "opencode",
                "--standalone"
            ]
        );
        let plain = build_run_args(&RunArgs {
            profile: "opencode",
            session_name: "s1",
            workspace_root: "/repo",
            argv: &owned(&["bash"]),
            ..Default::default()
        })
        .unwrap();
        assert!(
            !plain.contains(&"--allow-cwd".to_string()),
            "the pane's directory is never granted implicitly"
        );
        let error = build_run_args(&RunArgs {
            profile: "p",
            session_name: "s",
            workspace_root: "/r",
            ..Default::default()
        })
        .unwrap_err();
        assert!(error.message.contains("needs the command"));
    }

    #[test]
    fn classify_failure_maps_nonos_wording() {
        assert_eq!(
            classify_failure("nono: Session not found: abc"),
            ErrorKind::NotFound
        );
        assert_eq!(
            classify_failure("nono: Profile not found: x"),
            ErrorKind::NotFound
        );
        assert_eq!(classify_failure("Permission denied"), ErrorKind::Permission);
        assert_eq!(classify_failure("open: EACCES"), ErrorKind::Permission);
        assert_eq!(
            classify_failure("error: unexpected argument '--x' found"),
            ErrorKind::Config
        );
        assert_eq!(classify_failure("something else"), ErrorKind::Unknown);
        assert_eq!(classify_failure(""), ErrorKind::Unknown);
    }

    #[test]
    fn normalize_session_list_accepts_nonos_array_and_refuses_other_shapes() {
        let sessions = normalize_session_list(&json!([{"session_id": "ab12", "name": "herdr-opencode-1", "supervisor_pid": 10, "child_pid": 11, "status": "running", "command": ["opencode", "--standalone"], "profile": "/p.json", "workdir": "/repo", "exit_code": -1}])).unwrap();
        let session = &sessions[0];
        assert_eq!(
            (
                session.session_id.as_str(),
                session.supervisor_pid,
                session.child_pid,
                session.exit_code
            ),
            ("ab12", Some(10), Some(11), Some(-1))
        );
        assert_eq!(session.command, ["opencode", "--standalone"]);
        assert_eq!(normalize_session_list(&Value::Null).unwrap(), []);
        assert_eq!(
            normalize_session_list(&json!({"sessions": []})).unwrap(),
            []
        );
        for bad in [json!({"weird": true}), json!([1, 2]), json!("text")] {
            assert!(
                normalize_session_list(&bad)
                    .unwrap_err()
                    .message
                    .contains("unexpected shape"),
                "{bad}"
            );
        }
        let by_id = normalize_session_list(&json!([{"id": 7}, {"name": "no id"}])).unwrap();
        assert_eq!(by_id.len(), 1, "a record without an id is dropped");
        assert_eq!(by_id[0].session_id, "7");
        assert_eq!(by_id[0].supervisor_pid, None);
    }

    #[test]
    fn allow_domain_host_reads_the_host_of_a_name_a_wildcard_a_url_glob_a_port_and_an_ipv6_literal()
    {
        for (input, expected) in [
            ("API.GitHubCopilot.com.", "api.githubcopilot.com"),
            ("*.githubcopilot.com", "*.githubcopilot.com"),
            ("https://github.com/org/**", "github.com"),
            ("https://*.wikipedia.org/wiki/**", "*.wikipedia.org"),
            ("localhost:4096", "localhost"),
            ("[::1]:4096", "::1"),
            ("  spaced.example.com  ", "spaced.example.com"),
        ] {
            assert_eq!(allow_domain_host(input), expected, "{input}");
        }
    }

    const LOOPBACK_ENTRIES: [&str; 27] = [
        "*",
        "*.*",
        "localhost",
        "LOCALHOST",
        "foo.localhost",
        "localhost.localdomain",
        "127.0.0.1",
        "127.1",
        "0x7f.1",
        "2130706433",
        "0.0.0.0",
        "::1",
        "[::1]",
        "10.0.0.5",
        "http://127.0.0.1:4096/**",
        "myhost",
        "printer.local",
        "db.internal",
        "127.0.0.1.nip.io",
        "*.nip.io",
        "a.localtest.me",
        "*.localtest.me",
        "app.lvh.me",
        "127-0-0-1.sslip.io",
        "*.com",
        "*.io",
        "api.*.com",
    ];
    const PROVIDER_ENTRIES: [&str; 7] = [
        "api.githubcopilot.com",
        "*.githubcopilot.com",
        "github.com",
        "api.github.com",
        "models.opencode.ai",
        "https://github.com/org/**",
        "api.anthropic.com",
    ];

    #[test]
    fn loopback_reason_flags_every_way_to_localhost_and_passes_the_provider_hosts() {
        for entry in LOOPBACK_ENTRIES {
            assert!(
                loopback_reason(entry).is_some(),
                "{entry} should be flagged"
            );
        }
        for entry in PROVIDER_ENTRIES {
            assert_eq!(loopback_reason(entry), None, "{entry}");
        }
        let flagged: Vec<String> = loopback_domains(&owned(&["github.com", "*", "localhost"]))
            .into_iter()
            .map(|item| item.domain)
            .collect();
        assert_eq!(flagged, ["*", "localhost"]);
        assert_eq!(
            loopback_reason("*.com"),
            Some("covers a whole top-level domain, including names that resolve to localhost")
        );
        assert_eq!(
            loopback_reason("myhost"),
            Some("is a single-label name, which can resolve to the host itself")
        );
    }

    #[test]
    fn summarize_profile_says_whether_a_sandbox_can_reach_localhost() {
        assert!(!summarize_profile(&json!({"network": {"block": true}})).loopback);
        assert!(
            summarize_profile(&json!({"network": {}})).loopback,
            "open egress connects directly"
        );
        assert!(summarize_profile(&json!({})).loopback);
        let wildcard = summarize_profile(&json!({"network": {"allow_domain": ["*"]}}));
        assert_eq!(wildcard.egress, "allowlist");
        assert!(
            wildcard.loopback,
            "proxy mode with every domain allowed forwards to localhost"
        );
        assert_eq!(wildcard.loopback_domains[0].domain, "*");
        let copilot = summarize_profile(
            &json!({"network": {"allow_domain": ["models.opencode.ai", {"domain": "*.githubcopilot.com"}]}}),
        );
        assert!(!copilot.loopback);
        assert_eq!(
            copilot.allow_domains,
            ["models.opencode.ai", "*.githubcopilot.com"]
        );
        assert!(copilot.loopback_domains.is_empty());
        assert_eq!(
            summarize_profile(&json!({"network": {"network_profile": "developer"}})).egress,
            "allowlist"
        );
        assert_eq!(
            summarize_profile(&json!({"network": {"network_profile": ""}})).egress,
            "open"
        );
    }

    #[test]
    fn summarize_profile_reads_the_rest_of_the_profile() {
        let summary = summarize_profile(&json!({
            "name": "p", "description": "d", "extends": "nolabs-ai/opencode",
            "linux": {"af_unix_mediation": "pathname"}, "workdir": {"access": "readwrite"},
            "filesystem": {"allow": ["/a"], "read": ["/b", "/c"]},
        }));
        assert_eq!(summary.name.as_deref(), Some("p"));
        assert_eq!(summary.description.as_deref(), Some("d"));
        assert_eq!(summary.extends, ["nolabs-ai/opencode"]);
        assert_eq!(summary.af_unix_mediation, "pathname");
        assert_eq!(summary.workdir_access.as_deref(), Some("readwrite"));
        assert_eq!(
            (summary.read_write_paths, summary.read_only_paths),
            (owned(&["/a"]), owned(&["/b", "/c"]))
        );
        let bare = summarize_profile(&json!({}));
        assert_eq!(
            (
                &bare.name,
                &bare.description,
                bare.af_unix_mediation.as_str(),
                &bare.workdir_access
            ),
            (&None, &None, "off", &None)
        );
        assert!(bare.extends.is_empty());
        assert_eq!(
            serde_json::to_value(&bare).unwrap()["afUnixMediation"],
            "off"
        );
    }

    /// A fake `nono` shell script: answers `--version`, `ps --json`, `stop` and
    /// `profile show`, and fails by mode from the FAKE_FAIL file's content.
    fn fake_nono(dir: &Path) -> PathBuf {
        let script = dir.join("nono");
        std::fs::write(
            &script,
            r#"#!/bin/sh
case "$1" in
  --version) echo "nono ${FAKE_NONO_VERSION:-0.78.0}" ;;
  ps) echo '[]' ;;
  stop) echo "nono: Session not found: $2" >&2; exit 1 ;;
  profile) if [ -n "$FAKE_PROFILE_MISSING" ]; then echo "nono: Profile not found: $4" >&2; exit 1; fi
           echo '{"name":"'"$4"'","network":{"block":true}}' ;;
  slow) sleep 30 ;;
  *) echo "unsupported" >&2; exit 2 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn env_with(extra: &[(&str, &str)]) -> Env {
        let mut env = Env::from([("PATH".to_string(), std::env::var("PATH").unwrap())]);
        env.extend(
            extra
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string())),
        );
        env
    }

    #[test]
    fn the_client_reads_the_version_lists_sessions_and_classifies_failures() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_nono(dir.path());
        let nono = NonoClient::new(
            bin.to_str().unwrap(),
            env_with(&[("FAKE_NONO_VERSION", "0.79.1")]),
        );
        assert_eq!(nono.version().unwrap().version.as_deref(), Some("0.79.1"));
        assert_eq!(nono.version().unwrap().raw, "nono 0.79.1");
        assert_eq!(nono.list_sessions(false).unwrap(), []);
        assert_eq!(nono.list_sessions(true).unwrap(), []);
        assert_eq!(
            nono.stop("nothing", false).unwrap_err().kind,
            ErrorKind::NotFound
        );
        assert_eq!(
            nono.stop("nothing", true).unwrap_err().kind,
            ErrorKind::NotFound
        );
        assert_eq!(nono.show_profile("client").unwrap()["name"], "client");
        let missing_profile = NonoClient::new(
            bin.to_str().unwrap(),
            env_with(&[("FAKE_PROFILE_MISSING", "1")]),
        );
        let error = missing_profile.show_profile("missing").unwrap_err();
        assert_eq!(error.kind, ErrorKind::NotFound);
        assert!(
            error.output.contains("Profile not found"),
            "{}",
            error.output
        );
        let missing = NonoClient::new("/definitely/not/nono", Env::new());
        let error = missing.version().unwrap_err();
        assert_eq!(error.kind, ErrorKind::Startup);
        assert!(error.message.contains("Install nono"), "{}", error.message);
    }

    #[test]
    fn a_wedged_nono_is_killed_after_the_timeout_and_the_variable_raises_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_nono(dir.path());
        let nono = NonoClient::new(
            bin.to_str().unwrap(),
            env_with(&[(NONO_CALL_TIMEOUT_ENV, "200")]),
        );
        let started = std::time::Instant::now();
        let error = nono.run(&["slow"]).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(error.kind, ErrorKind::Unknown);
        assert!(
            error.message.contains("did not finish")
                && error.message.contains(NONO_CALL_TIMEOUT_ENV),
            "{}",
            error.message
        );
        assert_eq!(
            NonoClient::new("nono", Env::new()).timeout,
            Duration::from_millis(NONO_CALL_TIMEOUT_MS)
        );
        assert_eq!(
            NonoClient::new("nono", env_with(&[(NONO_CALL_TIMEOUT_ENV, "0")])).timeout,
            Duration::from_millis(NONO_CALL_TIMEOUT_MS)
        );
        assert_eq!(
            NonoClient::new("nono", env_with(&[(NONO_CALL_TIMEOUT_ENV, "junk")])).timeout,
            Duration::from_millis(NONO_CALL_TIMEOUT_MS)
        );
    }
}

#[cfg(test)]
mod shipped_profiles {
    use super::*;
    use serde_json::json;

    fn shipped(name: &str) -> Value {
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("profiles")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap()
    }

    #[test]
    fn the_shipped_profiles_extend_the_opencode_pack_close_the_host_sockets_and_lock_down_localhost(
    ) {
        let client = shipped("herdr-opencode-client.json");
        let server = shipped("herdr-opencode-server.json");
        for profile in [&client, &server] {
            assert_eq!(profile["extends"], "nolabs-ai/opencode");
            assert_eq!(profile["linux"]["af_unix_mediation"], "pathname");
            let denied = profile["environment"]["deny_vars"].as_array().unwrap();
            assert!(denied.contains(&json!("HERDR_*")) && denied.contains(&json!("SSH_AUTH_SOCK")));
            assert_eq!(profile["filesystem"]["suppress_save_prompt"], json!(["/"]));
        }
        assert_eq!(
            client["network"],
            json!({"block": true}),
            "the client reaches nothing but its server's port"
        );
        assert_eq!(
            server["network"],
            json!({"allow_domain": ["models.opencode.ai", "github.com", "api.github.com", "api.githubcopilot.com", "*.githubcopilot.com"]}),
            "the server reaches GitHub Copilot and OpenCode's model catalog through nono's proxy, nothing else"
        );
        let domains: Vec<String> = server["network"]["allow_domain"]
            .as_array()
            .unwrap()
            .iter()
            .map(|domain| domain.as_str().unwrap().to_string())
            .collect();
        assert!(
            loopback_domains(&domains).is_empty(),
            "no allowed domain can resolve to localhost"
        );
        let summary = summarize_profile(&server);
        assert_eq!(summary.egress, "allowlist");
        assert!(!summary.loopback);
        assert_eq!(summarize_profile(&client).egress, "blocked");
    }
}

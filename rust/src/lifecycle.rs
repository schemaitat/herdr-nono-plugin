//! Session lifecycle shared by the action dispatcher, the pane bridge and the
//! event hook. A nono sandbox lives exactly as long as the process it runs, so
//! the lifecycle is launch, run, exit; the mapping remembers the pane, the
//! workspace root, the agent and the last launch so `reconnect` can run the
//! agent again under the same name and policy. Progress messages go through
//! the injected `log` so pane modes print to the terminal while captured modes
//! keep stdout clean for the result marker.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::agents::{resolve_agent, with_port_all, ResolvedAgent};
use crate::config::PluginConfig;
use crate::constants::{
    is_stripped_env, AGENT_REPORT_SOURCE, LOGS_DIR, SERVER_READY_TIMEOUT_ENV,
    SERVER_READY_TIMEOUT_MS, SERVER_STOP_GRACE_MS, STOP_WAIT_MS, TERMINAL_RESTORE_SEQUENCE,
    VERIFY_INTERVAL_MS, VERIFY_WINDOW_ENV, VERIFY_WINDOW_MS,
};
use crate::context::{canonical_path, Env};
use crate::errors::{ErrorKind, PluginError, Result};
use crate::herdr::HerdrClient;
use crate::hostservice::{
    detect_opencode_service, home_dir, host_service_refusal, host_service_warning, HostService,
};
use crate::naming::{server_session_name, shell_session_name};
use crate::nono::{build_run_args, summarize_profile, NonoClient, RunArgs, Session};
use crate::procfs::{process_alive, procfs_available};
use crate::signals::{send_signal, take_pending, SignalGuard};
use crate::state::{
    delete_pane_entry_if_unchanged, get_pane_entry, load_state, own_start_token,
    process_start_token, require_pane_entry, update_pane_entry, with_pane_lock, Entry,
};
use crate::util::{base64, iso_now, positive_int, random_hex};
use crate::verify::{summarize_verification, verify_session, VerificationReport, VerifyInput};

// ---- identity of recorded processes ---------------------------------------------

/// The command line of a process as one space-separated string, or `None` when
/// it cannot be read (the process is gone or this is not Linux).
pub fn process_command_line(pid: u32) -> Option<String> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(
        String::from_utf8_lossy(&bytes)
            .replace('\0', " ")
            .trim()
            .to_string(),
    )
}

/// Whether a command line is a bridge of this plugin: the Rust binary's
/// `bridge` subcommand, or the `bridge.mjs` script of the Node version, so
/// bridges started before an upgrade are still recognised.
fn is_bridge_command(words: &[&str]) -> bool {
    words.iter().any(|word| word.ends_with("bridge.mjs"))
        || words
            .windows(2)
            .any(|pair| pair[0].ends_with("herdr-nono") && pair[1] == "bridge")
}

/// Whether a recorded process is alive, is the same incarnation that was
/// recorded (start token), and runs a bridge for the pane. A live process whose
/// command line cannot be read counts as the owner, so the checks fail closed.
/// `mode`, when given, must be one of the command line's words (the bridge mode).
pub fn process_owns(
    pid: Option<u32>,
    token: Option<&str>,
    pane_id: Option<&str>,
    mode: Option<&str>,
) -> bool {
    let Some(pid) = pid.filter(|pid| *pid > 0) else {
        return false;
    };
    if !process_alive(pid) {
        return false;
    }
    if let Some(token) = token.filter(|token| !token.is_empty() && *token != "-") {
        if let Some(current) = process_start_token(pid) {
            if current != token {
                // The pid was recycled since the record was written.
                return false;
            }
        }
    }
    let Some(command_line) = process_command_line(pid) else {
        return true;
    };
    let words: Vec<&str> = command_line.split_whitespace().collect();
    // `"".split(/\s+/)` has one empty word; an empty command line matches nothing either way.
    if !is_bridge_command(&words) {
        return false;
    }
    if mode.is_some_and(|mode| !words.contains(&mode)) {
        return false;
    }
    let Some(pane_id) = pane_id else {
        return true;
    };
    words
        .iter()
        .position(|word| *word == "--pane-id")
        .is_some_and(|index| words.get(index + 1) == Some(&pane_id))
}

fn entry_str<'a>(entry: &'a Entry, key: &str) -> Option<&'a str> {
    entry.get(key).and_then(Value::as_str)
}

fn entry_pid(entry: &Entry, key: &str) -> Option<u32> {
    positive_int(entry.get(key))
}

/// Whether the bridge process that last acknowledged a mapping is still alive
/// and really is that bridge. The bridge lives exactly as long as the agent's
/// nono session, so a live one means the agent is running in the pane.
pub fn bridge_is_running(entry: &Entry) -> bool {
    process_owns(
        entry_pid(entry, "bridgePid"),
        entry_str(entry, "bridgeToken"),
        entry_str(entry, "paneId"),
        None,
    )
}

/// The open-shell sessions of a mapping that are still alive.
pub fn live_shells(entry: &Entry) -> Vec<Value> {
    let Some(shells) = entry.get("shellPids").and_then(Value::as_array) else {
        return Vec::new();
    };
    shells
        .iter()
        .filter(|shell| {
            let pane = shell
                .get("paneId")
                .and_then(Value::as_str)
                .or_else(|| entry_str(entry, "paneId"));
            process_owns(
                positive_int(shell.get("pid")),
                shell.get("token").and_then(Value::as_str),
                pane,
                Some("shell"),
            )
        })
        .cloned()
        .collect()
}

/// Whether an open-shell session is running for the mapping.
pub fn shell_is_running(entry: &Entry) -> bool {
    !live_shells(entry).is_empty()
}

// ---- paths, environment and launch helpers --------------------------------------

/// Fails unless the path is an existing absolute directory.
pub fn assert_local_path(local_path: &str) -> Result<()> {
    if !Path::new(local_path).is_absolute() {
        return Err(PluginError::new(
            ErrorKind::Target,
            format!(
                "The workspace path must be absolute (got {}).",
                Value::String(local_path.to_string())
            ),
        ));
    }
    if !Path::new(local_path).is_dir() {
        return Err(PluginError::new(
            ErrorKind::Target,
            format!("The workspace path {local_path} is not an existing directory."),
        ));
    }
    Ok(())
}

/// Like [`assert_local_path`], but also refuses the filesystem root and the
/// home directory: granting either read-write to an agent is never intended.
pub fn assert_workspace_root(local_path: &str) -> Result<PathBuf> {
    assert_local_path(local_path)?;
    let resolved = canonical_path(local_path);
    let home = canonical_path(home_dir(&crate::context::process_env()));
    if resolved == Path::new("/") || resolved == home {
        return Err(PluginError::new(
            ErrorKind::Target,
            format!("Refusing to grant {} to a sandboxed agent. Invoke start-agent from a project directory.", resolved.display()),
        ));
    }
    Ok(resolved)
}

/// Resolves the adapter recorded in a mapping, honoring current config overrides.
pub fn agent_for_entry(
    config: &PluginConfig,
    entry: &Entry,
    plugin_root: &str,
) -> Result<ResolvedAgent> {
    let mut config = config.clone();
    config.agent_kind = entry_str(entry, "agentKind")
        .unwrap_or("undefined")
        .to_string();
    resolve_agent(&config, plugin_root)
}

/// The directory a launch starts in: the recorded working directory when it
/// still exists, else the workspace root.
pub fn launch_cwd(entry: &Entry) -> PathBuf {
    match entry_str(entry, "workdir") {
        Some(workdir) if Path::new(workdir).exists() => PathBuf::from(workdir),
        _ => PathBuf::from(entry_str(entry, "localPath").unwrap_or("/")),
    }
}

/// The environment handed to `nono run`: the bridge's own environment minus the
/// variables [`is_stripped_env`] names, plus `agentEnv`.
pub fn sandbox_env(env: &Env, agent_env: &[String]) -> Env {
    let mut result: Env = env
        .iter()
        .filter(|(key, _)| !is_stripped_env(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    for item in agent_env {
        if let Some((key, value)) = item.split_once('=') {
            result.insert(key.to_string(), value.to_string());
        }
    }
    result
}

/// Finds an executable on PATH the way a shell would, or `None`.
pub fn find_executable(command: &str, env: &Env) -> Option<PathBuf> {
    if command.contains('/') {
        return Path::new(command).exists().then(|| PathBuf::from(command));
    }
    env.get("PATH")?
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(command))
        .find(|candidate| std::fs::metadata(candidate).is_ok_and(|meta| meta.is_file()))
}

/// The argv and extra environment of an `open-shell` sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellLaunch {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// nono's profiles deny shell start-up files (they are a classic persistence
/// target), so the shells this plugin knows are started without them instead of
/// printing permission errors, with a prompt that says where the shell runs.
pub fn shell_launch(shell: &str, session_name: &str) -> ShellLaunch {
    let suffix: String = session_name
        .rsplit('-')
        .next()
        .unwrap_or("")
        .chars()
        .take(6)
        .collect();
    let label = format!("nono:{suffix}");
    let name = shell.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let ps1 = |text: String| vec![("PS1".to_string(), text)];
    match name {
        "bash" => ShellLaunch {
            argv: vec![shell.into(), "--noprofile".into(), "--norc".into()],
            env: ps1(format!("[{label}] \\w \\$ ")),
        },
        "zsh" => ShellLaunch {
            argv: vec![shell.into(), "--no-rcs".into()],
            env: ps1(format!("[{label}] %~ %# ")),
        },
        "fish" => ShellLaunch {
            argv: vec![shell.into(), "--no-config".into()],
            env: Vec::new(),
        },
        _ => ShellLaunch {
            argv: vec![shell.into()],
            env: ps1(format!("[{label}] $ ")),
        },
    }
}

/// Picks a free TCP port on 127.0.0.1 by binding port 0 on the host, where that
/// is allowed, and releasing it again for the sandboxed server.
pub fn pick_free_port() -> std::io::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// One authenticated GET against the private server: the HTTP status, or
/// `None` when nothing answered.
fn probe_server(port: u16, url_path: &str, user: &str, password: &str) -> Option<u16> {
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let timeout = Duration::from_millis(1000);
    let mut stream = TcpStream::connect_timeout(&address, timeout).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    let credentials = base64(format!("{user}:{password}").as_bytes());
    let request = format!("GET {url_path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Basic {credentials}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut buffer = [0u8; 64];
    let read = stream.read(&mut buffer).ok()?;
    let text = String::from_utf8_lossy(&buffer[..read]);
    text.strip_prefix("HTTP/1.1 ")
        .or_else(|| text.strip_prefix("HTTP/1.0 "))?
        .get(..3)?
        .parse()
        .ok()
}

/// The last lines of a log file, for error messages.
fn tail(file: &Path, count: usize) -> String {
    let Ok(text) = std::fs::read_to_string(file) else {
        return String::new();
    };
    let lines: Vec<&str> = text.trim_end().split('\n').collect();
    lines[lines.len().saturating_sub(count)..].join("\n")
}

/// `{kind, message, at}`: how a failure is recorded on a mapping.
pub fn describe_error(error: &PluginError) -> Value {
    json!({"kind": error.kind_str(), "message": error.message, "at": iso_now()})
}

fn obj(value: Value) -> Entry {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// `Number(env[name])`: a missing value is NaN, an empty one is 0.
fn env_number(env: &Env, name: &str) -> Option<f64> {
    let raw = env.get(name)?;
    if raw.trim().is_empty() {
        return Some(0.0);
    }
    raw.trim()
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
}

fn verify_window_ms(env: &Env) -> u64 {
    env_number(env, VERIFY_WINDOW_ENV)
        .filter(|ms| *ms >= 0.0)
        .map_or(VERIFY_WINDOW_MS, |ms| ms as u64)
}

fn server_ready_timeout_ms(env: &Env) -> u64 {
    env_number(env, SERVER_READY_TIMEOUT_ENV)
        .filter(|ms| *ms > 0.0)
        .map_or(SERVER_READY_TIMEOUT_MS, |ms| ms as u64)
}

/// Sleeps up to `total`, waking early once `keep_going` turns false.
fn sleep_while(total: Duration, keep_going: impl Fn() -> bool) {
    let end = Instant::now() + total;
    while keep_going() && Instant::now() < end {
        std::thread::sleep(
            Duration::from_millis(20).min(end.saturating_duration_since(Instant::now())),
        );
    }
}

// ---- results ---------------------------------------------------------------------

/// The live nono sessions of a mapping.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MappingSessions {
    /// The agent's session, matched by supervisor pid, else by name.
    pub agent: Option<Session>,
    /// Its private server sessions.
    pub servers: Vec<Session>,
    /// Its open-shell sessions.
    pub shells: Vec<Session>,
}

#[derive(Debug)]
pub struct LaunchOutcome {
    pub exit_code: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StopOutcome {
    pub session_name: String,
    pub session_id: Option<String>,
    pub server_session_ids: Vec<String>,
}

#[derive(Debug)]
pub struct VerifyOutcome {
    pub entry: Option<Entry>,
    pub report: VerificationReport,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSummary {
    pub kind: String,
    pub title: String,
    pub profile: String,
    pub server_profile: Option<String>,
    pub launch_argv: Vec<String>,
    pub resume_argv: Vec<String>,
    pub server_argv: Option<Vec<String>>,
    pub herdr_detection_kind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LiveSessions {
    pub agent: Option<Session>,
    pub server: Option<Session>,
    pub shells: Vec<Session>,
}

/// A mapping together with its live sessions when nono answers.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Description {
    pub mapping: Entry,
    pub agent: AgentSummary,
    pub sessions: Option<LiveSessions>,
    pub session_error: Option<Value>,
    pub verification: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct ListAll {
    pub mappings: Vec<Value>,
    #[serde(rename = "sessionError")]
    pub session_error: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrunedMapping {
    pub pane_id: String,
    pub session_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeptMapping {
    pub pane_id: String,
    pub session_name: String,
    pub reason: String,
}

#[derive(Debug, Default, Serialize)]
pub struct PruneOutcome {
    pub pruned: Vec<PrunedMapping>,
    pub kept: Vec<KeptMapping>,
}

/// What the host check found before a launch.
struct Preflight {
    host_service: Option<HostService>,
    warning: Option<String>,
    loopback_open: bool,
}

struct RunningServer {
    child: Child,
    log_file: PathBuf,
}

// ---- the lifecycle ---------------------------------------------------------------

/// Everything a [`Lifecycle`] is bound to.
pub struct LifecycleOptions {
    pub state_dir: PathBuf,
    pub config: PluginConfig,
    pub plugin_root: String,
    pub nono: NonoClient,
    pub log: Box<dyn Fn(&str) + Send + Sync>,
    pub herdr: Option<HerdrClient>,
    pub env: Env,
    pub proc_root: PathBuf,
}

impl LifecycleOptions {
    /// Options with progress lines on stderr and the real /proc.
    pub fn new(
        state_dir: impl Into<PathBuf>,
        config: PluginConfig,
        plugin_root: impl Into<String>,
        nono: NonoClient,
        env: Env,
    ) -> Self {
        LifecycleOptions {
            state_dir: state_dir.into(),
            config,
            plugin_root: plugin_root.into(),
            nono,
            log: Box::new(|line| eprintln!("{line}")),
            herdr: None,
            env,
            proc_root: PathBuf::from(crate::procfs::PROC_ROOT),
        }
    }
}

pub struct Lifecycle {
    opts: LifecycleOptions,
    /// Set when the verification of the running launch failed and ended it.
    stopped_for_verification: AtomicBool,
}

impl Lifecycle {
    pub fn new(opts: LifecycleOptions) -> Self {
        Lifecycle {
            opts,
            stopped_for_verification: AtomicBool::new(false),
        }
    }

    fn log(&self, line: &str) {
        (self.opts.log)(line);
    }

    fn notify(&self, title: &str, body: &str) {
        if let Some(herdr) = &self.opts.herdr {
            herdr.notify(title, body);
        }
    }

    fn state_dir(&self) -> &Path {
        &self.opts.state_dir
    }

    fn update(&self, pane_id: &str, patch: Value) -> Result<Entry> {
        update_pane_entry(self.state_dir(), pane_id, &obj(patch))
    }

    /// The host service an agent could use to leave the sandbox, when the
    /// adapter knows of one and the check is on.
    pub fn host_service_for(&self, agent: &ResolvedAgent) -> Option<HostService> {
        if agent.host_service.as_deref() != Some("opencode")
            || self.opts.config.host_service_check == "off"
        {
            return None;
        }
        Some(detect_opencode_service(
            &self.opts.env,
            &self.opts.proc_root,
        ))
    }

    /// Whether the sandbox that runs the agent's tools (the server sandbox when
    /// there is one) can reach localhost ports: directly with open egress, or
    /// through nono's proxy when an allowed domain can resolve to the host. A
    /// profile nono cannot resolve counts as open, so the check fails safe.
    pub fn tools_loopback(&self, agent: &ResolvedAgent) -> ToolsLoopbackReport {
        let reference = agent
            .server_profile_ref
            .as_deref()
            .unwrap_or(&agent.profile_ref);
        match self.opts.nono.show_profile(reference) {
            Ok(profile) => {
                let summary = summarize_profile(&profile);
                if !summary.loopback {
                    return ToolsLoopbackReport {
                        open: false,
                        reason: None,
                    };
                }
                let reason = if summary.egress == "open" {
                    format!("profile {reference} lets them connect directly")
                } else {
                    let domains: Vec<String> = summary
                        .loopback_domains
                        .iter()
                        .map(|item| format!("\"{}\", which {}", item.domain, item.reason))
                        .collect();
                    format!("profile {reference} allows {}", domains.join("; "))
                };
                ToolsLoopbackReport {
                    open: true,
                    reason: Some(reason),
                }
            }
            Err(error) => {
                self.log(&format!(
                    "could not resolve the profile to check localhost access: {}",
                    error.message
                ));
                ToolsLoopbackReport {
                    open: true,
                    reason: Some(format!("nono could not resolve profile {reference}")),
                }
            }
        }
    }

    /// Checks the host before a launch: the workspace root exists, the agent
    /// binary is on PATH, and the sandbox that runs the agent's tools cannot
    /// reach localhost (refuse, or warn). Localhost counts whether or not the
    /// OpenCode host service runs right now: plain `opencode` on the host
    /// restarts it at any time, on a port the sandbox can then reach.
    fn preflight(&self, entry: &Entry, agent: &ResolvedAgent) -> Result<Preflight> {
        assert_local_path(entry_str(entry, "localPath").unwrap_or(""))?;
        let command = agent.command.first().map_or("", String::as_str);
        if find_executable(command, &self.opts.env).is_none() {
            return Err(PluginError::new(
                ErrorKind::Config,
                format!(
                    "The agent command \"{command}\" was not found on PATH ({}). Install it or set command in a custom agent.",
                    self.opts.env.get("PATH").map_or("unset", String::as_str)
                ),
            ));
        }
        let Some(host_service) = self.host_service_for(agent) else {
            return Ok(Preflight {
                host_service: None,
                warning: None,
                loopback_open: false,
            });
        };
        let loopback = self.tools_loopback(agent);
        if !loopback.open {
            return Ok(Preflight {
                host_service: Some(host_service),
                warning: None,
                loopback_open: false,
            });
        }
        let reason = loopback.reason.unwrap_or_default();
        let running = host_service_warning(&host_service);
        let warning = running.clone().unwrap_or_else(|| {
            format!(
                "The agent's tools can reach localhost: {reason}. An OpenCode host service, which plain \"opencode\" restarts on demand, would let them run commands outside the sandbox. Restrict network.allow_domain in the server profile to your provider's hosts."
            )
        });
        if self.opts.config.host_service_check == "refuse" {
            if running.is_some() {
                let home = home_dir(&self.opts.env);
                for line in host_service_refusal(&host_service, &home.to_string_lossy(), true) {
                    self.log(&line);
                }
                let pid = host_service
                    .pid
                    .map_or_else(|| "null".to_string(), |pid| pid.to_string());
                self.notify("nono: agent NOT started", &format!("An unsandboxed OpenCode service runs (pid {pid}). Run \"opencode service stop\", then reconnect."));
                return Err(PluginError::new(
                    ErrorKind::Unconfined,
                    format!("Not started: an unsandboxed OpenCode service runs (pid {pid}); hostServiceCheck is \"refuse\"."),
                ));
            }
            self.log(&format!("nono: the agent was NOT started. {warning}"));
            self.notify(
                "nono: agent NOT started",
                "The agent's tools could reach localhost; see the pane or run doctor.",
            );
            return Err(PluginError::new(
                ErrorKind::Unconfined,
                format!("Not started: the agent's tools can reach localhost ({reason}); hostServiceCheck is \"refuse\"."),
            ));
        }
        Ok(Preflight {
            host_service: Some(host_service),
            warning: Some(warning),
            loopback_open: true,
        })
    }

    /// Starts the agent's private server in its own nono sandbox, detached from
    /// the pane's terminal, and waits until it answers on its port.
    fn start_server(
        &self,
        entry: &Entry,
        agent: &ResolvedAgent,
        port: u16,
        password: &str,
    ) -> Result<RunningServer> {
        let session_name = entry_str(entry, "sessionName").unwrap_or_default();
        let server = agent
            .server
            .as_ref()
            .expect("an agent with a server sandbox");
        let log_file = self
            .state_dir()
            .join(LOGS_DIR)
            .join(format!("{session_name}-server.log"));
        let startup = |message: String| PluginError::new(ErrorKind::Startup, message);
        std::fs::create_dir_all(log_file.parent().expect("a log directory")).map_err(|error| {
            startup(format!(
                "Could not create the server log directory: {error}"
            ))
        })?;
        let log = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&log_file)
            .map_err(|error| startup(format!("Could not open {}: {error}", log_file.display())))?;
        let mut extra: Vec<String> = self.opts.config.nono_args.clone();
        extra.extend(["--listen-port".to_string(), port.to_string()]);
        let args = build_run_args(&RunArgs {
            profile: agent.server_profile_ref.as_deref().unwrap_or_default(),
            session_name: &server_session_name(session_name),
            workspace_root: entry_str(entry, "localPath").unwrap_or_default(),
            allow_paths: &self.opts.config.allow_paths,
            read_paths: &self.opts.config.read_paths,
            silent: true,
            extra_args: &extra,
            argv: &with_port_all(&server.command, Some(port)),
        })?;
        let mut env = sandbox_env(&self.opts.env, &self.opts.config.agent_env);
        env.insert(server.password_env.clone(), password.to_string());
        let mut command = Command::new(&self.opts.nono.bin);
        command
            .args(&args)
            .current_dir(launch_cwd(entry))
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log.try_clone()
                    .map_err(|error| startup(error.to_string()))?,
            ))
            .stderr(Stdio::from(log))
            .env_clear()
            .envs(&env);
        // Its own session: Ctrl-C in the pane must not reach the server.
        // SAFETY: setsid is async-signal-safe and runs between fork and exec.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(|error| {
            startup(format!(
                "Could not start nono (\"{}\") for {}'s server: {error}. Its log is {}.",
                self.opts.nono.bin,
                agent.title,
                log_file.display()
            ))
        })?;
        let timeout_ms = server_ready_timeout_ms(&self.opts.env);
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut exited = false;
        while Instant::now() < deadline && !exited {
            if probe_server(port, &server.ready_path, &server.user, password) == Some(200) {
                return Ok(RunningServer { child, log_file });
            }
            std::thread::sleep(Duration::from_millis(250));
            exited = child
                .try_wait()
                .map(|status| status.is_some())
                .unwrap_or(true);
        }
        stop_server(&mut child);
        let reason = if exited {
            "exited before it answered".to_string()
        } else {
            format!(
                "did not answer on port {port} within {}s",
                (timeout_ms as f64 / 1000.0).round()
            )
        };
        Err(startup(format!(
            "{}'s server {reason}. Its log is {}.",
            agent.title,
            log_file.display()
        ))
        .with_output(tail(&log_file, 15)))
    }

    /// Stops server sessions of this mapping that outlived their bridge (a
    /// bridge killed with SIGKILL cannot take its server down).
    fn stop_orphaned_servers(&self, entry: &Entry) {
        let Ok(sessions) = self.opts.nono.list_sessions(false) else {
            return;
        };
        let session_name = entry_str(entry, "sessionName").unwrap_or_default();
        for session in self.sessions_of(entry, &sessions).servers {
            match self.opts.nono.stop(&session.session_id, false) {
                Ok(_) => self.log(&format!(
                    "Stopped a leftover server session {} of {session_name}.",
                    session.session_id
                )),
                Err(error) => self.log(&format!(
                    "could not stop the leftover server session {}: {}",
                    session.session_id, error.message
                )),
            }
        }
    }

    /// Finds the nono session a supervisor pid belongs to.
    fn session_for_supervisor(&self, supervisor_pid: u32) -> Option<Session> {
        match self.opts.nono.list_sessions(false) {
            Ok(sessions) => sessions
                .into_iter()
                .find(|session| session.supervisor_pid == Some(supervisor_pid)),
            Err(error) => {
                self.log(&format!("could not list nono sessions: {}", error.message));
                None
            }
        }
    }

    /// Launches the agent in the pane and returns once it exits. The agent runs
    /// under `nono run` with the terminal inherited; while it starts, the bridge
    /// checks from outside that every process of the session is confined and
    /// records the result (and shows a Herdr toast when it is not).
    pub fn launch(&self, pane_id: &str, connect: bool) -> Result<LaunchOutcome> {
        let state_dir = self.state_dir();
        let entry = require_pane_entry(state_dir, Some(pane_id))?;
        let agent = agent_for_entry(&self.opts.config, &entry, &self.opts.plugin_root)?;
        let checked = match self.preflight(&entry, &agent) {
            Ok(checked) => checked,
            Err(error) => {
                self.update(
                    pane_id,
                    json!({"lifecycleState": "failed", "lastError": describe_error(&error)}),
                )?;
                return Err(error);
            }
        };
        if let Some(warning) = &checked.warning {
            self.log(&format!("warning: {warning}"));
            self.notify(
                "nono: host service reachable",
                "An unsandboxed OpenCode service is running; see the pane or run doctor.",
            );
        }
        let session_name = entry_str(&entry, "sessionName")
            .unwrap_or_default()
            .to_string();
        let local_path = entry_str(&entry, "localPath")
            .unwrap_or_default()
            .to_string();
        let cwd = launch_cwd(&entry);
        let password = random_hex(24);
        let mut port = None;
        let mut server: Option<RunningServer> = None;
        if agent.server.is_some() {
            self.stop_orphaned_servers(&entry);
            port = Some(pick_free_port().map_err(|error| {
                PluginError::new(
                    ErrorKind::Startup,
                    format!("Could not pick a free port: {error}"),
                )
            })?);
        }
        let argv = with_port_all(
            if connect {
                &agent.resume_argv
            } else {
                &agent.launch_argv
            },
            port,
        );
        let mut extra: Vec<String> = self.opts.config.nono_args.clone();
        if let Some(port) = port {
            extra.extend(["--open-port".to_string(), port.to_string()]);
        }
        let args = build_run_args(&RunArgs {
            profile: &agent.profile_ref,
            session_name: &session_name,
            workspace_root: &local_path,
            allow_paths: &self.opts.config.allow_paths,
            read_paths: &self.opts.config.read_paths,
            silent: self.opts.config.silent,
            extra_args: &extra,
            argv: &argv,
        })?;
        let launch_count = entry
            .get("launchCount")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            + 1.0;
        self.update(
            pane_id,
            json!({
                "lifecycleState": "starting", "lastError": null, "verification": null,
                "profile": agent.profile_ref, "serverProfile": agent.server_profile_ref,
                "port": port, "launchArgv": argv, "launchCount": launch_count, "lastLaunchAt": iso_now(),
            }),
        )?;
        if agent.server.is_some() {
            let port = port.expect("a server needs a port");
            self.log(&format!(
                "Starting {}'s server in nono sandbox {} (profile {}, port {port})...",
                agent.title,
                server_session_name(&session_name),
                agent.server_profile_ref.as_deref().unwrap_or_default()
            ));
            let started = match self.start_server(&entry, &agent, port, &password) {
                Ok(started) => started,
                Err(error) => {
                    self.update(
                        pane_id,
                        json!({"lifecycleState": "failed", "lastError": describe_error(&error)}),
                    )?;
                    return Err(error);
                }
            };
            let pid = started.child.id();
            self.update(
                pane_id,
                json!({"serverSupervisorPid": pid, "serverToken": process_start_token(pid), "serverLog": started.log_file.to_string_lossy()}),
            )?;
            server = Some(started);
        }
        self.log(&format!(
            "Launching {} in nono sandbox {session_name} (profile {}, read-write {local_path})...",
            agent.title, agent.profile_ref
        ));
        let report_agent = self.opts.config.report_agent_status
            && self.opts.herdr.is_some()
            && agent.herdr_detection_kind.is_none();
        if report_agent {
            if let Some(herdr) = &self.opts.herdr {
                let message = format!("{} in nono sandbox {session_name}", agent.title);
                if let Err(error) = herdr.report_agent(
                    pane_id,
                    AGENT_REPORT_SOURCE,
                    &agent.kind,
                    "unknown",
                    Some(&message),
                ) {
                    self.log(&format!(
                        "could not report the agent to Herdr: {}",
                        error.message
                    ));
                }
            }
        }
        let release_agent = |this: &Self| {
            if report_agent {
                if let Some(herdr) = &this.opts.herdr {
                    if let Err(error) =
                        herdr.release_agent(pane_id, AGENT_REPORT_SOURCE, &agent.kind)
                    {
                        this.log(&format!(
                            "could not release the agent in Herdr: {}",
                            error.message
                        ));
                    }
                }
            }
        };
        let mut env = sandbox_env(&self.opts.env, &self.opts.config.agent_env);
        if let Some(spec) = &agent.server {
            env.insert(spec.password_env.clone(), password.clone());
        }
        let spawned = Command::new(&self.opts.nono.bin)
            .args(&args)
            .current_dir(&cwd)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .env_clear()
            .envs(&env)
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                if let Some(running) = server.as_mut() {
                    stop_server(&mut running.child);
                }
                release_agent(self);
                let kind = if error.kind() == std::io::ErrorKind::NotFound {
                    ErrorKind::Startup
                } else {
                    ErrorKind::Unknown
                };
                let failure = PluginError::new(
                    kind,
                    format!("Could not run nono (\"{}\"): {error}. Install nono or set nonoBin in config.json.", self.opts.nono.bin),
                )
                .with_cause(error);
                self.update(pane_id, json!({"lifecycleState": "failed", "lastError": describe_error(&failure), "supervisorPid": null, "supervisorToken": null}))?;
                return Err(failure);
            }
        };
        let supervisor_pid = child.id();
        let signals = SignalGuard::install(true);
        self.stopped_for_verification.store(false, Ordering::SeqCst);
        self.update(
            pane_id,
            json!({"lifecycleState": "running", "supervisorPid": supervisor_pid, "supervisorToken": process_start_token(supervisor_pid), "sessionId": null}),
        )?;
        let server_supervisor_pid = server.as_ref().map(|running| running.child.id());
        let running = AtomicBool::new(true);
        let waited = std::thread::scope(|scope| {
            let verifier = scope.spawn(|| {
                self.verify_while_starting(&VerifyWatch {
                    pane_id,
                    agent: &agent,
                    supervisor_pid,
                    server_supervisor_pid,
                    port,
                    host_service: checked.host_service.as_ref(),
                    host_service_reachable: checked.loopback_open,
                    running: &running,
                })
            });
            let status = loop {
                if let Some(signal) = take_pending() {
                    send_signal(supervisor_pid, signal);
                }
                match child.try_wait() {
                    Ok(Some(status)) => break Ok(status),
                    Ok(None) => {}
                    Err(error) => break Err(error),
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            running.store(false, Ordering::SeqCst);
            // The verification may still be recording its report; wait for it.
            let _ = verifier.join();
            status
        });
        drop(signals);
        {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(TERMINAL_RESTORE_SEQUENCE.as_bytes());
            let _ = out.flush();
        }
        release_agent(self);
        if let Some(running) = server.as_mut() {
            stop_server(&mut running.child);
            self.update(
                pane_id,
                json!({"serverSupervisorPid": null, "serverToken": null}),
            )?;
        }
        let status = match waited {
            Ok(status) => status,
            Err(error) => {
                let failure = PluginError::new(ErrorKind::Unknown, format!("Could not run nono (\"{}\"): {error}. Install nono or set nonoBin in config.json.", self.opts.nono.bin));
                self.update(pane_id, json!({"lifecycleState": "failed", "lastError": describe_error(&failure), "supervisorPid": null, "supervisorToken": null}))?;
                return Err(failure);
            }
        };
        let exit_code = status
            .code()
            .unwrap_or(if status.signal() == Some(libc::SIGINT) {
                130
            } else {
                128 + 15
            });
        if self.stopped_for_verification.load(Ordering::SeqCst) {
            let report = get_pane_entry(state_dir, Some(pane_id))?
                .and_then(|entry| entry.get("verification").cloned());
            let problems: Vec<String> = report
                .as_ref()
                .and_then(|report| report.get("problems"))
                .and_then(Value::as_array)
                .map_or_else(Vec::new, |problems| {
                    problems
                        .iter()
                        .filter_map(|problem| problem.as_str().map(str::to_string))
                        .collect()
                });
            let details = if problems.is_empty() {
                "see info".to_string()
            } else {
                problems.join(" ")
            };
            let error = PluginError::new(
                ErrorKind::Unconfined,
                format!(
                    "The sandbox verification failed, so {} was stopped: {details}",
                    agent.title
                ),
            );
            self.update(
                pane_id,
                json!({"lifecycleState": "failed", "lastExitCode": exit_code, "exitedAt": iso_now(), "supervisorPid": null, "supervisorToken": null, "lastError": describe_error(&error)}),
            )?;
            self.log(&format!("error: {}", error.message));
            self.log("Run verify-sandbox after reconnect to see the process tree, or set \"onVerificationFailure\": \"warn\" to keep such sessions running.");
            // The agent may well have exited cleanly on SIGTERM; the launch still failed.
            return Ok(LaunchOutcome {
                exit_code: if exit_code == 0 { 1 } else { exit_code },
            });
        }
        self.update(
            pane_id,
            json!({"lifecycleState": "exited", "lastExitCode": exit_code, "exitedAt": iso_now(), "supervisorPid": null, "supervisorToken": null}),
        )?;
        self.log(&format!(
            "{} exited with code {exit_code}. Use reconnect to resume it in the sandbox, or open-shell for a shell with the same policy.",
            agent.title
        ));
        Ok(LaunchOutcome { exit_code })
    }

    /// Verifies a freshly started session until it passes or the window ends,
    /// records the report on the mapping, and on failure raises a toast and,
    /// with `onVerificationFailure: "stop"`, ends the session: an agent whose
    /// server or tools are not confined must not keep running. The agent's
    /// server needs a moment to start, so early misses are retried.
    fn verify_while_starting(&self, watch: &VerifyWatch) {
        let config = &self.opts.config;
        if !config.verify_after_start || !procfs_available(&self.opts.proc_root) {
            return;
        }
        let is_running = || watch.running.load(Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_millis(verify_window_ms(&self.opts.env));
        let mut report: Option<VerificationReport> = None;
        let mut session_id: Option<String> = None;
        while is_running() {
            sleep_while(Duration::from_millis(VERIFY_INTERVAL_MS), is_running);
            if !is_running() {
                break;
            }
            if session_id.is_none() {
                session_id = self
                    .session_for_supervisor(watch.supervisor_pid)
                    .map(|session| session.session_id);
            }
            let current = verify_session(&VerifyInput {
                supervisor_pid: Some(watch.supervisor_pid),
                server_supervisor_pid: watch.server_supervisor_pid,
                port: watch.port,
                agent: &watch.agent.adapter,
                host_service: watch.host_service,
                host_service_reachable: watch.host_service_reachable,
                proc_root: &self.opts.proc_root,
            });
            let finished = current.ok == Some(true) || Instant::now() >= deadline;
            report = Some(current);
            if finished {
                break;
            }
        }
        let Some(report) = report else {
            return;
        };
        let mut patch =
            json!({"verification": serde_json::to_value(&report).unwrap_or(Value::Null)});
        if let Some(id) = &session_id {
            patch["sessionId"] = json!(id);
        }
        if let Err(error) = self.update(watch.pane_id, patch) {
            self.log(&format!(
                "could not record the verification: {}",
                error.message
            ));
        }
        if report.ok == Some(false) && is_running() {
            let stopping = config.on_verification_failure == "stop";
            let summary = summarize_verification(serde_json::to_value(&report).ok().as_ref());
            self.notify(
                "nono sandbox verification FAILED",
                &format!(
                    "{summary}{}",
                    if stopping {
                        " The agent was stopped."
                    } else {
                        ""
                    }
                ),
            );
            if stopping {
                self.stopped_for_verification.store(true, Ordering::SeqCst);
                send_signal(watch.supervisor_pid, libc::SIGTERM);
            }
        }
    }

    /// Opens an interactive shell in a new nono sandbox with the agent's policy
    /// and workspace grant. It is a second sandbox, not the agent's own: nono
    /// sandboxes are processes and cannot be entered from outside.
    pub fn shell(&self, pane_id: &str) -> Result<i32> {
        self.acknowledge_shell(pane_id)?;
        let outcome = self.run_shell(pane_id);
        self.release_shell(pane_id);
        outcome
    }

    fn run_shell(&self, pane_id: &str) -> Result<i32> {
        let entry = require_pane_entry(self.state_dir(), Some(pane_id))?;
        let agent = agent_for_entry(&self.opts.config, &entry, &self.opts.plugin_root)?;
        assert_local_path(entry_str(&entry, "localPath").unwrap_or(""))?;
        let session_name = entry_str(&entry, "sessionName").unwrap_or_default();
        let config = &self.opts.config;
        let launch = shell_launch(&config.shell, session_name);
        // The shell gets the policy the agent's tools run under: the server's, when there is one.
        let profile = agent
            .server_profile_ref
            .clone()
            .unwrap_or_else(|| agent.profile_ref.clone());
        let args = build_run_args(&RunArgs {
            profile: &profile,
            session_name: &shell_session_name(session_name),
            workspace_root: entry_str(&entry, "localPath").unwrap_or_default(),
            allow_paths: &config.allow_paths,
            read_paths: &config.read_paths,
            silent: config.silent,
            extra_args: &config.nono_args,
            argv: &launch.argv,
        })?;
        self.log(&format!("Opening {} in a nono sandbox with the policy {session_name}'s tools run under (profile {profile})...", config.shell));
        let mut env = sandbox_env(&self.opts.env, &config.agent_env);
        env.extend(launch.env.iter().cloned());
        let mut child = Command::new(&self.opts.nono.bin)
            .args(&args)
            .current_dir(launch_cwd(&entry))
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .env_clear()
            .envs(&env)
            .spawn()
            .map_err(|error| {
                PluginError::new(
                    ErrorKind::Startup,
                    format!("Could not run nono (\"{}\"): {error}", self.opts.nono.bin),
                )
                .with_cause(error)
            })?;
        let signals = SignalGuard::install(false);
        let status = child.wait();
        drop(signals);
        {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(TERMINAL_RESTORE_SEQUENCE.as_bytes());
            let _ = out.flush();
        }
        let code = status
            .map_err(|error| {
                PluginError::new(
                    ErrorKind::Startup,
                    format!("Could not run nono (\"{}\"): {error}", self.opts.nono.bin),
                )
            })?
            .code()
            .unwrap_or(1);
        self.log(&format!("Shell exited with code {code}."));
        Ok(code)
    }

    /// Records this process as an open-shell session of the mapping.
    pub fn acknowledge_shell(&self, pane_id: &str) -> Result<()> {
        with_pane_lock(self.state_dir(), pane_id, || {
            let entry = require_pane_entry(self.state_dir(), Some(pane_id))?;
            let own = u64::from(std::process::id());
            let mut shells: Vec<Value> = live_shells(&entry)
                .into_iter()
                .filter(|item| item.get("pid").and_then(Value::as_u64) != Some(own))
                .collect();
            shells.push(json!({"pid": own, "token": own_start_token(), "since": iso_now(), "paneId": pane_id}));
            self.update(pane_id, json!({"shellPids": shells})).map(drop)
        })
    }

    /// Removes this process from the mapping's open-shell sessions.
    pub fn release_shell(&self, pane_id: &str) {
        let released = with_pane_lock(self.state_dir(), pane_id, || {
            let Some(entry) = get_pane_entry(self.state_dir(), Some(pane_id))? else {
                return Ok(());
            };
            let own = u64::from(std::process::id());
            let remaining: Vec<Value> = entry
                .get("shellPids")
                .and_then(Value::as_array)
                .map_or_else(Vec::new, |shells| {
                    shells
                        .iter()
                        .filter(|item| item.get("pid").and_then(Value::as_u64) != Some(own))
                        .cloned()
                        .collect()
                });
            self.update(pane_id, json!({"shellPids": remaining}))
                .map(drop)
        });
        if let Err(error) = released {
            self.log(&format!(
                "Could not release the shell record for pane {pane_id}: {}",
                error.message
            ));
        }
    }

    /// Records that a bridge process started for a pane. The action that typed
    /// the bridge command waits for the launch id it generated, so an older
    /// acknowledgement can never satisfy a newer launch.
    pub fn acknowledge_bridge(&self, pane_id: &str, launch_id: Option<&str>) -> Result<()> {
        with_pane_lock(self.state_dir(), pane_id, || {
            let entry = require_pane_entry(self.state_dir(), Some(pane_id))?;
            if bridge_is_running(&entry)
                && entry_pid(&entry, "bridgePid") != Some(std::process::id())
            {
                let show = |key: &str| {
                    entry
                        .get(key)
                        .map_or_else(|| "undefined".to_string(), crate::util::js_string)
                };
                return Err(PluginError::new(
                    ErrorKind::Conflict,
                    format!(
                        "Pane {pane_id} already runs a bridge for {} (pid {}, since {}); not starting a second agent. Exit that agent first, or use another pane.",
                        show("sessionName"),
                        show("bridgePid"),
                        show("bridgeStartedAt")
                    ),
                ));
            }
            self.update(
                pane_id,
                json!({"bridgeStartedAt": iso_now(), "bridgeLaunchId": launch_id, "bridgePid": std::process::id(), "bridgeToken": own_start_token()}),
            )
            .map(drop)
        })
    }

    /// Clears this process's ownership of a mapping when the bridge exits.
    pub fn release_bridge(&self, pane_id: &str) {
        let released = with_pane_lock(self.state_dir(), pane_id, || {
            let Some(entry) = get_pane_entry(self.state_dir(), Some(pane_id))? else {
                return Ok(());
            };
            if entry_pid(&entry, "bridgePid") != Some(std::process::id()) {
                return Ok(());
            }
            self.update(
                pane_id,
                json!({"bridgePid": null, "bridgeToken": null, "bridgeExitedAt": iso_now()}),
            )
            .map(drop)
        });
        if let Err(error) = released {
            self.log(&format!(
                "Could not release the mapping for pane {pane_id} on exit: {}",
                error.message
            ));
        }
    }

    /// The live nono sessions of a mapping: its agent session (matched by
    /// supervisor pid, else by name), its private server sessions and its
    /// open-shell sessions (by name).
    pub fn sessions_of(&self, entry: &Entry, sessions: &[Session]) -> MappingSessions {
        let running: Vec<&Session> = sessions
            .iter()
            .filter(|session| session.status.as_deref() != Some("exited"))
            .collect();
        let session_name = entry_str(entry, "sessionName").unwrap_or_default();
        let supervisor = entry_pid(entry, "supervisorPid");
        let agent = running
            .iter()
            .find(|session| supervisor.is_some() && session.supervisor_pid == supervisor)
            .or_else(|| {
                running
                    .iter()
                    .find(|session| session.name.as_deref() == Some(session_name))
            })
            .map(|session| (*session).clone());
        let named = |name: String| -> Vec<Session> {
            running
                .iter()
                .filter(|session| session.name.as_deref() == Some(name.as_str()))
                .map(|session| (*session).clone())
                .collect()
        };
        MappingSessions {
            agent,
            shells: named(shell_session_name(session_name)),
            servers: named(server_session_name(session_name)),
        }
    }

    /// Stops the agent's running nono session (SIGTERM, SIGKILL after nono's
    /// grace period) and waits for the bridge to record the exit.
    pub fn stop(&self, pane_id: &str, force: bool) -> Result<StopOutcome> {
        let state_dir = self.state_dir();
        let entry = require_pane_entry(state_dir, Some(pane_id))?;
        let session_name = entry_str(&entry, "sessionName")
            .unwrap_or_default()
            .to_string();
        let live = self.sessions_of(&entry, &self.opts.nono.list_sessions(false)?);
        if live.agent.is_none() && live.servers.is_empty() {
            return Err(PluginError::new(
                ErrorKind::NotFound,
                format!("No nono session is running for {session_name}; nothing to stop."),
            ));
        }
        if let Some(agent) = &live.agent {
            self.opts.nono.stop(&agent.session_id, force)?;
        }
        let deadline = Instant::now() + Duration::from_millis(STOP_WAIT_MS);
        while Instant::now() < deadline
            && bridge_is_running(&get_pane_entry(state_dir, Some(pane_id))?.unwrap_or_default())
        {
            std::thread::sleep(Duration::from_millis(200));
        }
        // The bridge takes its server down when the client exits; a server whose bridge is gone is stopped here.
        let mut stopped_servers = Vec::new();
        for session in self
            .sessions_of(&entry, &self.opts.nono.list_sessions(false)?)
            .servers
        {
            match self.opts.nono.stop(&session.session_id, force) {
                Ok(_) => stopped_servers.push(session.session_id),
                Err(error) if error.kind == ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(StopOutcome {
            session_name,
            session_id: live.agent.map(|session| session.session_id),
            server_session_ids: stopped_servers,
        })
    }

    /// Verifies the mapping's running session now and records the report.
    pub fn verify(&self, pane_id: &str) -> Result<VerifyOutcome> {
        let state_dir = self.state_dir();
        let entry = require_pane_entry(state_dir, Some(pane_id))?;
        let agent = agent_for_entry(&self.opts.config, &entry, &self.opts.plugin_root)?;
        let session_name = entry_str(&entry, "sessionName").unwrap_or_default();
        let still = |pid: Option<u32>, token: Option<&str>| match (pid, token) {
            (Some(pid), Some(token)) if !token.is_empty() => {
                process_start_token(pid).as_deref() == Some(token)
            }
            (pid, _) => pid.is_some(),
        };
        let mut supervisor_pid = entry_pid(&entry, "supervisorPid")
            .filter(|pid| still(Some(*pid), entry_str(&entry, "supervisorToken")));
        if supervisor_pid.is_none() {
            // A session started by an older bridge, or one whose mapping missed the pid: ask nono.
            match self.opts.nono.list_sessions(false) {
                Ok(sessions) => {
                    supervisor_pid = self
                        .sessions_of(&entry, &sessions)
                        .agent
                        .and_then(|session| session.supervisor_pid)
                }
                Err(error) => self.log(&format!("could not list nono sessions: {}", error.message)),
            }
        }
        let Some(supervisor_pid) = supervisor_pid else {
            return Err(PluginError::new(
                ErrorKind::NotFound,
                format!("No nono session is running for {session_name}; start or reconnect the agent, then verify."),
            ));
        };
        let mut server_supervisor_pid = entry_pid(&entry, "serverSupervisorPid")
            .filter(|pid| still(Some(*pid), entry_str(&entry, "serverToken")));
        if agent.server.is_some() && server_supervisor_pid.is_none() {
            match self.opts.nono.list_sessions(false) {
                Ok(sessions) => {
                    server_supervisor_pid = self
                        .sessions_of(&entry, &sessions)
                        .servers
                        .first()
                        .and_then(|session| session.supervisor_pid)
                }
                Err(error) => self.log(&format!("could not list nono sessions: {}", error.message)),
            }
        }
        let host_service = self.host_service_for(&agent);
        let reachable = host_service.as_ref().is_some_and(|service| service.running)
            && self.tools_loopback(&agent).open;
        let report = verify_session(&VerifyInput {
            supervisor_pid: Some(supervisor_pid),
            server_supervisor_pid,
            port: entry
                .get("port")
                .and_then(Value::as_u64)
                .and_then(|port| u16::try_from(port).ok()),
            agent: &agent.adapter,
            host_service: host_service.as_ref(),
            host_service_reachable: reachable,
            proc_root: &self.opts.proc_root,
        });
        self.update(
            pane_id,
            json!({"verification": serde_json::to_value(&report).unwrap_or(Value::Null)}),
        )?;
        Ok(VerifyOutcome {
            entry: get_pane_entry(state_dir, Some(pane_id))?,
            report,
        })
    }

    /// Describes a mapping together with its live sessions when nono answers.
    pub fn describe(&self, pane_id: &str) -> Result<Description> {
        let entry = require_pane_entry(self.state_dir(), Some(pane_id))?;
        let agent = agent_for_entry(&self.opts.config, &entry, &self.opts.plugin_root)?;
        let (sessions, session_error) = match self.opts.nono.list_sessions(false) {
            Ok(list) => {
                let live = self.sessions_of(&entry, &list);
                (
                    Some(LiveSessions {
                        agent: live.agent,
                        server: live.servers.first().cloned(),
                        shells: live.shells,
                    }),
                    None,
                )
            }
            Err(error) => (None, Some(describe_error(&error))),
        };
        let verification = entry
            .get("verification")
            .filter(|value| !value.is_null())
            .cloned();
        Ok(Description {
            agent: AgentSummary {
                kind: agent.kind.clone(),
                title: agent.title.clone(),
                profile: agent.profile_ref.clone(),
                server_profile: agent.server_profile_ref.clone(),
                launch_argv: agent.launch_argv.clone(),
                resume_argv: agent.resume_argv.clone(),
                server_argv: agent.server.as_ref().map(|server| server.command.clone()),
                herdr_detection_kind: agent.herdr_detection_kind.clone(),
            },
            mapping: entry,
            sessions,
            session_error,
            verification,
        })
    }

    /// Lists every mapping with the state of its agent session.
    pub fn list_all(&self) -> Result<ListAll> {
        let state = load_state(self.state_dir())?;
        let (sessions, session_error) = match self.opts.nono.list_sessions(false) {
            Ok(list) => (Some(list), None),
            Err(error) => (None, Some(describe_error(&error))),
        };
        let mappings = state
            .panes
            .iter()
            .map(|(_, entry)| {
                let live = sessions.as_ref().map(|list| self.sessions_of(entry, list));
                let mut item = Map::new();
                for key in ["paneId", "sessionName", "agentKind", "localPath"] {
                    if let Some(value) = entry.get(key) {
                        item.insert(key.into(), value.clone());
                    }
                }
                item.insert(
                    "workdir".into(),
                    entry
                        .get("workdir")
                        .filter(|value| !value.is_null())
                        .or_else(|| entry.get("localPath"))
                        .cloned()
                        .unwrap_or(Value::Null),
                );
                if let Some(value) = entry.get("lifecycleState") {
                    item.insert("lifecycleState".into(), value.clone());
                }
                item.insert(
                    "running".into(),
                    live.as_ref()
                        .map_or(Value::Null, |live| json!(live.agent.is_some())),
                );
                item.insert(
                    "sessionId".into(),
                    json!(live
                        .as_ref()
                        .and_then(|live| live.agent.as_ref())
                        .map(|session| session.session_id.clone())),
                );
                item.insert(
                    "serverRunning".into(),
                    live.as_ref()
                        .map_or(Value::Null, |live| json!(!live.servers.is_empty())),
                );
                item.insert(
                    "shells".into(),
                    live.as_ref()
                        .map_or(Value::Null, |live| json!(live.shells.len())),
                );
                let verification = entry.get("verification").filter(|value| !value.is_null());
                item.insert(
                    "verification".into(),
                    verification.map_or(Value::Null, |report| {
                        json!(summarize_verification(Some(report)))
                    }),
                );
                item.insert(
                    "verified".into(),
                    verification
                        .and_then(|report| report.get("ok"))
                        .cloned()
                        .unwrap_or(Value::Null),
                );
                Value::Object(item)
            })
            .collect();
        Ok(ListAll {
            mappings,
            session_error,
        })
    }

    /// Drops every mapping whose Herdr pane is gone and whose agent and shells
    /// are not running. Each candidate is decided again under its lock, so a
    /// mapping rewritten since the snapshot, or one whose bridge or shell still
    /// runs (the pane closed under a live agent), stays.
    pub fn prune(&self, live_pane_ids: &[String]) -> Result<PruneOutcome> {
        let state_dir = self.state_dir();
        let mut outcome = PruneOutcome::default();
        for (pane_id, entry) in load_state(state_dir)?.panes {
            if live_pane_ids.contains(&pane_id) {
                continue;
            }
            let session_name = entry_str(&entry, "sessionName")
                .unwrap_or_default()
                .to_string();
            let decision = with_pane_lock(state_dir, &pane_id, || {
                if let Some(now) = get_pane_entry(state_dir, Some(&pane_id))? {
                    if bridge_is_running(&now) || shell_is_running(&now) {
                        return Ok("busy");
                    }
                }
                Ok(
                    if delete_pane_entry_if_unchanged(state_dir, &pane_id, &entry)? {
                        "pruned"
                    } else {
                        "changed"
                    },
                )
            })?;
            if decision == "pruned" {
                outcome.pruned.push(PrunedMapping {
                    pane_id,
                    session_name,
                });
            } else {
                let reason = if decision == "busy" {
                    "its agent or shell still runs; stop it first"
                } else {
                    "mapping changed while pruning; run prune-mappings again"
                };
                outcome.kept.push(KeptMapping {
                    pane_id,
                    session_name,
                    reason: reason.into(),
                });
            }
        }
        Ok(outcome)
    }
}

/// Whether the tools' sandbox can reach localhost, and why.
pub struct ToolsLoopbackReport {
    pub open: bool,
    pub reason: Option<String>,
}

/// What the background verification of a launch needs.
struct VerifyWatch<'a> {
    pane_id: &'a str,
    agent: &'a ResolvedAgent,
    supervisor_pid: u32,
    server_supervisor_pid: Option<u32>,
    port: Option<u16>,
    host_service: Option<&'a HostService>,
    host_service_reachable: bool,
    running: &'a AtomicBool,
}

/// Stops a private server: SIGTERM to its nono supervisor, SIGKILL after a grace period.
fn stop_server(child: &mut Child) {
    if child
        .try_wait()
        .map(|status| status.is_some())
        .unwrap_or(true)
    {
        return;
    }
    if !send_signal(child.id(), libc::SIGTERM) {
        return;
    }
    let grace = Instant::now() + Duration::from_millis(SERVER_STOP_GRACE_MS);
    while Instant::now() < grace {
        if child
            .try_wait()
            .map(|status| status.is_some())
            .unwrap_or(true)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let end = Instant::now() + Duration::from_millis(1000);
    while Instant::now() < end
        && child
            .try_wait()
            .map(|status| status.is_none())
            .unwrap_or(false)
    {
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::validate_config;
    use crate::state::{save_pane_entry, State};
    use std::os::unix::fs::PermissionsExt;

    fn words(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn entry(value: Value) -> Entry {
        obj(value)
    }

    /// A process that only sleeps but whose command line is `args` (after `sh -c`), killed on drop.
    struct Lookalike(Child);

    impl Lookalike {
        fn new(args: &[&str]) -> Lookalike {
            let child = Command::new("sh")
                .arg("-c")
                .arg("sleep 60")
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            // The shell execs sleep only after reading its arguments; wait until /proc shows them.
            for _ in 0..200 {
                if process_command_line(child.id()).is_some_and(|line| line.contains("sleep 60")) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Lookalike(child)
        }

        fn pid(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for Lookalike {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn process_owns_recognises_the_rust_bridge_and_the_node_bridge_for_a_pane() {
        let rust = Lookalike::new(&["herdr-nono", "bridge", "connect", "--pane-id", "pane-1"]);
        let node = Lookalike::new(&["src/bridge.mjs", "connect", "--pane-id", "pane-1"]);
        for bridge in [&rust, &node] {
            assert!(process_owns(Some(bridge.pid()), None, Some("pane-1"), None));
            assert!(
                process_owns(Some(bridge.pid()), None, None, None),
                "no pane to match"
            );
            assert!(
                !process_owns(Some(bridge.pid()), None, Some("pane-2"), None),
                "another pane's bridge"
            );
            assert!(
                !process_owns(Some(bridge.pid()), None, Some("pane-1"), Some("shell")),
                "not a shell bridge"
            );
            assert!(process_owns(
                Some(bridge.pid()),
                None,
                Some("pane-1"),
                Some("connect")
            ));
        }
        let shell = Lookalike::new(&[
            "/p/bin/herdr-nono",
            "bridge",
            "shell",
            "--pane-id",
            "pane-1",
        ]);
        assert!(process_owns(
            Some(shell.pid()),
            None,
            Some("pane-1"),
            Some("shell")
        ));
        let unrelated = Lookalike::new(&["herdr-nono", "events", "--pane-id", "pane-1"]);
        assert!(
            !process_owns(Some(unrelated.pid()), None, Some("pane-1"), None),
            "only the bridge subcommand counts"
        );
        let wrong_binary = Lookalike::new(&["other-tool", "bridge", "--pane-id", "pane-1"]);
        assert!(!process_owns(
            Some(wrong_binary.pid()),
            None,
            Some("pane-1"),
            None
        ));
    }

    #[test]
    fn process_owns_checks_liveness_and_the_start_token() {
        let bridge = Lookalike::new(&["herdr-nono", "bridge", "start", "--pane-id", "p"]);
        let token = process_start_token(bridge.pid()).unwrap();
        assert!(process_owns(
            Some(bridge.pid()),
            Some(&token),
            Some("p"),
            None
        ));
        assert!(
            process_owns(Some(bridge.pid()), Some("-"), Some("p"), None),
            "an unknown token is not held against it"
        );
        assert!(
            !process_owns(Some(bridge.pid()), Some("linux:0"), Some("p"), None),
            "a recycled pid carries another token"
        );
        assert!(!process_owns(Some(2_147_483_647), None, None, None));
        assert!(!process_owns(None, None, None, None));
        assert!(!process_owns(Some(0), None, None, None));
    }

    #[test]
    fn bridge_and_shell_liveness_come_from_the_mapping() {
        let bridge = Lookalike::new(&["herdr-nono", "bridge", "connect", "--pane-id", "pane-1"]);
        let busy = entry(json!({"paneId": "pane-1", "bridgePid": bridge.pid()}));
        assert!(bridge_is_running(&busy));
        assert!(!bridge_is_running(&entry(
            json!({"paneId": "pane-2", "bridgePid": bridge.pid()})
        )));
        assert!(!bridge_is_running(&entry(
            json!({"paneId": "pane-1", "bridgePid": null})
        )));
        let shell = Lookalike::new(&["herdr-nono", "bridge", "shell", "--pane-id", "pane-1"]);
        let with_shell = entry(
            json!({"paneId": "pane-1", "shellPids": [{"pid": shell.pid(), "paneId": "pane-1"}, {"pid": 2_147_483_647}]}),
        );
        assert_eq!(live_shells(&with_shell).len(), 1, "a dead shell is dropped");
        assert!(shell_is_running(&with_shell));
        assert!(!shell_is_running(&entry(json!({"paneId": "pane-1"}))));
        let bridge_as_shell =
            entry(json!({"paneId": "pane-1", "shellPids": [{"pid": bridge.pid()}]}));
        assert!(
            !shell_is_running(&bridge_as_shell),
            "a non-shell bridge is not a shell session"
        );
    }

    #[test]
    fn sandbox_env_strips_what_must_not_reach_the_sandbox_and_adds_agent_env() {
        let env = |pairs: &[(&str, &str)]| -> Env {
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect()
        };
        let nono = sandbox_env(
            &env(&[
                ("PATH", "/bin"),
                ("NONO_ALLOW_DOMAIN", "localhost"),
                ("NONO_NETWORK_PROFILE", "open"),
                ("NONO_UPSTREAM_PROXY", "127.0.0.1:4096"),
                ("NONO_ALLOW", "/"),
                ("NONO_THEME", "dark"),
            ]),
            &[],
        );
        assert_eq!(nono, env(&[("PATH", "/bin"), ("NONO_THEME", "dark")]));
        let sockets = sandbox_env(
            &env(&[
                ("PATH", "/bin"),
                ("HERDR_SOCKET_PATH", "/s"),
                ("HERDR_PANE_ID", "p"),
                ("SSH_AUTH_SOCK", "/a"),
                ("DBUS_SESSION_BUS_ADDRESS", "unix:x"),
                ("TMUX", "t"),
                ("KEEP", "1"),
            ]),
            &words(&["EXTRA=a=b"]),
        );
        assert_eq!(
            sockets,
            env(&[("PATH", "/bin"), ("KEEP", "1"), ("EXTRA", "a=b")])
        );
    }

    #[test]
    fn shell_launch_knows_bash_zsh_and_fish() {
        let bash = shell_launch("bash", "herdr-opencode-abc123def456");
        assert_eq!(bash.argv, ["bash", "--noprofile", "--norc"]);
        assert_eq!(
            bash.env,
            [("PS1".to_string(), "[nono:abc123] \\w \\$ ".to_string())]
        );
        assert_eq!(
            shell_launch("/usr/bin/zsh", "s-1").argv,
            ["/usr/bin/zsh", "--no-rcs"]
        );
        assert_eq!(
            shell_launch("/usr/bin/zsh", "s-1").env,
            [("PS1".to_string(), "[nono:1] %~ %# ".to_string())]
        );
        assert_eq!(
            shell_launch("fish", "s-1"),
            ShellLaunch {
                argv: words(&["fish", "--no-config"]),
                env: Vec::new()
            }
        );
        assert_eq!(
            shell_launch("/bin/true", "herdr-opencode-abc123def456").env,
            [("PS1".to_string(), "[nono:abc123] $ ".to_string())]
        );
    }

    #[test]
    fn local_paths_must_be_absolute_existing_directories_and_never_the_root_or_home() {
        let dir = tempfile::tempdir().unwrap();
        assert!(assert_local_path(dir.path().to_str().unwrap()).is_ok());
        let relative = assert_local_path("relative/dir").unwrap_err();
        assert_eq!(relative.kind, ErrorKind::Target);
        assert!(
            relative
                .message
                .contains("must be absolute (got \"relative/dir\")"),
            "{}",
            relative.message
        );
        assert!(assert_local_path("/definitely/not/here")
            .unwrap_err()
            .message
            .contains("is not an existing directory"));
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert!(
            assert_local_path(file.to_str().unwrap()).is_err(),
            "a file is not a directory"
        );
        assert_eq!(
            assert_workspace_root(dir.path().to_str().unwrap()).unwrap(),
            std::fs::canonicalize(dir.path()).unwrap()
        );
        assert!(assert_workspace_root("/")
            .unwrap_err()
            .message
            .starts_with("Refusing to grant / to a sandboxed agent."));
        if let Some(home) = std::env::var_os("HOME").filter(|home| Path::new(home).is_dir()) {
            assert!(
                assert_workspace_root(home.to_str().unwrap()).is_err(),
                "the home directory is refused"
            );
        }
    }

    #[test]
    fn launch_cwd_prefers_an_existing_workdir() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let root = dir.path().to_str().unwrap();
        assert_eq!(
            launch_cwd(&entry(json!({"localPath": root, "workdir": sub}))),
            sub
        );
        assert_eq!(
            launch_cwd(&entry(
                json!({"localPath": root, "workdir": dir.path().join("gone")})
            )),
            dir.path()
        );
        assert_eq!(launch_cwd(&entry(json!({"localPath": root}))), dir.path());
    }

    #[test]
    fn find_executable_searches_path_like_a_shell() {
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::create_dir(dir.path().join("adir")).unwrap();
        let env = |path: String| Env::from([("PATH".to_string(), path)]);
        let path = format!("/nonexistent::{}", dir.path().display());
        assert_eq!(
            find_executable("tool", &env(path.clone())),
            Some(tool.clone())
        );
        assert_eq!(
            find_executable("adir", &env(path.clone())),
            None,
            "a directory is not an executable"
        );
        assert_eq!(find_executable("missing", &env(path)), None);
        assert_eq!(
            find_executable(tool.to_str().unwrap(), &Env::new()),
            Some(tool)
        );
        assert_eq!(find_executable("/no/such/tool", &Env::new()), None);
        assert_eq!(
            find_executable("tool", &Env::new()),
            None,
            "no PATH, no search"
        );
    }

    #[test]
    fn the_verification_window_and_server_timeout_follow_the_environment() {
        let env = |name: &str, value: &str| Env::from([(name.to_string(), value.to_string())]);
        assert_eq!(verify_window_ms(&Env::new()), VERIFY_WINDOW_MS);
        assert_eq!(verify_window_ms(&env(VERIFY_WINDOW_ENV, "1500")), 1500);
        assert_eq!(verify_window_ms(&env(VERIFY_WINDOW_ENV, "0")), 0);
        assert_eq!(
            verify_window_ms(&env(VERIFY_WINDOW_ENV, "")),
            0,
            "an empty value is 0, as Number(\"\") is"
        );
        assert_eq!(
            verify_window_ms(&env(VERIFY_WINDOW_ENV, "-5")),
            VERIFY_WINDOW_MS
        );
        assert_eq!(
            verify_window_ms(&env(VERIFY_WINDOW_ENV, "soon")),
            VERIFY_WINDOW_MS
        );
        assert_eq!(
            server_ready_timeout_ms(&Env::new()),
            SERVER_READY_TIMEOUT_MS
        );
        assert_eq!(
            server_ready_timeout_ms(&env(SERVER_READY_TIMEOUT_ENV, "800")),
            800
        );
        assert_eq!(
            server_ready_timeout_ms(&env(SERVER_READY_TIMEOUT_ENV, "0")),
            SERVER_READY_TIMEOUT_MS
        );
    }

    #[test]
    fn probe_server_reads_the_status_of_an_authenticated_get() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let record = std::sync::Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 1024];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let ok = request.contains(&format!(
                    "Authorization: Basic {}",
                    base64(b"opencode:secret")
                ));
                *record.lock().unwrap() = request;
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {} X\r\nContent-Length: 2\r\n\r\n{{}}",
                        if ok { 200 } else { 401 }
                    )
                    .as_bytes(),
                );
            }
        });
        assert_eq!(
            probe_server(port, "/api/info", "opencode", "secret"),
            Some(200)
        );
        assert!(seen
            .lock()
            .unwrap()
            .starts_with("GET /api/info HTTP/1.1\r\nHost: 127.0.0.1:"));
        assert_eq!(
            probe_server(port, "/api/info", "opencode", "wrong"),
            Some(401)
        );
        assert_eq!(probe_server(1, "/", "u", "p"), None, "nothing listens");
    }

    // ---- lifecycle against shell fakes ----

    /// A fake `nono`: `ps --json` prints the file `sessions.json` next to it; `stop <id>` records the id.
    fn fake_nono(dir: &Path) -> PathBuf {
        let script = dir.join("nono");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
case "$1" in
  ps) cat '{dir}/sessions.json' ;;
  stop) echo "$2" >> '{dir}/stopped'; echo "Stopped $2" ;;
  profile) echo '{{"name":"p","network":{{"block":true}}}}' ;;
  *) echo "unsupported: $*" >&2; exit 2 ;;
esac
"#,
                dir = dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("sessions.json"), "[]").unwrap();
        script
    }

    struct Fixture {
        dir: tempfile::TempDir,
        lifecycle: Lifecycle,
        logs: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Fixture {
        fn new(config: Value) -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let nono = fake_nono(dir.path());
            let env = Env::from([
                ("PATH".to_string(), std::env::var("PATH").unwrap()),
                (
                    "HOME".to_string(),
                    dir.path().join("home").to_string_lossy().into_owned(),
                ),
            ]);
            let logs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let sink = std::sync::Arc::clone(&logs);
            let mut options = LifecycleOptions::new(
                dir.path().join("state"),
                validate_config(&config).unwrap(),
                "/plugin",
                NonoClient::new(nono.to_str().unwrap(), env.clone()),
                env,
            );
            options.log = Box::new(move |line| sink.lock().unwrap().push(line.to_string()));
            Fixture {
                lifecycle: Lifecycle::new(options),
                dir,
                logs,
            }
        }

        fn state_dir(&self) -> PathBuf {
            self.dir.path().join("state")
        }

        fn save(&self, pane_id: &str, extra: Value) -> Entry {
            let mut base = entry(json!({
                "sessionName": "herdr-opencode-abc123def456", "agentKind": "opencode", "localPath": "/w", "workdir": "/w",
                "lifecycleState": "exited", "launchCount": 1,
            }));
            base.extend(entry(extra));
            save_pane_entry(&self.state_dir(), pane_id, &base).unwrap()
        }

        fn set_sessions(&self, sessions: Value) {
            std::fs::write(self.dir.path().join("sessions.json"), sessions.to_string()).unwrap();
        }

        fn stopped(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("stopped"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    #[test]
    fn sessions_of_matches_the_agent_by_supervisor_pid_then_name_and_finds_servers_and_shells() {
        let f = Fixture::new(json!({}));
        let sessions = crate::nono::normalize_session_list(&json!([
            {"session_id": "a", "name": "herdr-opencode-abc123def456", "supervisor_pid": 10, "status": "running"},
            {"session_id": "b", "name": "other", "supervisor_pid": 11, "status": "running"},
            {"session_id": "s", "name": "herdr-opencode-abc123def456-server", "supervisor_pid": 12, "status": "running"},
            {"session_id": "h", "name": "herdr-opencode-abc123def456-shell", "supervisor_pid": 13, "status": "running"},
            {"session_id": "x", "name": "herdr-opencode-abc123def456-shell", "supervisor_pid": 14, "status": "exited"},
        ]))
        .unwrap();
        let by_pid = f.lifecycle.sessions_of(
            &entry(json!({"sessionName": "none", "supervisorPid": 11})),
            &sessions,
        );
        assert_eq!(
            by_pid.agent.unwrap().session_id,
            "b",
            "the supervisor pid wins over the name"
        );
        let by_name = f.lifecycle.sessions_of(
            &entry(json!({"sessionName": "herdr-opencode-abc123def456"})),
            &sessions,
        );
        assert_eq!(by_name.agent.unwrap().session_id, "a");
        assert_eq!(
            by_name
                .servers
                .iter()
                .map(|s| s.session_id.as_str())
                .collect::<Vec<_>>(),
            ["s"]
        );
        assert_eq!(
            by_name
                .shells
                .iter()
                .map(|s| s.session_id.as_str())
                .collect::<Vec<_>>(),
            ["h"],
            "exited sessions are not live"
        );
        let none = f
            .lifecycle
            .sessions_of(&entry(json!({"sessionName": "nobody"})), &sessions);
        assert_eq!(none, MappingSessions::default());
    }

    #[test]
    fn acknowledging_and_releasing_the_bridge_marks_and_clears_the_mapping() {
        let f = Fixture::new(json!({}));
        f.save("pane-1", json!({}));
        f.lifecycle
            .acknowledge_bridge("pane-1", Some("launch-9"))
            .unwrap();
        let busy = get_pane_entry(&f.state_dir(), Some("pane-1"))
            .unwrap()
            .unwrap();
        assert_eq!(busy["bridgePid"], std::process::id());
        assert_eq!(busy["bridgeLaunchId"], "launch-9");
        assert_eq!(busy["bridgeToken"].as_str(), own_start_token().as_deref());
        // The same process may acknowledge again (a second launch in the same bridge).
        f.lifecycle.acknowledge_bridge("pane-1", None).unwrap();
        f.lifecycle.release_bridge("pane-1");
        let released = get_pane_entry(&f.state_dir(), Some("pane-1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            (&released["bridgePid"], &released["bridgeToken"]),
            (&Value::Null, &Value::Null)
        );
        assert!(released["bridgeExitedAt"].is_string());
        f.lifecycle.release_bridge("pane-1");
        f.lifecycle.release_bridge("no-such-pane");
        assert!(
            f.logs.lock().unwrap().is_empty(),
            "{:?}",
            f.logs.lock().unwrap()
        );
    }

    #[test]
    fn a_second_bridge_is_refused_while_the_first_runs() {
        let f = Fixture::new(json!({}));
        let other = Lookalike::new(&["herdr-nono", "bridge", "start", "--pane-id", "pane-1"]);
        f.save(
            "pane-1",
            json!({"bridgePid": other.pid(), "bridgeStartedAt": "2026-10-06T00:00:00.000Z"}),
        );
        let error = f.lifecycle.acknowledge_bridge("pane-1", None).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Conflict);
        assert!(error.message.starts_with(&format!("Pane pane-1 already runs a bridge for herdr-opencode-abc123def456 (pid {}, since 2026-10-06T00:00:00.000Z); not starting a second agent.", other.pid())), "{}", error.message);
        // Once that bridge is gone the pane is free again.
        drop(other);
        f.lifecycle.acknowledge_bridge("pane-1", None).unwrap();
    }

    #[test]
    fn shell_records_come_and_go_with_the_process() {
        let f = Fixture::new(json!({}));
        f.save("pane-1", json!({}));
        f.lifecycle.acknowledge_shell("pane-1").unwrap();
        let entry = get_pane_entry(&f.state_dir(), Some("pane-1"))
            .unwrap()
            .unwrap();
        assert_eq!(entry["shellPids"][0]["pid"], std::process::id());
        assert_eq!(entry["shellPids"][0]["paneId"], "pane-1");
        f.lifecycle.release_shell("pane-1");
        assert_eq!(
            get_pane_entry(&f.state_dir(), Some("pane-1"))
                .unwrap()
                .unwrap()["shellPids"],
            json!([])
        );
        f.lifecycle.release_shell("no-such-pane");
    }

    #[test]
    fn prune_drops_mappings_of_gone_panes_and_keeps_busy_or_live_ones() {
        let f = Fixture::new(json!({}));
        let bridge = Lookalike::new(&["herdr-nono", "bridge", "connect", "--pane-id", "busy"]);
        f.save("gone", json!({"sessionName": "s-gone"}));
        f.save("alive", json!({"sessionName": "s-alive"}));
        f.save(
            "busy",
            json!({"sessionName": "s-busy", "bridgePid": bridge.pid()}),
        );
        let outcome = f.lifecycle.prune(&words(&["alive"])).unwrap();
        assert_eq!(
            outcome.pruned,
            [PrunedMapping {
                pane_id: "gone".into(),
                session_name: "s-gone".into()
            }]
        );
        assert_eq!(
            outcome.kept,
            [KeptMapping {
                pane_id: "busy".into(),
                session_name: "s-busy".into(),
                reason: "its agent or shell still runs; stop it first".into()
            }]
        );
        let state: State = load_state(&f.state_dir()).unwrap();
        let mut left = state.pane_ids();
        left.sort_unstable();
        assert_eq!(left, ["alive", "busy"]);
    }

    #[test]
    fn stop_stops_the_agent_session_and_reports_it() {
        let f = Fixture::new(json!({}));
        f.save("pane-1", json!({}));
        let none = f.lifecycle.stop("pane-1", false).unwrap_err();
        assert_eq!(none.kind, ErrorKind::NotFound);
        assert_eq!(
            none.message,
            "No nono session is running for herdr-opencode-abc123def456; nothing to stop."
        );
        f.set_sessions(json!([{"session_id": "s1", "name": "herdr-opencode-abc123def456", "supervisor_pid": 1, "status": "running"}]));
        let stopped = f.lifecycle.stop("pane-1", false).unwrap();
        assert_eq!(
            (stopped.session_name.as_str(), stopped.session_id.as_deref()),
            ("herdr-opencode-abc123def456", Some("s1"))
        );
        assert_eq!(f.stopped(), ["s1"]);
        assert!(f
            .lifecycle
            .stop("unmapped", false)
            .unwrap_err()
            .message
            .contains("No sandboxed agent is mapped to pane unmapped"));
    }

    #[test]
    fn describe_and_list_all_show_the_mapping_with_its_live_sessions() {
        let f = Fixture::new(json!({}));
        f.save("pane-1", json!({"verification": {"supported": true, "ok": true, "processes": [{}, {}], "server": {"pid": 7}}}));
        f.set_sessions(json!([{"session_id": "s1", "name": "herdr-opencode-abc123def456", "supervisor_pid": 1, "status": "running"}]));
        let description = serde_json::to_value(f.lifecycle.describe("pane-1").unwrap()).unwrap();
        assert_eq!(description["agent"]["kind"], "opencode");
        assert_eq!(
            description["agent"]["serverArgv"],
            json!([
                "opencode",
                "serve",
                "--hostname",
                "127.0.0.1",
                "--port",
                "{port}"
            ])
        );
        assert_eq!(description["sessions"]["agent"]["sessionId"], "s1");
        assert_eq!(description["sessions"]["server"], Value::Null);
        assert_eq!(description["verification"]["ok"], true);
        assert_eq!(description["sessionError"], Value::Null);
        let listed = serde_json::to_value(f.lifecycle.list_all().unwrap()).unwrap();
        let row = &listed["mappings"][0];
        assert_eq!(row["paneId"], "pane-1");
        assert_eq!(
            (
                row["running"].clone(),
                row["sessionId"].clone(),
                row["serverRunning"].clone(),
                row["shells"].clone()
            ),
            (json!(true), json!("s1"), json!(false), json!(0))
        );
        assert_eq!(row["verification"], "confined: 2 processes, server pid 7");
        assert_eq!(row["verified"], true);
        assert_eq!(listed["sessionError"], Value::Null);
    }

    #[test]
    fn a_nono_that_cannot_list_sessions_is_reported_not_raised() {
        let f = Fixture::new(json!({"nonoBin": "/definitely/not/nono"}));
        let broken = Lifecycle::new(LifecycleOptions::new(
            f.state_dir(),
            validate_config(&json!({})).unwrap(),
            "/plugin",
            NonoClient::new("/definitely/not/nono", Env::new()),
            Env::new(),
        ));
        f.save("pane-1", json!({}));
        let listed = serde_json::to_value(broken.list_all().unwrap()).unwrap();
        assert_eq!(listed["sessionError"]["kind"], "startup");
        assert_eq!(listed["mappings"][0]["running"], Value::Null);
        let description = serde_json::to_value(broken.describe("pane-1").unwrap()).unwrap();
        assert_eq!(description["sessions"], Value::Null);
        assert_eq!(description["sessionError"]["kind"], "startup");
    }

    #[test]
    fn verify_reports_a_missing_session_and_tools_loopback_reads_the_profile() {
        let f = Fixture::new(json!({}));
        f.save("pane-1", json!({}));
        let error = f.lifecycle.verify("pane-1").unwrap_err();
        assert_eq!(error.kind, ErrorKind::NotFound);
        assert!(error
            .message
            .contains("start or reconnect the agent, then verify"));
        let agent = resolve_agent(&validate_config(&json!({})).unwrap(), "/plugin").unwrap();
        let loopback = f.lifecycle.tools_loopback(&agent);
        assert!(!loopback.open, "the fake profile blocks the network");
        let broken = Lifecycle::new(LifecycleOptions::new(
            f.state_dir(),
            validate_config(&json!({})).unwrap(),
            "/plugin",
            NonoClient::new("/definitely/not/nono", Env::new()),
            Env::new(),
        ));
        let unknown = broken.tools_loopback(&agent);
        assert!(unknown.open, "a profile nono cannot resolve counts as open");
        assert!(unknown.reason.unwrap().contains(
            "nono could not resolve profile /plugin/profiles/herdr-opencode-server.json"
        ));
    }

    /// A manual check on a real host: a session that the Node bridge started
    /// under the real nono is verified and then stopped by this binary. Run by
    /// `scripts`-free hand: start the Node bridge, then
    /// `HERDR_NONO_REAL_STATE_DIR=... HERDR_NONO_REAL_CONFIG_DIR=... HERDR_NONO_REAL_NONO=... HERDR_NONO_REAL_PANE=... HERDR_NONO_REAL_ROOT=...
    ///  cargo test a_node_started_real_session -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a session started by the Node bridge under the real nono; see the phase 3 notes"]
    fn a_node_started_real_session_is_verified_and_stopped_by_this_binary() {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        let env: Env = std::env::vars().collect();
        let config =
            crate::config::load_config(Path::new(&var("HERDR_NONO_REAL_CONFIG_DIR")), &env)
                .unwrap()
                .config;
        let nono = NonoClient::new(var("HERDR_NONO_REAL_NONO"), env.clone());
        let lifecycle = Lifecycle::new(LifecycleOptions::new(
            var("HERDR_NONO_REAL_STATE_DIR"),
            config,
            var("HERDR_NONO_REAL_ROOT"),
            nono,
            env,
        ));
        let pane = var("HERDR_NONO_REAL_PANE");
        let entry =
            require_pane_entry(Path::new(&var("HERDR_NONO_REAL_STATE_DIR")), Some(&pane)).unwrap();
        assert!(
            bridge_is_running(&entry),
            "the Node bridge is recognised: {entry:?}"
        );
        let outcome = lifecycle.verify(&pane).unwrap();
        println!(
            "verification: {}",
            summarize_verification(serde_json::to_value(&outcome.report).ok().as_ref())
        );
        assert_eq!(
            outcome.report.ok,
            Some(true),
            "{:?}",
            outcome.report.problems
        );
        assert!(
            !outcome.report.processes.is_empty(),
            "the sandboxed process tree was found"
        );
        assert!(outcome
            .report
            .processes
            .iter()
            .all(|process| process.confined));
        let stopped = lifecycle.stop(&pane, false).unwrap();
        println!("stopped session {:?}", stopped.session_id);
        assert!(stopped.session_id.is_some());
        let after = get_pane_entry(Path::new(&var("HERDR_NONO_REAL_STATE_DIR")), Some(&pane))
            .unwrap()
            .unwrap();
        assert!(
            !bridge_is_running(&after),
            "the Node bridge exited after the stop"
        );
    }

    #[test]
    fn describe_error_records_kind_message_and_time() {
        let described = describe_error(&PluginError::new(ErrorKind::Unconfined, "bad"));
        assert_eq!(
            (&described["kind"], &described["message"]),
            (&json!("unconfined"), &json!("bad"))
        );
        assert!(described["at"].as_str().unwrap().ends_with('Z'));
    }
}

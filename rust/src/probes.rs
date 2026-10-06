//! Escape probes for `doctor`: the plugin binary re-executes itself
//! (`herdr-nono probe`) inside a throwaway nono sandbox with the profile the
//! agent's tools run under and tries the ways out this plugin knows about
//! (Herdr's control socket, the user's systemd and D-Bus sockets, the SSH and
//! GPG agents, a direct connect to the OpenCode host service's port, a canary
//! listener on the host's loopback interface reached directly and through
//! nono's proxy, secrets in the home directory, leaked environment variables,
//! the host service's password). Each check says what a well-confined agent
//! should see.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Map, Value};
use url::Url;

use crate::constants::NONO_CALL_TIMEOUT_MS;
use crate::context::Env;
use crate::errors::{ErrorKind, PluginError, Result};
use crate::exec::{run_cli, ExecError, RunOptions};
use crate::hostservice::{home_dir, opencode_service_files};
use crate::util::{js_string, random_hex};

/// The line the in-sandbox probe prints before it exits.
pub const PROBE_MARKER: &str = "HERDR_NONO_PROBE ";

/// The names a sandbox could use for the host's loopback interface through
/// nono's proxy: IPv4 and IPv6 forms, the unspecified address, and a public
/// wildcard DNS name that resolves to 127.0.0.1.
pub const LOOPBACK_NAMES: [&str; 6] = [
    "127.0.0.1",
    "localhost",
    "0.0.0.0",
    "127.1",
    "[::1]",
    "127.0.0.1.nip.io",
];

/// The checks the probe runs and what a confined agent should see.
pub fn probe_targets(env: &Env) -> Value {
    let home = home_dir(env);
    let non_empty = |name: &str| env.get(name).filter(|value| !value.is_empty()).cloned();
    let runtime: Option<PathBuf> = non_empty("XDG_RUNTIME_DIR").map(PathBuf::from).or_else(|| {
        // SAFETY: getuid has no preconditions and cannot fail.
        Some(PathBuf::from(format!("/run/user/{}", unsafe {
            libc::getuid()
        })))
    });
    let config_home =
        non_empty("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
    let herdr_socket = non_empty("HERDR_SOCKET_PATH").map_or_else(
        || config_home.join("herdr").join("herdr.sock"),
        PathBuf::from,
    );
    let service = opencode_service_files(env);
    // No service file (or a default port in its URL): OpenCode's default port.
    let service_port = std::fs::read_to_string(&service.state_file)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|state| {
            state
                .get("url")?
                .as_str()
                .and_then(|url| Url::parse(url).ok())
        })
        .and_then(|url| url.port())
        .unwrap_or(4096);
    let under_runtime = |parts: &[&str]| {
        runtime.as_ref().map(|dir| {
            parts
                .iter()
                .fold(dir.clone(), |path, part| path.join(part))
                .to_string_lossy()
                .into_owned()
        })
    };
    let ssh_agent = non_empty("SSH_AUTH_SOCK").or_else(|| under_runtime(&["openssh_agent"]));
    json!({
        "tcp": {"opencodeServicePort": service_port},
        "sockets": {
            "herdrSocket": herdr_socket,
            "systemdUser": under_runtime(&["systemd", "private"]),
            "sessionBus": under_runtime(&["bus"]),
            "sshAgent": ssh_agent,
            "gpgAgent": under_runtime(&["gnupg", "S.gpg-agent"]),
            "dockerSocket": "/var/run/docker.sock",
        },
        "files": {"opencodeServicePassword": service.state_file},
        "dirs": {"sshKeys": home.join(".ssh")},
        "writes": {"homeDirectory": home},
    })
}

/// What one probe result should be for a confined agent, and why it matters.
pub struct Expectation {
    pub check: &'static str,
    pub expect: &'static [&'static str],
    pub severity: &'static str,
    pub why: &'static str,
}

/// The expectations, in the order the checks are reported.
pub const PROBE_EXPECTATIONS: [Expectation; 13] = [
    Expectation { check: "herdrSocket", expect: &["denied", "absent"], severity: "critical", why: "Herdr's control socket can type commands into any pane, outside the sandbox" },
    Expectation { check: "opencodeServicePort", expect: &["denied"], severity: "critical", why: "a direct localhost connect reaches the OpenCode host service, which runs tools outside the sandbox" },
    Expectation { check: "loopbackCanary", expect: &["denied"], severity: "critical", why: "a direct connect reaches a listener on the host's 127.0.0.1, so any localhost service, such as the OpenCode host service, is reachable" },
    Expectation { check: "loopbackViaProxy", expect: &["denied", "absent"], severity: "critical", why: "nono's proxy forwards to a listener on the host's loopback interface; an allowed domain (such as \"*\") covers localhost, so the OpenCode host service is reachable" },
    Expectation { check: "systemdUser", expect: &["denied", "absent"], severity: "critical", why: "the user's systemd manager can start processes outside the sandbox" },
    Expectation { check: "sessionBus", expect: &["denied", "absent"], severity: "critical", why: "the D-Bus session bus can ask systemd to start processes outside the sandbox" },
    Expectation { check: "dockerSocket", expect: &["denied", "absent"], severity: "critical", why: "the Docker socket gives root-equivalent access to the host" },
    Expectation { check: "sshAgent", expect: &["denied", "absent"], severity: "high", why: "the SSH agent signs with your keys" },
    Expectation { check: "gpgAgent", expect: &["denied", "absent"], severity: "high", why: "the GPG agent signs and decrypts with your keys" },
    Expectation { check: "sshKeys", expect: &["denied", "absent"], severity: "high", why: "~/.ssh holds private keys" },
    Expectation { check: "homeDirectory", expect: &["denied"], severity: "high", why: "writes to the home directory can plant shell start-up files" },
    Expectation { check: "opencodeServicePassword", expect: &["denied", "absent"], severity: "warning", why: "it holds the password of the OpenCode host service; harmless while the service's port is unreachable (opencodeServicePort)" },
    Expectation { check: "env", expect: &[], severity: "high", why: "these variables point sandboxed code at sockets outside the sandbox" },
];

/// One evaluated probe result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub check: String,
    pub result: String,
    pub ok: bool,
    pub severity: String,
    pub why: String,
}

/// Evaluates raw probe results into a list of checks.
pub fn evaluate_probes(results: &Value) -> Vec<Check> {
    let mut checks = Vec::new();
    for expectation in &PROBE_EXPECTATIONS {
        let Some(value) = results.get(expectation.check) else {
            continue;
        };
        let (result, ok) = if expectation.check == "env" {
            let names: Vec<String> = value
                .as_array()
                .map_or_else(Vec::new, |items| items.iter().map(js_string).collect());
            (
                if names.is_empty() {
                    "none".to_string()
                } else {
                    names.join(" ")
                },
                names.is_empty(),
            )
        } else {
            let result = js_string(value);
            let ok = expectation.expect.contains(&result.as_str());
            (result, ok)
        };
        checks.push(Check {
            check: expectation.check.into(),
            result,
            ok,
            severity: expectation.severity.into(),
            why: expectation.why.into(),
        });
    }
    let marker = results
        .get("marker")
        .is_some_and(|marker| marker.as_bool().unwrap_or(!marker.is_null()));
    checks.push(Check {
        check: "marker".into(),
        result: if marker {
            "NONO_CAP_FILE set"
        } else {
            "NONO_CAP_FILE missing"
        }
        .into(),
        ok: marker,
        severity: "critical".into(),
        why: "a sandboxed process carries nono's capability marker".into(),
    });
    checks
}

// ---- the host side: a canary and the sandbox run --------------------------------

/// Listens on an ephemeral port of the host's 127.0.0.1 until closed: the
/// target of the loopback probes. Nothing inside the sandbox should reach it.
pub struct Canary {
    pub port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

pub fn start_canary() -> std::io::Result<Canary> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            match listener.accept() {
                // Greeting nobody: the connection is dropped, as the JS canary destroys it.
                Ok((stream, _)) => drop(stream),
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    });
    Ok(Canary {
        port,
        stop,
        thread: Some(thread),
    })
}

impl Canary {
    /// Stops listening; the port refuses connections afterwards.
    pub fn close(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Canary {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A scratch directory that is removed again.
struct Workspace(PathBuf);

impl Workspace {
    fn create(prefix: &str) -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!("{prefix}{}", random_hex(6)));
        std::fs::create_dir(&path)?;
        Ok(Workspace(path))
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What the host side of the probe needs.
pub struct ProbeOptions<'a> {
    pub nono_bin: &'a str,
    pub profile: &'a str,
    pub env: &'a Env,
    /// The executable to run inside the sandbox; `None` copies this binary into the workspace.
    pub probe_exe: Option<&'a Path>,
    pub timeout: Duration,
}

impl<'a> ProbeOptions<'a> {
    pub fn new(nono_bin: &'a str, profile: &'a str, env: &'a Env) -> Self {
        ProbeOptions {
            nono_bin,
            profile,
            env,
            probe_exe: None,
            timeout: Duration::from_millis(NONO_CALL_TIMEOUT_MS),
        }
    }
}

/// The targets the probe tried and what it found.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProbeRun {
    pub targets: Value,
    pub checks: Vec<Check>,
}

/// Runs the probe inside a throwaway sandbox with the given profile. The
/// probe's environment deliberately keeps HERDR_* and the socket variables, so
/// the result shows whether the profile itself strips them. A canary listener
/// on the host's loopback interface runs for the duration; the sandbox tries
/// it directly and through nono's proxy.
pub fn run_probes(options: &ProbeOptions) -> Result<ProbeRun> {
    let startup = |message: String| PluginError::new(ErrorKind::Startup, message);
    let workspace = Workspace::create("herdr-nono-probe-")
        .map_err(|error| startup(format!("Could not create the probe workspace: {error}")))?;
    let canary = start_canary()
        .map_err(|error| startup(format!("Could not start the loopback canary: {error}")))?;
    let exe = match options.probe_exe {
        Some(exe) => exe.to_path_buf(),
        None => {
            // The workspace is the one directory the sandbox may always run files from.
            let current = std::env::current_exe().map_err(|error| {
                startup(format!(
                    "Could not find the plugin binary to run the probe with: {error}"
                ))
            })?;
            let copy = workspace.0.join("herdr-nono-probe");
            std::fs::copy(&current, &copy).map_err(|error| {
                startup(format!(
                    "Could not copy {} into the probe workspace: {error}",
                    current.display()
                ))
            })?;
            copy
        }
    };
    let mut targets = probe_targets(options.env);
    targets["canary"] = json!({"port": canary.port, "names": LOOPBACK_NAMES});
    let workspace_arg = workspace.0.to_string_lossy().into_owned();
    let exe_arg = exe.to_string_lossy().into_owned();
    let args = [
        "run",
        "--silent",
        "--profile",
        options.profile,
        "--name",
        "herdr-nono-probe",
        "--allow",
        &workspace_arg,
        "--",
        &exe_arg,
        "probe",
        &targets.to_string(),
    ];
    let run_options = RunOptions {
        env: Some(options.env),
        cwd: Some(&workspace.0),
        timeout: options.timeout,
        cancel: None,
    };
    // The canary keeps accepting on its own thread while the probe runs.
    let result = run_cli(options.nono_bin, &args, &run_options);
    canary.close();
    let output = match result {
        Ok(output) => output,
        Err(ExecError::Spawn(error)) => {
            return Err(startup(format!(
                "Could not run the escape probe through nono: {error}"
            ))
            .with_cause(error));
        }
        Err(ExecError::TimedOut(partial)) => {
            return Err(startup(format!(
                "Could not run the escape probe through nono: timed out after {}s",
                options.timeout.as_secs()
            ))
            .with_output(partial.output()));
        }
        Err(ExecError::Cancelled(partial)) => {
            return Err(startup(
                "Could not run the escape probe through nono: cancelled".to_string(),
            )
            .with_output(partial.output()));
        }
    };
    let combined = output.output();
    let Some(line) = output
        .stdout
        .lines()
        .find(|line| line.starts_with(PROBE_MARKER))
    else {
        let exit = output
            .status
            .map_or_else(|| "a signal".to_string(), |code| code.to_string());
        return Err(startup(format!(
            "The escape probe did not report (nono exit {exit}); the profile may not allow running {} from the workspace.",
            exe.display()
        ))
        .with_output(combined));
    };
    let results: Value = serde_json::from_str(&line[PROBE_MARKER.len()..]).map_err(|error| {
        startup(format!("The escape probe printed invalid JSON: {error}")).with_output(combined)
    })?;
    Ok(ProbeRun {
        targets,
        checks: evaluate_probes(&results),
    })
}

// ---- the sandbox side: what `herdr-nono probe` does -----------------------------

const SOCKET_TIMEOUT: Duration = Duration::from_secs(2);
const PROXY_TIMEOUT: Duration = Duration::from_secs(3);

fn str_of(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.is_empty())
}

/// Connects to a Unix socket path: `allowed`, `absent`, `refused`, `denied` or `timeout`.
fn probe_socket(file: Option<&str>) -> &'static str {
    let Some(file) = file else {
        return "absent";
    };
    if let Err(error) = std::fs::metadata(file) {
        if error.kind() == std::io::ErrorKind::NotFound {
            return "absent";
        }
    }
    let (sender, receiver) = mpsc::channel();
    let path = file.to_string();
    std::thread::spawn(move || {
        let _ = sender.send(UnixStream::connect(path).map(drop));
    });
    match receiver.recv_timeout(SOCKET_TIMEOUT) {
        Ok(Ok(())) => "allowed",
        Ok(Err(error)) => match error.kind() {
            std::io::ErrorKind::NotFound => "absent",
            std::io::ErrorKind::ConnectionRefused => "refused",
            _ => "denied",
        },
        Err(_) => "timeout",
    }
}

/// The error code a failed TCP connect reports, as a name.
fn errno_name(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(libc::ECONNREFUSED) => "ECONNREFUSED",
        Some(libc::EACCES) => "EACCES",
        Some(libc::EPERM) => "EPERM",
        Some(libc::ENETUNREACH) => "ENETUNREACH",
        Some(libc::EHOSTUNREACH) => "EHOSTUNREACH",
        Some(libc::ETIMEDOUT) => "ETIMEDOUT",
        Some(libc::ECONNRESET) => "ECONNRESET",
        _ => "error",
    }
    .to_string()
}

/// Connects to 127.0.0.1:port: `allowed`, `refused`, `denied`, `timeout` or an error code.
fn probe_tcp(port: u16) -> String {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    match TcpStream::connect_timeout(&address, SOCKET_TIMEOUT) {
        Ok(_) => "allowed".into(),
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => "timeout".into(),
        Err(error) => match error.raw_os_error() {
            Some(libc::ECONNREFUSED) => "refused".into(),
            Some(libc::EACCES | libc::EPERM) => "denied".into(),
            _ => errno_name(&error),
        },
    }
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let decoded = (bytes[index] == b'%' && index + 2 < bytes.len())
            .then(|| {
                std::str::from_utf8(&bytes[index + 1..index + 3])
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            })
            .flatten();
        match decoded {
            Some(byte) => {
                out.push(byte);
                index += 3;
            }
            None => {
                out.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The status code of an HTTP/1.x response line at the start of `buffer`.
fn http_status(buffer: &str) -> Option<u16> {
    let rest = buffer
        .strip_prefix("HTTP/1.0 ")
        .or_else(|| buffer.strip_prefix("HTTP/1.1 "))?;
    let digits = rest.get(..3)?;
    digits
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| digits.parse().ok())
        .flatten()
}

/// CONNECTs through the proxy to `name:port`; the status code, or `None` on any failure or silence.
fn proxy_connect(proxy: &Url, auth: Option<&str>, name: &str, port: u16) -> Option<u16> {
    let host = proxy.host_str()?;
    let proxy_port = proxy.port().unwrap_or(80);
    let address = (host, proxy_port).to_socket_addrs_first()?;
    let mut stream = TcpStream::connect_timeout(&address, PROXY_TIMEOUT).ok()?;
    let credentials = auth.map_or_else(String::new, |value| {
        format!("Proxy-Authorization: {value}\r\n")
    });
    let request =
        format!("CONNECT {name}:{port} HTTP/1.1\r\nHost: {name}:{port}\r\n{credentials}\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let deadline = Instant::now() + PROXY_TIMEOUT;
    let mut buffer = String::new();
    let mut chunk = [0u8; 512];
    while Instant::now() < deadline {
        stream
            .set_read_timeout(Some(
                deadline
                    .saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(1)),
            ))
            .ok()?;
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(read) => {
                buffer.push_str(&String::from_utf8_lossy(&chunk[..read]));
                if let Some(status) = http_status(&buffer) {
                    return Some(status);
                }
            }
        }
    }
    None
}

/// `ToSocketAddrs` for a `(host, port)` pair, first address only.
trait FirstAddress {
    fn to_socket_addrs_first(&self) -> Option<SocketAddr>;
}

impl FirstAddress for (&str, u16) {
    fn to_socket_addrs_first(&self) -> Option<SocketAddr> {
        std::net::ToSocketAddrs::to_socket_addrs(self).ok()?.next()
    }
}

/// CONNECT through nono's proxy (HTTP_PROXY carries its address and token) to
/// each name of the canary; the result lists every name the proxy forwarded.
fn probe_via_proxy(names: &[String], port: u16, env: &dyn Fn(&str) -> Option<String>) -> String {
    let raw = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]
        .iter()
        .find_map(|name| env(name).filter(|value| !value.is_empty()));
    let Some(proxy) = raw.and_then(|raw| Url::parse(&raw).ok()) else {
        return "absent".into();
    };
    let auth = (!proxy.username().is_empty()).then(|| {
        format!(
            "Basic {}",
            base64(
                format!(
                    "{}:{}",
                    percent_decode(proxy.username()),
                    percent_decode(proxy.password().unwrap_or(""))
                )
                .as_bytes()
            )
        )
    });
    let mut reached = Vec::new();
    for name in names {
        // nono rate-limits connects in proxy mode; spaced requests stay under it.
        std::thread::sleep(Duration::from_millis(150));
        if proxy_connect(&proxy, auth.as_deref(), name, port) == Some(200) {
            reached.push(name.as_str());
        }
    }
    if reached.is_empty() {
        "denied".into()
    } else {
        format!("allowed via {}", reached.join(", "))
    }
}

fn probe_readable(file: &str) -> &'static str {
    match std::fs::File::open(file).and_then(|mut handle| handle.read(&mut [0u8; 1])) {
        Ok(_) => "allowed",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "absent",
        Err(_) => "denied",
    }
}

fn probe_listable(dir: &str) -> &'static str {
    match std::fs::read_dir(dir) {
        Ok(_) => "allowed",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "absent",
        Err(_) => "denied",
    }
}

fn probe_writable(dir: &str) -> &'static str {
    let file = Path::new(dir).join(format!(".herdr-nono-probe-{}", std::process::id()));
    match std::fs::write(&file, "").and_then(|()| std::fs::remove_file(&file)) {
        Ok(()) => "allowed",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "absent",
        Err(_) => "denied",
    }
}

/// Whether an environment variable name is one the sandbox must not inherit.
fn is_leaked_env_name(name: &str) -> bool {
    name.starts_with("HERDR_") || name == "SSH_AUTH_SOCK" || name == "DBUS_SESSION_BUS_ADDRESS"
}

/// The checks that run inside the sandbox. It receives its targets as JSON and
/// returns one object of results; it never reads file contents out.
pub fn run_probe_checks(
    targets: &Value,
    env: &dyn Fn(&str) -> Option<String>,
    environment_names: &[String],
) -> Value {
    let mut results = Map::new();
    let entries = |key: &str| -> Vec<(String, Value)> {
        targets
            .get(key)
            .and_then(Value::as_object)
            .map_or_else(Vec::new, |map| {
                map.iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect()
            })
    };
    for (name, file) in entries("sockets") {
        results.insert(name, json!(probe_socket(str_of(&file))));
    }
    for (name, port) in entries("tcp") {
        let result = port
            .as_u64()
            .and_then(|port| u16::try_from(port).ok())
            .map_or_else(|| "error".to_string(), probe_tcp);
        results.insert(name, json!(result));
    }
    if let Some(canary) = targets.get("canary").filter(|canary| !canary.is_null()) {
        let port = canary
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .unwrap_or(0);
        let names: Vec<String> = canary
            .get("names")
            .and_then(Value::as_array)
            .map_or_else(Vec::new, |names| names.iter().map(js_string).collect());
        results.insert("loopbackCanary".into(), json!(probe_tcp(port)));
        results.insert(
            "loopbackViaProxy".into(),
            json!(probe_via_proxy(&names, port, env)),
        );
    }
    for (name, file) in entries("files") {
        results.insert(name, json!(str_of(&file).map_or("absent", probe_readable)));
    }
    for (name, dir) in entries("dirs") {
        results.insert(name, json!(str_of(&dir).map_or("absent", probe_listable)));
    }
    for (name, dir) in entries("writes") {
        results.insert(name, json!(str_of(&dir).map_or("absent", probe_writable)));
    }
    let mut leaked: Vec<&String> = environment_names
        .iter()
        .filter(|name| is_leaked_env_name(name))
        .collect();
    leaked.sort();
    results.insert("env".into(), json!(leaked));
    results.insert(
        "marker".into(),
        json!(env("NONO_CAP_FILE").is_some_and(|value| !value.is_empty())),
    );
    Value::Object(results)
}

/// `herdr-nono probe <targets-json>`: runs the checks, prints the result line
/// and exits at once (a pending connect must not hold the sandbox open).
pub fn probe_main(args: &[String]) -> std::process::ExitCode {
    let targets: Value = match args.first().map(|text| serde_json::from_str(text)) {
        Some(Ok(targets)) => targets,
        _ => {
            eprintln!("herdr-nono probe needs the probe targets as one JSON argument");
            return std::process::ExitCode::from(2);
        }
    };
    let lookup = |name: &str| std::env::var(name).ok();
    let names: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
    let results = run_probe_checks(&targets, &lookup, &names);
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{PROBE_MARKER}{results}");
    let _ = out.flush();
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    fn env_of(pairs: &[(&str, &str)]) -> Env {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    fn by_name(checks: Vec<Check>) -> std::collections::HashMap<String, Check> {
        checks
            .into_iter()
            .map(|check| (check.check.clone(), check))
            .collect()
    }

    #[test]
    fn probe_targets_aim_at_herdrs_socket_the_users_buses_and_agents_and_the_opencode_service_file()
    {
        let targets = probe_targets(&env_of(&[
            ("HOME", "/home/u"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("HERDR_SOCKET_PATH", "/home/u/.config/herdr/herdr.sock"),
        ]));
        assert_eq!(
            targets["sockets"]["herdrSocket"],
            "/home/u/.config/herdr/herdr.sock"
        );
        assert_eq!(
            targets["sockets"]["systemdUser"],
            "/run/user/1000/systemd/private"
        );
        assert_eq!(targets["sockets"]["sessionBus"], "/run/user/1000/bus");
        assert_eq!(
            targets["sockets"]["sshAgent"],
            "/run/user/1000/openssh_agent"
        );
        assert_eq!(
            targets["sockets"]["gpgAgent"],
            "/run/user/1000/gnupg/S.gpg-agent"
        );
        assert_eq!(targets["sockets"]["dockerSocket"], "/var/run/docker.sock");
        assert_eq!(
            targets["files"]["opencodeServicePassword"],
            "/home/u/.local/state/opencode/service.json"
        );
        assert_eq!(targets["dirs"]["sshKeys"], "/home/u/.ssh");
        assert_eq!(targets["writes"]["homeDirectory"], "/home/u");
        assert_eq!(targets["tcp"]["opencodeServicePort"], 4096);
        let fallback = probe_targets(&env_of(&[("HOME", "/home/u")]));
        assert_eq!(
            fallback["sockets"]["herdrSocket"], "/home/u/.config/herdr/herdr.sock",
            "falls back to Herdr's default socket"
        );
        let with_agent = probe_targets(&env_of(&[("HOME", "/h"), ("SSH_AUTH_SOCK", "/tmp/agent")]));
        assert_eq!(with_agent["sockets"]["sshAgent"], "/tmp/agent");
    }

    #[test]
    fn the_service_port_comes_from_the_service_file() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".local/state/opencode");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("service.json"),
            r#"{"url":"http://127.0.0.1:5123"}"#,
        )
        .unwrap();
        let targets = probe_targets(&env_of(&[("HOME", home.path().to_str().unwrap())]));
        assert_eq!(targets["tcp"]["opencodeServicePort"], 5123);
    }

    #[test]
    fn evaluate_probes_flags_reachable_sockets_and_leaked_variables_with_their_severity() {
        let checks = by_name(evaluate_probes(
            &json!({"herdrSocket": "allowed", "sessionBus": "denied", "sshKeys": "denied", "homeDirectory": "allowed", "opencodeServicePassword": "allowed", "env": ["HERDR_SOCKET_PATH"], "marker": true}),
        ));
        assert!(!checks["herdrSocket"].ok);
        assert_eq!(checks["herdrSocket"].severity, "critical");
        assert!(checks["sessionBus"].ok);
        assert!(!checks["homeDirectory"].ok);
        assert_eq!(checks["opencodeServicePassword"].severity, "warning");
        assert!(!checks["env"].ok);
        assert_eq!(checks["env"].result, "HERDR_SOCKET_PATH");
        assert!(checks["marker"].ok);
        assert!(!by_name(evaluate_probes(&json!({"marker": false})))["marker"].ok);
        assert!(by_name(evaluate_probes(&json!({"env": []})))["env"].ok);
        assert_eq!(
            by_name(evaluate_probes(&json!({"env": []})))["env"].result,
            "none"
        );
        assert!(PROBE_EXPECTATIONS
            .iter()
            .any(|expectation| expectation.check == "dockerSocket"));
        let order: Vec<String> = evaluate_probes(
            &json!({"marker": true, "sshAgent": "absent", "herdrSocket": "denied"}),
        )
        .into_iter()
        .map(|check| check.check)
        .collect();
        assert_eq!(
            order,
            ["herdrSocket", "sshAgent", "marker"],
            "reported in the order of the expectations, the marker last"
        );
    }

    #[test]
    fn evaluate_probes_fails_doctor_when_the_canary_on_localhost_is_reachable_directly_or_through_nonos_proxy(
    ) {
        let evaluate = |results: Value| {
            by_name(evaluate_probes(&{
                let mut all = json!({"marker": true});
                all.as_object_mut()
                    .unwrap()
                    .extend(results.as_object().unwrap().clone());
                all
            }))
        };
        let proxied = evaluate(
            json!({"loopbackCanary": "denied", "loopbackViaProxy": "allowed via 127.0.0.1, localhost"}),
        );
        assert!(proxied["loopbackCanary"].ok);
        assert!(!proxied["loopbackViaProxy"].ok);
        assert_eq!(proxied["loopbackViaProxy"].severity, "critical");
        assert!(
            evaluate(json!({"loopbackViaProxy": "absent"}))["loopbackViaProxy"].ok,
            "no proxy: direct connects are what count"
        );
        assert!(!evaluate(json!({"loopbackCanary": "allowed"}))["loopbackCanary"].ok);
    }

    #[test]
    fn the_canary_listens_on_127_0_0_1_until_closed() {
        let canary = start_canary().unwrap();
        let port = canary.port;
        assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
        canary.close();
        let refused = TcpStream::connect(("127.0.0.1", port)).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::ConnectionRefused);
    }

    /// A fake `nono` that prints the probe line FAKE_PROBE (or a clean one) when asked to run something.
    fn fake_nono(dir: &Path, fail: bool) -> PathBuf {
        let script = dir.join(if fail { "nono-fail" } else { "nono" });
        let body = if fail {
            "echo 'nono: something went wrong' >&2\nexit 1\n".to_string()
        } else {
            format!(
                "printf '%s\\n' \"$*\" > '{}/args'\nprintf 'HERDR_NONO_PROBE %s\\n' '{{\"herdrSocket\":\"denied\",\"opencodeServicePort\":\"denied\",\"loopbackCanary\":\"denied\",\"loopbackViaProxy\":\"denied\",\"sshAgent\":\"absent\",\"homeDirectory\":\"denied\",\"env\":[],\"marker\":true}}'\n",
                dir.display()
            )
        };
        std::fs::write(&script, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[test]
    fn run_probes_goes_through_nono_run_with_the_profile_and_parses_the_probe_line() {
        let dir = tempfile::tempdir().unwrap();
        let nono = fake_nono(dir.path(), false);
        let env = env_of(&[("HOME", "/nonexistent"), ("PATH", "/usr/bin:/bin")]);
        let mut options = ProbeOptions::new(nono.to_str().unwrap(), "/p.json", &env);
        options.probe_exe = Some(Path::new("/bin/true"));
        let run = run_probes(&options).unwrap();
        assert!(run.checks.iter().all(|check| check.ok), "{:?}", run.checks);
        assert!(
            run.targets["canary"]["port"].as_u64().unwrap() > 0,
            "a canary listens on the host's loopback interface while the probe runs"
        );
        assert_eq!(run.targets["canary"]["names"], json!(LOOPBACK_NAMES));
        let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
        assert!(
            args.starts_with("run --silent --profile /p.json --name herdr-nono-probe --allow /"),
            "{args}"
        );
        assert!(args.contains(" -- /bin/true probe {"), "{args}");
    }

    #[test]
    fn run_probes_names_the_cause_when_the_probe_does_not_report_and_when_nono_cannot_start() {
        let dir = tempfile::tempdir().unwrap();
        let failing = fake_nono(dir.path(), true);
        let env = env_of(&[("HOME", "/nonexistent"), ("PATH", "/usr/bin:/bin")]);
        let mut options = ProbeOptions::new(failing.to_str().unwrap(), "/p.json", &env);
        options.probe_exe = Some(Path::new("/bin/true"));
        let error = run_probes(&options).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Startup);
        assert!(
            error.message.contains("did not report (nono exit 1)")
                && error.message.contains("may not allow running /bin/true"),
            "{}",
            error.message
        );
        assert!(error.output.contains("something went wrong"));
        let mut missing = ProbeOptions::new("/definitely/not/nono", "/p.json", &env);
        missing.probe_exe = Some(Path::new("/bin/true"));
        assert!(run_probes(&missing)
            .unwrap_err()
            .message
            .starts_with("Could not run the escape probe through nono: "));
    }

    #[test]
    fn run_probes_copies_this_binary_into_the_workspace_when_no_probe_exe_is_given() {
        let dir = tempfile::tempdir().unwrap();
        let nono = fake_nono(dir.path(), false);
        let env = env_of(&[("HOME", "/nonexistent"), ("PATH", "/usr/bin:/bin")]);
        let run = run_probes(&ProbeOptions::new(nono.to_str().unwrap(), "/p.json", &env)).unwrap();
        assert!(run.checks.iter().any(|check| check.check == "herdrSocket"));
        let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
        assert!(args.contains("/herdr-nono-probe probe {"), "{args}");
        let workspace = args
            .split("--allow ")
            .nth(1)
            .unwrap()
            .split(' ')
            .next()
            .unwrap();
        assert!(
            !Path::new(workspace).exists(),
            "the workspace is removed afterwards"
        );
    }

    #[test]
    fn socket_probes_report_allowed_absent_refused_and_denied() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.sock");
        let _listener = UnixListener::bind(&live).unwrap();
        assert_eq!(probe_socket(Some(live.to_str().unwrap())), "allowed");
        assert_eq!(probe_socket(None), "absent");
        assert_eq!(
            probe_socket(Some(dir.path().join("missing.sock").to_str().unwrap())),
            "absent"
        );
        let stale = dir.path().join("stale.sock");
        drop(UnixListener::bind(&stale).unwrap());
        assert_eq!(probe_socket(Some(stale.to_str().unwrap())), "refused");
        let plain = dir.path().join("plain");
        std::fs::write(&plain, "").unwrap();
        assert_eq!(
            probe_socket(Some(plain.to_str().unwrap())),
            "refused",
            "connecting to a plain file fails with ECONNREFUSED, as it does for Node"
        );
    }

    #[test]
    fn tcp_probes_report_allowed_and_refused() {
        let canary = start_canary().unwrap();
        assert_eq!(probe_tcp(canary.port), "allowed");
        let port = canary.port;
        canary.close();
        assert_eq!(probe_tcp(port), "refused");
    }

    #[test]
    fn file_directory_and_write_probes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(probe_readable(file.to_str().unwrap()), "allowed");
        assert_eq!(
            probe_readable(dir.path().join("none").to_str().unwrap()),
            "absent"
        );
        assert_eq!(probe_listable(dir.path().to_str().unwrap()), "allowed");
        assert_eq!(
            probe_listable(dir.path().join("none").to_str().unwrap()),
            "absent"
        );
        assert_eq!(probe_writable(dir.path().to_str().unwrap()), "allowed");
        assert_eq!(
            probe_writable(dir.path().join("none").to_str().unwrap()),
            "absent"
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "the probe file is removed again"
        );
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
        // A root-owned test run can write anywhere; only assert the denial for other users.
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(probe_writable(locked.to_str().unwrap()), "denied");
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// A proxy that answers CONNECT with 200 for the hosts in `allowed` and 403 otherwise, recording the requests.
    fn fake_proxy(allowed: &'static [&'static str]) -> (u16, Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 2048];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let target = request.split(' ').nth(1).unwrap_or("").to_string();
                let host = target
                    .rsplit_once(':')
                    .map_or(target.as_str(), |(host, _)| host)
                    .to_string();
                record.lock().unwrap().push(request);
                let status = if allowed.contains(&host.as_str()) {
                    "200 Connection Established"
                } else {
                    "403 Forbidden"
                };
                let _ = stream.write_all(format!("HTTP/1.1 {status}\r\n\r\n").as_bytes());
            }
        });
        (port, seen)
    }

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn the_proxy_probe_lists_every_name_the_proxy_forwarded() {
        let (port, seen) = fake_proxy(&["127.0.0.1", "localhost"]);
        let proxy_url = format!("http://tok:se%2Fcret@127.0.0.1:{port}");
        let lookup = |name: &str| (name == "HTTP_PROXY").then(|| proxy_url.clone());
        let result = probe_via_proxy(
            &names(&["127.0.0.1", "0.0.0.0", "localhost"]),
            9999,
            &lookup,
        );
        assert_eq!(result, "allowed via 127.0.0.1, localhost");
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].starts_with("CONNECT 127.0.0.1:9999 HTTP/1.1\r\nHost: 127.0.0.1:9999\r\nProxy-Authorization: Basic "), "{}", requests[0]);
        assert!(
            requests[0].contains(&base64(b"tok:se/cret")),
            "credentials are percent-decoded before they are encoded"
        );
    }

    #[test]
    fn the_proxy_probe_is_denied_when_nothing_is_forwarded_and_absent_without_a_proxy() {
        let (port, _) = fake_proxy(&[]);
        let proxy_url = format!("http://127.0.0.1:{port}");
        let lookup = |name: &str| (name == "https_proxy").then(|| proxy_url.clone());
        assert_eq!(
            probe_via_proxy(&names(&["127.0.0.1"]), 9999, &lookup),
            "denied"
        );
        assert_eq!(
            probe_via_proxy(&names(&["127.0.0.1"]), 9999, &|_| None),
            "absent"
        );
        assert_eq!(
            probe_via_proxy(&names(&["127.0.0.1"]), 9999, &|name: &str| (name
                == "HTTP_PROXY")
                .then(|| "not a url".to_string())),
            "absent"
        );
        let unreachable =
            |name: &str| (name == "HTTP_PROXY").then(|| "http://127.0.0.1:1".to_string());
        assert_eq!(
            probe_via_proxy(&names(&["127.0.0.1"]), 9999, &unreachable),
            "denied",
            "a proxy that cannot be reached forwards nothing"
        );
    }

    #[test]
    fn base64_and_percent_decoding_match_the_standard_forms() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(percent_decode("a%2Fb%20c%zz%4"), "a/b c%zz%4");
        assert_eq!(http_status("HTTP/1.1 200 OK"), Some(200));
        assert_eq!(http_status("HTTP/1.0 407 x"), Some(407));
        assert_eq!(http_status("HTTP/2 200"), None);
        assert_eq!(http_status("HTTP/1.1 2"), None);
    }

    #[test]
    fn the_whole_probe_reports_every_target_kind_and_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("herdr.sock");
        let _listener = UnixListener::bind(&live).unwrap();
        let canary = start_canary().unwrap();
        let targets = json!({
            "tcp": {"opencodeServicePort": canary.port},
            "sockets": {"herdrSocket": live, "sshAgent": null},
            "files": {"opencodeServicePassword": dir.path().join("service.json")},
            "dirs": {"sshKeys": dir.path()},
            "writes": {"homeDirectory": dir.path()},
            "canary": {"port": canary.port, "names": ["127.0.0.1"]},
        });
        let lookup = |name: &str| (name == "NONO_CAP_FILE").then(|| "/tmp/cap".to_string());
        let names = names(&[
            "PATH",
            "HERDR_SOCKET_PATH",
            "HERDR_PANE_ID",
            "SSH_AUTH_SOCK",
            "SSH_AUTH_SOCKS",
            "MYHERDR_X",
        ]);
        let results = run_probe_checks(&targets, &lookup, &names);
        assert_eq!(results["herdrSocket"], "allowed");
        assert_eq!(results["sshAgent"], "absent");
        assert_eq!(results["opencodeServicePort"], "allowed");
        assert_eq!(results["loopbackCanary"], "allowed");
        assert_eq!(results["loopbackViaProxy"], "absent");
        assert_eq!(results["opencodeServicePassword"], "absent");
        assert_eq!(results["sshKeys"], "allowed");
        assert_eq!(results["homeDirectory"], "allowed");
        assert_eq!(
            results["env"],
            json!(["HERDR_PANE_ID", "HERDR_SOCKET_PATH", "SSH_AUTH_SOCK"])
        );
        assert_eq!(results["marker"], true);
        let no_marker = run_probe_checks(&json!({}), &|_| None, &[]);
        assert_eq!(no_marker, json!({"env": [], "marker": false}));
    }
}

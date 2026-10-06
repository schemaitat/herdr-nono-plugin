//! Detection of OpenCode's host background service (`opencode serve
//! --service`). The service runs outside any sandbox, executes tools on the
//! host, and publishes its URL and password in OpenCode's state and config
//! directories, which the `nolabs-ai/opencode` nono profile grants read-write
//! to the sandbox (Landlock cannot carve a single file out of a granted
//! directory). With unrestricted egress a sandboxed agent can therefore read
//! the password and drive the host service: a way out of the sandbox. The
//! plugin cannot close that hole from inside the profile, so it detects the
//! service and warns, or refuses to launch (`hostServiceCheck`).
//! This module reads the URL, pid and port only; it never reads the password.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::context::Env;
use crate::procfs::{process_alive, read_process, tcp_connections, TCP_LISTEN};
use crate::util::positive_int;

/// Where OpenCode records its background service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceFiles {
    pub state_file: PathBuf,
    pub config_file: PathBuf,
}

/// The home directory: `$HOME` when set and non-empty, else the account's.
pub fn home_dir(env: &Env) -> PathBuf {
    env.get("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(std::env::home_dir)
        .unwrap_or_default()
}

pub fn opencode_service_files(env: &Env) -> ServiceFiles {
    let home = home_dir(env);
    let from_env = |name: &str, fallback: &[&str]| -> PathBuf {
        match env.get(name).filter(|value| !value.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => fallback
                .iter()
                .fold(home.clone(), |path, part| path.join(part)),
        }
    };
    ServiceFiles {
        state_file: from_env("XDG_STATE_HOME", &[".local", "state"])
            .join("opencode")
            .join("service.json"),
        config_file: from_env("XDG_CONFIG_HOME", &[".config"])
            .join("opencode")
            .join("service.json"),
    }
}

fn read_json(file: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()
}

/// The port of a URL: its explicit port, else 443 for https and 80 for anything else.
pub fn port_of(url: &str) -> Option<u16> {
    let parsed = Url::parse(url).ok()?;
    Some(
        parsed
            .port()
            .unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 }),
    )
}

/// The host OpenCode service as far as it can be seen from outside.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostService {
    pub kind: String,
    pub pid: Option<u32>,
    pub url: Option<String>,
    pub port: Option<u16>,
    pub state_file: String,
    pub config_file: String,
    pub running: bool,
    pub listening: Option<bool>,
    pub sandboxed: Option<bool>,
}

/// Detects the host OpenCode service, or returns a record with `running: false`.
pub fn detect_opencode_service(env: &Env, proc_root: &Path) -> HostService {
    let files = opencode_service_files(env);
    let state = read_json(&files.state_file);
    let config = read_json(&files.config_file);
    let url = state
        .as_ref()
        .and_then(|state| state.get("url"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let pid = positive_int(state.as_ref().and_then(|state| state.get("pid")));
    let port = url.as_deref().and_then(port_of).or_else(|| {
        positive_int(config.as_ref().and_then(|config| config.get("port")))
            .and_then(|port| u16::try_from(port).ok())
    });
    let stopped = |pid, url: &Option<String>| HostService {
        kind: "opencode".into(),
        running: false,
        pid,
        url: url.clone(),
        port,
        listening: None,
        sandboxed: None,
        state_file: files.state_file.to_string_lossy().into_owned(),
        config_file: files.config_file.to_string_lossy().into_owned(),
    };
    let Some(pid) = pid.filter(|pid| process_alive(*pid)) else {
        return stopped(pid, &url);
    };
    let info = read_process(pid, proc_root);
    if info.as_ref().is_some_and(|info| {
        !info.argv.is_empty() && !info.argv.iter().any(|word| word.contains("opencode"))
    }) {
        // The recorded pid was recycled by an unrelated process: the service is gone.
        return stopped(Some(pid), &url);
    }
    let sandboxed = info.as_ref().and_then(|info| {
        info.no_new_privs
            .map(|privs| privs && info.nono_cap_file == Some(true))
    });
    let mut listening = None;
    if let Some(port) = port {
        let connections = tcp_connections(proc_root);
        if !connections.is_empty() {
            listening =
                Some(connections.values().any(|connection| {
                    connection.state == TCP_LISTEN && connection.local.port == port
                }));
        }
    }
    HostService {
        running: listening != Some(false),
        listening,
        sandboxed,
        ..stopped(Some(pid), &url)
    }
}

/// A warning about a host service a sandboxed agent could use to leave the
/// sandbox, or `None` when there is nothing to warn about.
pub fn host_service_warning(service: &HostService) -> Option<String> {
    if !service.running || service.sandboxed == Some(true) {
        return None;
    }
    let where_ = service
        .url
        .clone()
        .or_else(|| service.port.map(|port| format!("port {port}")))
        .unwrap_or_else(|| "an unknown port".to_string());
    Some(format!(
        "An OpenCode background service (pid {}, {where_}) is running outside any sandbox. \
The opencode nono profile lets sandboxed processes read {}, which holds its password, \
so an agent could drive that service and run tools on the host. \
Stop it with \"opencode service stop\" while sandboxed agents run, and do not start plain \"opencode\" on the host meanwhile (it restarts the service).",
        service.pid.map_or_else(|| "null".to_string(), |pid| pid.to_string()),
        service.state_file
    ))
}

/// The block the bridge prints in the pane when it refuses to start an agent
/// because of the host service: short lines (panes are narrow), the headline
/// in bold red, and the exact command that fixes it.
pub fn host_service_refusal(service: &HostService, home: &str, color: bool) -> Vec<String> {
    let where_ = service
        .url
        .clone()
        .or_else(|| service.port.map(|port| format!("port {port}")))
        .unwrap_or_else(|| "unknown port".to_string());
    let file = match service.state_file.strip_prefix(&format!("{home}/")) {
        Some(rest) if !home.is_empty() => format!("~/{rest}"),
        _ => service.state_file.clone(),
    };
    let headline = "nono: the agent was NOT started";
    let rule = "=".repeat(56);
    let pid = service
        .pid
        .map_or_else(|| "null".to_string(), |pid| pid.to_string());
    [
        rule.clone(),
        if color {
            format!("\u{1b}[1;31m{headline}\u{1b}[0m")
        } else {
            headline.to_string()
        },
        rule.clone(),
        "An OpenCode background service runs OUTSIDE any sandbox:".to_string(),
        format!("  pid {pid}, {where_}"),
        "A sandboxed agent can read its password from".to_string(),
        format!("  {file}"),
        "and use it to run commands on the host, unsandboxed.".to_string(),
        String::new(),
        "Stop the service, then reconnect (prefix, shift+b):".to_string(),
        "  opencode service stop".to_string(),
        "Plain \"opencode\" on the host starts it again.".to_string(),
        String::new(),
        "To start agents anyway, set \"hostServiceCheck\": \"warn\"".to_string(),
        "in the plugin's config.json (not recommended).".to_string(),
        rule,
    ]
    .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procfs::fake::{write_fake_proc, FakeProcess};
    use serde_json::json;

    fn tcp_line(local: &str, remote: &str, state: &str, inode: &str) -> String {
        format!("   0: {local} {remote} {state} 00000000:00000000 00:00000000 00000000  1000        0 {inode} 1 0000000000000000 20 4 30 10 -1")
    }

    fn home_env(home: &Path) -> Env {
        Env::from([("HOME".to_string(), home.to_string_lossy().into_owned())])
    }

    fn write_state(home: &Path, value: Value) {
        let dir = home.join(".local/state/opencode");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("service.json"), value.to_string()).unwrap();
    }

    #[test]
    fn service_files_follow_the_xdg_variables_then_the_home_directory() {
        let plain =
            opencode_service_files(&Env::from([("HOME".to_string(), "/home/u".to_string())]));
        assert_eq!(
            plain.state_file,
            Path::new("/home/u/.local/state/opencode/service.json")
        );
        assert_eq!(
            plain.config_file,
            Path::new("/home/u/.config/opencode/service.json")
        );
        let xdg = opencode_service_files(&Env::from([
            ("XDG_STATE_HOME".into(), "/s".into()),
            ("XDG_CONFIG_HOME".into(), "/c".into()),
            ("HOME".into(), "/home/u".into()),
        ]));
        assert_eq!(
            (xdg.state_file, xdg.config_file),
            (
                PathBuf::from("/s/opencode/service.json"),
                PathBuf::from("/c/opencode/service.json")
            )
        );
    }

    #[test]
    fn port_of_reads_explicit_and_default_ports() {
        assert_eq!(port_of("http://127.0.0.1:4096"), Some(4096));
        assert_eq!(port_of("https://example.com"), Some(443));
        assert_eq!(port_of("http://example.com"), Some(80));
        assert_eq!(port_of("http://example.com:80/x"), Some(80));
        assert_eq!(port_of("not a url"), None);
    }

    #[test]
    fn detect_opencode_service_reads_the_service_file_and_checks_the_process_and_listener() {
        let home = tempfile::tempdir().unwrap();
        let env = home_env(home.path());
        let proc = tempfile::tempdir().unwrap();
        write_fake_proc(
            proc.path(),
            &[],
            &[&tcp_line("0100007F:1000", "00000000:0000", "0A", "8888")],
        );
        assert!(
            !detect_opencode_service(&env, proc.path()).running,
            "no service file, no service"
        );
        // The fake /proc has no entry for this pid, so only liveness (the real process) and the listener are known.
        write_state(
            home.path(),
            json!({"url": "http://127.0.0.1:4096", "pid": std::process::id(), "password": "secret"}),
        );
        let service = detect_opencode_service(&env, proc.path());
        assert!(service.running);
        assert_eq!(service.port, Some(4096));
        assert_eq!(service.listening, Some(true));
        assert_eq!(service.sandboxed, None);
        assert!(
            !serde_json::to_string(&service).unwrap().contains("secret"),
            "the password is never read into the result"
        );
        assert!(host_service_warning(&service)
            .unwrap()
            .contains("opencode service stop"));
        assert_eq!(
            host_service_warning(&HostService {
                sandboxed: Some(true),
                ..service.clone()
            }),
            None,
            "a service inside a sandbox is no way out"
        );
        write_state(
            home.path(),
            json!({"url": "http://127.0.0.1:4097", "pid": std::process::id()}),
        );
        assert!(
            !detect_opencode_service(&env, proc.path()).running,
            "a live pid without a listener on the port is a stale file"
        );
    }

    #[test]
    fn a_recycled_pid_and_a_dead_pid_are_not_a_running_service() {
        let home = tempfile::tempdir().unwrap();
        let env = home_env(home.path());
        let proc = tempfile::tempdir().unwrap();
        write_fake_proc(
            proc.path(),
            &[FakeProcess {
                pid: std::process::id(),
                ppid: 1,
                argv: vec!["/usr/bin/unrelated"],
                ..Default::default()
            }],
            &[],
        );
        write_state(
            home.path(),
            json!({"url": "http://127.0.0.1:4096", "pid": std::process::id()}),
        );
        assert!(
            !detect_opencode_service(&env, proc.path()).running,
            "the pid now belongs to an unrelated process"
        );
        write_state(
            home.path(),
            json!({"url": "http://127.0.0.1:4096", "pid": 2_147_483_647}),
        );
        let dead = detect_opencode_service(&env, proc.path());
        assert!(!dead.running);
        assert_eq!(dead.pid, Some(2_147_483_647));
    }

    #[test]
    fn a_service_whose_process_is_a_confined_opencode_counts_as_sandboxed() {
        let home = tempfile::tempdir().unwrap();
        let env = home_env(home.path());
        let proc = tempfile::tempdir().unwrap();
        write_fake_proc(
            proc.path(),
            &[FakeProcess {
                pid: std::process::id(),
                ppid: 1,
                argv: vec!["/x/opencode", "serve", "--service"],
                ..Default::default()
            }],
            &[&tcp_line("0100007F:1000", "00000000:0000", "0A", "8888")],
        );
        write_state(
            home.path(),
            json!({"url": "http://127.0.0.1:4096", "pid": std::process::id()}),
        );
        let service = detect_opencode_service(&env, proc.path());
        assert_eq!(service.sandboxed, Some(true));
        assert_eq!(host_service_warning(&service), None);
    }

    #[test]
    fn the_refusal_block_names_the_service_the_file_and_the_fix() {
        let service = HostService {
            kind: "opencode".into(),
            running: true,
            pid: Some(77),
            url: Some("http://127.0.0.1:4096".into()),
            port: Some(4096),
            listening: Some(true),
            sandboxed: Some(false),
            state_file: "/home/u/.local/state/opencode/service.json".into(),
            config_file: "/c".into(),
        };
        let plain = host_service_refusal(&service, "/home/u", false);
        assert_eq!(plain.len(), 16);
        assert_eq!(plain[1], "nono: the agent was NOT started");
        assert_eq!(plain[4], "  pid 77, http://127.0.0.1:4096");
        assert_eq!(plain[6], "  ~/.local/state/opencode/service.json");
        assert!(plain.contains(&"  opencode service stop".to_string()));
        assert_eq!(
            host_service_refusal(&service, "/home/u", true)[1],
            "\u{1b}[1;31mnono: the agent was NOT started\u{1b}[0m"
        );
        assert_eq!(
            host_service_refusal(&service, "/elsewhere", false)[6],
            "  /home/u/.local/state/opencode/service.json"
        );
    }
}

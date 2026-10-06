//! Proves, from the host's side, that a launched agent runs confined: every
//! process under the nono supervisors (the client's and, for agents with a
//! server sandbox, the server's) carries `no_new_privs` and nono's
//! `NONO_CAP_FILE` marker, the agent's own server is one of those processes,
//! the client was started with the arguments that point it at that server, and
//! no sandboxed process holds a connection to an unsandboxed host service.
//! Linux only: it reads /proc.

use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::agents::{compile_server_pattern, with_port, with_port_all, Adapter};
use crate::hostservice::{host_service_warning, HostService};
use crate::procfs::{
    descendants, procfs_available, read_process, socket_inodes, tcp_connections, ProcessInfo,
    TCP_ESTABLISHED,
};
use crate::util::iso_now;

const LOOPBACK: [&str; 4] = ["127.0.0.1", "::1", "0.0.0.0", "::"];

/// The last path component, as `path.basename` reads it.
fn basename(word: &str) -> &str {
    word.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}

fn command_text(argv: &[String]) -> String {
    let text = argv.join(" ");
    if text.chars().count() > 200 {
        format!("{}...", text.chars().take(197).collect::<String>())
    } else {
        text
    }
}

/// Whether a process shows the marks of a nono-confined process. A missing
/// `NONO_CAP_FILE` only counts against it when the environment was readable.
pub fn looks_confined(info: &ProcessInfo) -> bool {
    info.no_new_privs == Some(true) && info.nono_cap_file != Some(false)
}

/// Finds the agent's client process in the sandboxed tree: the first process
/// whose executable, or script for interpreters, is the adapter's command.
pub fn find_client<'a>(tree: &'a [ProcessInfo], command: &str) -> Option<&'a ProcessInfo> {
    let wanted = basename(command);
    tree.iter().find(|info| {
        info.argv
            .iter()
            .take(2)
            .any(|word| basename(word) == wanted)
    })
}

/// One process of a verified launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedProcess {
    pub pid: u32,
    pub ppid: Option<u32>,
    /// `client` or `server`.
    pub sandbox: String,
    /// `client`, `server`, `tool` or `child`.
    pub role: String,
    pub confined: bool,
    pub no_new_privs: Option<bool>,
    pub nono_cap_file: Option<bool>,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientReport {
    pub pid: u32,
    pub command: String,
    pub required_args: Vec<String>,
    pub missing_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerReport {
    pub pattern: String,
    pub pid: Option<u32>,
    pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostConnection {
    pub pid: u32,
    pub remote: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostServiceReport {
    pub running: bool,
    pub pid: Option<u32>,
    pub url: Option<String>,
    pub sandboxed: Option<bool>,
    pub reachable: bool,
}

/// The outcome of verifying one running launch; serialises to the JSON the
/// mapping stores as `verification`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationReport {
    pub checked_at: String,
    pub supported: bool,
    pub ok: Option<bool>,
    pub supervisor_pid: Option<u32>,
    pub server_supervisor_pid: Option<u32>,
    pub port: Option<u16>,
    pub running: bool,
    pub processes: Vec<VerifiedProcess>,
    pub client: Option<ClientReport>,
    pub server: Option<ServerReport>,
    pub host_connections: Vec<HostConnection>,
    pub host_service: Option<HostServiceReport>,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
}

/// What to verify.
pub struct VerifyInput<'a> {
    pub supervisor_pid: Option<u32>,
    pub server_supervisor_pid: Option<u32>,
    pub port: Option<u16>,
    pub agent: &'a Adapter,
    pub host_service: Option<&'a HostService>,
    /// Whether the tools' sandbox may connect to localhost directly; only then is a running host service a warning.
    pub host_service_reachable: bool,
    pub proc_root: &'a Path,
}

/// Verifies one running launch.
pub fn verify_session(input: &VerifyInput) -> VerificationReport {
    let VerifyInput {
        supervisor_pid,
        server_supervisor_pid,
        port,
        agent,
        host_service,
        host_service_reachable,
        proc_root,
    } = *input;
    let mut report = VerificationReport {
        checked_at: iso_now(),
        supported: procfs_available(proc_root),
        ok: None,
        supervisor_pid,
        server_supervisor_pid,
        port,
        running: false,
        processes: Vec::new(),
        client: None,
        server: None,
        host_connections: Vec::new(),
        host_service: host_service.map(|service| HostServiceReport {
            running: service.running,
            pid: service.pid,
            url: service.url.clone(),
            sandboxed: service.sandboxed,
            reachable: service.running && host_service_reachable,
        }),
        problems: Vec::new(),
        warnings: Vec::new(),
    };
    if let Some(warning) = host_service
        .filter(|_| host_service_reachable)
        .and_then(host_service_warning)
    {
        report.warnings.push(warning);
    }
    if !report.supported {
        report.warnings.push("This system has no /proc, so the process tree cannot be inspected; verification only runs on Linux.".into());
        return report;
    }
    let alive = |pid: Option<u32>| pid.is_some_and(|pid| read_process(pid, proc_root).is_some());
    if !alive(supervisor_pid) {
        report
            .problems
            .push("The nono session is not running, so there is nothing to verify.".into());
        report.ok = Some(false);
        return report;
    }
    report.running = true;
    let supervisor = supervisor_pid.expect("alive implies a pid");
    let client_tree = descendants(supervisor, proc_root);
    if client_tree.is_empty() {
        report.problems.push(format!(
            "The nono supervisor (pid {supervisor}) has not started the sandboxed process yet."
        ));
    }
    let mut server_tree = Vec::new();
    if agent.server.is_some() {
        match server_supervisor_pid.filter(|_| alive(server_supervisor_pid)) {
            None => report
                .problems
                .push("The agent's server sandbox is not running.".into()),
            Some(server_supervisor) => {
                server_tree = descendants(server_supervisor, proc_root);
                if server_tree.is_empty() {
                    report.problems.push(format!("The server's nono supervisor (pid {server_supervisor}) has not started the server yet."));
                }
            }
        }
    }
    let server_regex = match agent.server_pattern.as_deref() {
        None => None,
        Some(pattern) => match compile_server_pattern(pattern, port) {
            Ok(regex) => Some(regex),
            Err(error) => {
                report.problems.push(format!(
                    "The agent's serverPattern is not a valid regular expression ({error})."
                ));
                None
            }
        },
    };
    let client = find_client(
        &client_tree,
        agent.command.first().map_or("", String::as_str),
    );
    let server_home = if agent.server.is_some() {
        &server_tree
    } else {
        &client_tree
    };
    let server = server_regex.as_ref().and_then(|regex| {
        server_home
            .iter()
            .find(|info| regex.is_match(&info.argv.join(" ")))
    });
    for (sandbox, tree) in [("client", &client_tree), ("server", &server_tree)] {
        for info in tree {
            let confined = looks_confined(info);
            let role = if server.is_some_and(|found| found.pid == info.pid) {
                "server"
            } else if client.is_some_and(|found| found.pid == info.pid) {
                "client"
            } else if sandbox == "server" {
                "tool"
            } else {
                "child"
            };
            report.processes.push(VerifiedProcess {
                pid: info.pid,
                ppid: info.ppid,
                sandbox: sandbox.into(),
                role: role.into(),
                confined,
                no_new_privs: info.no_new_privs,
                nono_cap_file: info.nono_cap_file,
                command: command_text(&info.argv),
            });
            if !confined {
                let show = |flag: Option<bool>| {
                    flag.map_or_else(|| "null".to_string(), |flag| flag.to_string())
                };
                report.problems.push(format!(
                    "Process {} ({}) under the {sandbox} sandbox's supervisor is not confined (no_new_privs {}, NONO_CAP_FILE {}).",
                    info.pid,
                    command_text(&info.argv),
                    show(info.no_new_privs),
                    show(info.nono_cap_file)
                ));
            }
        }
    }
    if !client_tree.is_empty() {
        let required = with_port_all(&agent.required_args, port);
        let command_name = agent.command.first().map_or("", String::as_str);
        match client {
            None => report.problems.push(format!(
                "No {} process runs inside the sandbox.",
                basename(command_name)
            )),
            Some(client) => {
                let missing_args: Vec<String> = required
                    .iter()
                    .filter(|arg| !client.argv.contains(arg))
                    .cloned()
                    .collect();
                if !missing_args.is_empty() {
                    report.problems.push(format!(
                        "The {} client (pid {}) was started without {}, so its server may run outside the sandbox.",
                        agent.title,
                        client.pid,
                        missing_args.join(" ")
                    ));
                }
                report.client = Some(ClientReport {
                    pid: client.pid,
                    command: command_text(&client.argv),
                    required_args: required,
                    missing_args,
                });
            }
        }
    }
    if let Some(pattern) = agent.server_pattern.as_deref().filter(|_| {
        server_regex.is_some() || report.problems.iter().all(|p| !p.contains("serverPattern"))
    }) {
        let pattern = with_port(pattern, port);
        report.server = Some(ServerReport {
            pattern: pattern.clone(),
            pid: server.map(|found| found.pid),
            command: server.map(|found| command_text(&found.argv)),
        });
        if server.is_none() {
            let place = if agent.server.is_some() {
                "server "
            } else {
                ""
            };
            report.problems.push(format!("No process matching /{pattern}/ (the agent's own server) runs inside the {place}sandbox."));
        }
    }
    if let Some(service) = host_service.filter(|service| service.running && service.port.is_some())
    {
        let service_port = service.port.expect("filtered");
        let connections = tcp_connections(proc_root);
        for info in client_tree.iter().chain(&server_tree) {
            for inode in socket_inodes(info.pid, proc_root).unwrap_or_default() {
                let Some(connection) = connections.get(&inode) else {
                    continue;
                };
                if connection.state == TCP_ESTABLISHED
                    && connection.remote.port == service_port
                    && LOOPBACK.contains(&connection.remote.address.as_str())
                {
                    let remote =
                        format!("{}:{}", connection.remote.address, connection.remote.port);
                    report.problems.push(format!(
                        "Sandboxed process {} ({}) is connected to the host service at {remote}.",
                        info.pid,
                        command_text(&info.argv)
                    ));
                    report.host_connections.push(HostConnection {
                        pid: info.pid,
                        remote,
                    });
                }
            }
        }
    }
    report.ok = Some(report.problems.is_empty());
    report
}

/// One-line summary of a stored verification report for panes, toasts and
/// tables. Reads the report as JSON, so reports written by any version work.
pub fn summarize_verification(report: Option<&Value>) -> String {
    let Some(report) = report.filter(|report| !report.is_null()) else {
        return "not verified".to_string();
    };
    if report.get("supported").and_then(Value::as_bool) != Some(true) {
        return "unsupported (no /proc)".to_string();
    }
    if report.get("ok").and_then(Value::as_bool) == Some(true) {
        let count = report
            .get("processes")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let server = report
            .get("server")
            .and_then(|server| server.get("pid"))
            .and_then(Value::as_u64)
            .map_or_else(String::new, |pid| format!(", server pid {pid}"));
        return format!(
            "confined: {count} process{}{server}",
            if count == 1 { "" } else { "es" }
        );
    }
    let first = report
        .get("problems")
        .and_then(Value::as_array)
        .and_then(|problems| problems.first())
        .and_then(Value::as_str)
        .unwrap_or("unknown problem");
    format!("FAILED: {first}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{builtin_agents, with_port};
    use crate::procfs::fake::{write_fake_proc, FakeProcess};
    use serde_json::json;

    const PORT: u16 = 4242;

    fn opencode() -> Adapter {
        builtin_agents().remove(0).1
    }

    static OPENCODE: std::sync::LazyLock<Adapter> = std::sync::LazyLock::new(opencode);

    fn tcp_line(local: &str, remote: &str, state: &str, inode: &str) -> String {
        format!("   0: {local} {remote} {state} 00000000:00000000 00:00000000 00000000  1000        0 {inode} 1 0000000000000000 20 4 30 10 -1")
    }

    fn proc(pid: u32, ppid: u32, argv: Vec<&'static str>) -> FakeProcess {
        FakeProcess {
            pid,
            ppid,
            argv,
            ..Default::default()
        }
    }

    fn unconfined(pid: u32, ppid: u32, argv: Vec<&'static str>) -> FakeProcess {
        FakeProcess {
            no_new_privs: Some(false),
            cap_file: Some(false),
            ..proc(pid, ppid, argv)
        }
    }

    /// A client sandbox (100 -> 101) and a server sandbox (110 -> 111 -> 112), plus the host service (200).
    fn proc_tree(extra: Vec<FakeProcess>, tcp: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let server_port: &'static str = Box::leak(PORT.to_string().into_boxed_str());
        let client_url: &'static str =
            Box::leak(format!("http://127.0.0.1:{PORT}").into_boxed_str());
        let mut processes = vec![
            unconfined(
                100,
                1,
                vec![
                    "nono",
                    "run",
                    "--profile",
                    "client",
                    "--",
                    "opencode",
                    "--server",
                    client_url,
                ],
            ),
            proc(
                101,
                100,
                vec!["/home/u/.opencode/bin/opencode", "--server", client_url],
            ),
            unconfined(
                110,
                1,
                vec![
                    "nono",
                    "run",
                    "--profile",
                    "server",
                    "--",
                    "opencode",
                    "serve",
                ],
            ),
            proc(
                111,
                110,
                vec![
                    "/home/u/.opencode/bin/opencode",
                    "serve",
                    "--hostname",
                    "127.0.0.1",
                    "--port",
                    server_port,
                ],
            ),
            proc(112, 111, vec!["/usr/bin/bash", "-c", "echo hi"]),
            unconfined(
                200,
                1,
                vec!["/home/u/.opencode/bin/opencode", "serve", "--service"],
            ),
        ];
        processes.extend(extra);
        write_fake_proc(root.path(), &processes, tcp);
        root
    }

    fn verify_tree<'a>(
        root: &'a Path,
        adjust: impl FnOnce(&mut VerifyInput<'a>),
    ) -> VerificationReport {
        let mut input = VerifyInput {
            supervisor_pid: Some(100),
            server_supervisor_pid: Some(110),
            port: Some(PORT),
            agent: &OPENCODE,
            host_service: None,
            host_service_reachable: true,
            proc_root: root,
        };
        adjust(&mut input);
        verify_session(&input)
    }

    fn host_service() -> HostService {
        HostService {
            kind: "opencode".into(),
            running: true,
            pid: Some(200),
            url: Some("http://127.0.0.1:4096".into()),
            port: Some(4096),
            listening: Some(true),
            sandboxed: Some(false),
            state_file: "/s".into(),
            config_file: "/c".into(),
        }
    }

    #[test]
    fn a_client_and_its_private_server_each_in_their_own_sandbox_with_the_tools_under_the_server_verify(
    ) {
        let root = proc_tree(vec![], &[]);
        let report = verify_tree(root.path(), |_| {});
        assert_eq!(report.ok, Some(true), "{:?}", report.problems);
        let shape: Vec<(u32, &str, &str)> = report
            .processes
            .iter()
            .map(|item| (item.pid, item.sandbox.as_str(), item.role.as_str()))
            .collect();
        assert_eq!(
            shape,
            [
                (101, "client", "client"),
                (111, "server", "server"),
                (112, "server", "tool")
            ]
        );
        assert_eq!(report.server.as_ref().unwrap().pid, Some(111));
        let client = report.client.as_ref().unwrap();
        assert_eq!(client.required_args, ["--server", "http://127.0.0.1:4242"]);
        assert!(client.missing_args.is_empty());
        let stored = serde_json::to_value(&report).unwrap();
        assert_eq!(
            summarize_verification(Some(&stored)),
            "confined: 3 processes, server pid 111"
        );
        assert_eq!(stored["checkedAt"].as_str().unwrap().len(), 24);
    }

    #[test]
    fn a_client_pointed_elsewhere_or_a_missing_server_sandbox_fails_the_verification() {
        let root = tempfile::tempdir().unwrap();
        write_fake_proc(
            root.path(),
            &[
                FakeProcess {
                    no_new_privs: Some(false),
                    ..proc(100, 1, vec!["nono"])
                },
                proc(
                    101,
                    100,
                    vec![
                        "/home/u/.opencode/bin/opencode",
                        "--server",
                        "http://127.0.0.1:4096",
                    ],
                ),
                unconfined(
                    200,
                    1,
                    vec!["/home/u/.opencode/bin/opencode", "serve", "--service"],
                ),
            ],
            &[],
        );
        let report = verify_tree(root.path(), |input| input.server_supervisor_pid = None);
        assert_eq!(report.ok, Some(false));
        let problems = report.problems.join("\n");
        assert!(
            problems.contains("started without http://127.0.0.1:4242"),
            "{problems}"
        );
        assert!(
            problems.contains("server sandbox is not running"),
            "{problems}"
        );
        assert!(
            problems.contains("agent's own server"),
            "the host service never counts as the server: {problems}"
        );
        assert!(
            summarize_verification(Some(&serde_json::to_value(&report).unwrap()))
                .starts_with("FAILED: ")
        );
        let tree = proc_tree(vec![], &[]);
        let wrong_port = verify_tree(tree.path(), |input| input.port = Some(4243));
        assert_eq!(
            wrong_port.ok,
            Some(false),
            "a server on another port is not this launch's server"
        );
    }

    #[test]
    fn an_unconfined_process_in_either_sandbox_fails_the_verification() {
        let tree = proc_tree(
            vec![FakeProcess {
                no_new_privs: Some(false),
                ..proc(113, 111, vec!["/usr/bin/escape"])
            }],
            &[],
        );
        let report = verify_tree(tree.path(), |_| {});
        assert_eq!(report.ok, Some(false));
        assert!(
            report.problems.join("\n").contains("Process 113 (/usr/bin/escape) under the server sandbox's supervisor is not confined"),
            "{:?}",
            report.problems
        );
        let no_marker = proc_tree(
            vec![FakeProcess {
                cap_file: Some(false),
                ..proc(105, 101, vec!["x"])
            }],
            &[],
        );
        assert_eq!(
            verify_tree(no_marker.path(), |_| {}).ok,
            Some(false),
            "a readable environment without NONO_CAP_FILE counts against the process"
        );
        let unreadable = proc_tree(
            vec![FakeProcess {
                environ: Some(false),
                ..proc(105, 101, vec!["x"])
            }],
            &[],
        );
        assert_eq!(
            verify_tree(unreadable.path(), |_| {}).ok,
            Some(true),
            "an unreadable environment is judged by no_new_privs alone"
        );
    }

    #[test]
    fn a_sandboxed_process_connected_to_the_host_service_port_fails_the_verification() {
        let connected = proc_tree(
            vec![FakeProcess {
                sockets: vec!["7777"],
                ..proc(113, 111, vec!["curl", "http://127.0.0.1:4096/api/info"])
            }],
            &[&tcp_line("0100007F:A000", "0100007F:1000", "01", "7777")],
        );
        let service = host_service();
        let report = verify_tree(connected.path(), |input| {
            input.host_service = Some(&service)
        });
        assert_eq!(report.ok, Some(false));
        assert_eq!(
            report.host_connections,
            [HostConnection {
                pid: 113,
                remote: "127.0.0.1:4096".into()
            }]
        );
        assert_eq!(
            report.warnings.len(),
            1,
            "a host service the tools can reach is also a warning"
        );
        let clean = proc_tree(vec![], &[]);
        let unreachable = verify_tree(clean.path(), |input| {
            input.host_service = Some(&service);
            input.host_service_reachable = false;
        });
        assert_eq!(unreachable.ok, Some(true));
        assert!(
            unreachable.warnings.is_empty(),
            "behind the proxy the running service is no warning"
        );
        assert!(!unreachable.host_service.as_ref().unwrap().reachable);
    }

    #[test]
    fn a_session_that_is_not_running_or_not_linux_is_reported_as_such() {
        let root = proc_tree(vec![], &[]);
        assert_eq!(
            verify_tree(root.path(), |input| input.supervisor_pid = Some(4242)).ok,
            Some(false)
        );
        let none = verify_tree(&root.path().join("absent"), |_| {});
        assert!(!none.supported);
        assert_eq!(none.ok, None);
        assert!(none
            .warnings
            .iter()
            .any(|warning| warning.contains("no /proc")));
        assert!(
            summarize_verification(Some(&serde_json::to_value(&none).unwrap()))
                .contains("unsupported")
        );
        assert_eq!(summarize_verification(None), "not verified");
        assert_eq!(summarize_verification(Some(&Value::Null)), "not verified");
    }

    #[test]
    fn the_report_serialises_to_the_json_shape_the_js_stored() {
        let root = proc_tree(vec![], &[]);
        let value = serde_json::to_value(verify_tree(root.path(), |_| {})).unwrap();
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "checkedAt",
                "supported",
                "ok",
                "supervisorPid",
                "serverSupervisorPid",
                "port",
                "running",
                "processes",
                "client",
                "server",
                "hostConnections",
                "hostService",
                "problems",
                "warnings"
            ]
        );
        assert_eq!(
            value["processes"][0],
            json!({"pid": 101, "ppid": 100, "sandbox": "client", "role": "client", "confined": true, "noNewPrivs": true, "nonoCapFile": true, "command": "/home/u/.opencode/bin/opencode --server http://127.0.0.1:4242"})
        );
        assert_eq!(value["hostService"], Value::Null);
    }

    #[test]
    fn summarize_verification_reads_reports_written_by_any_version() {
        assert_eq!(
            summarize_verification(Some(
                &json!({"supported": true, "ok": true, "processes": [{}]})
            )),
            "confined: 1 process"
        );
        assert_eq!(
            summarize_verification(Some(
                &json!({"supported": true, "ok": false, "problems": []})
            )),
            "FAILED: unknown problem"
        );
        assert_eq!(
            summarize_verification(Some(&json!({"ok": true}))),
            "unsupported (no /proc)"
        );
    }

    #[test]
    fn find_client_matches_the_executable_or_the_script_an_interpreter_runs() {
        let info = |argv: &[&str]| ProcessInfo {
            pid: 1,
            ppid: None,
            argv: argv.iter().map(|word| word.to_string()).collect(),
            no_new_privs: None,
            seccomp: None,
            nono_cap_file: None,
        };
        assert_eq!(
            find_client(&[info(&["/usr/bin/node", "/x/bin/opencode"])], "opencode")
                .map(|found| found.pid),
            Some(1)
        );
        assert!(find_client(&[info(&["/usr/bin/node", "/x/other"])], "opencode").is_none());
        assert!(
            find_client(&[info(&["/usr/bin/node", "x", "/x/opencode"])], "opencode").is_none(),
            "only the first two words count"
        );
    }

    #[test]
    fn a_custom_agent_without_a_server_sandbox_looks_for_its_server_in_the_client_tree() {
        let mut agent = opencode();
        agent.server = None;
        agent.command = vec!["agent".into()];
        agent.required_args = Vec::new();
        agent.server_pattern = Some("srv {port}".into());
        let root = tempfile::tempdir().unwrap();
        write_fake_proc(
            root.path(),
            &[
                unconfined(100, 1, vec!["nono"]),
                proc(101, 100, vec!["/bin/agent"]),
                proc(102, 101, vec!["srv", "4242"]),
            ],
            &[],
        );
        let input = VerifyInput {
            supervisor_pid: Some(100),
            server_supervisor_pid: None,
            port: Some(PORT),
            agent: &agent,
            host_service: None,
            host_service_reachable: true,
            proc_root: root.path(),
        };
        let report = verify_session(&input);
        assert_eq!(report.ok, Some(true), "{:?}", report.problems);
        assert_eq!(report.server.unwrap().pid, Some(102));
        assert_eq!(
            with_port(&agent.server_pattern.unwrap(), Some(PORT)),
            "srv 4242"
        );
    }
}

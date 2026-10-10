//! The overlay's data: one row per mapping merged from the state files, the
//! live nono sessions and Herdr's panes, plus the pure helpers that describe
//! them (fitting text, short paths, profile sources and network summaries).
//! Nothing here touches the terminal.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::context::Env;
use crate::errors::Result;
use crate::herdr::HerdrClient;
use crate::hostservice::home_dir;
use crate::naming::{server_session_name, shell_session_name};
use crate::nono::{summarize_profile, NonoClient, ProfileSummary};
use crate::procfs::{descendants, procfs_available};
use crate::state::{load_state, Entry};
use crate::verify::{looks_confined, summarize_verification};

/// One process of a sandbox, with its depth below the supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeProcess {
    pub pid: u32,
    pub depth: usize,
    pub confined: bool,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trees {
    pub client: Vec<TreeProcess>,
    pub server: Vec<TreeProcess>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Networks {
    pub client: Option<ProfileSummary>,
    pub server: Option<ProfileSummary>,
}

/// One mapping with what is live around it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub pane_id: String,
    pub pane_exists: Option<bool>,
    pub session_name: String,
    pub agent_kind: String,
    pub lifecycle_state: String,
    pub running: Option<bool>,
    pub server_running: Option<bool>,
    pub shells: Option<usize>,
    pub verified: Option<bool>,
    pub verification: Option<String>,
    pub local_path: String,
    pub trees: Trees,
    pub network: Networks,
    pub entry: Entry,
}

/// A profile reference of the next launch with what nono says about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfiguredProfile {
    pub reference: Option<String>,
    pub summary: Option<ProfileSummary>,
}

/// The profiles the next launch uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Configured {
    pub client: ConfiguredProfile,
    pub server: ConfiguredProfile,
}

/// One frame's worth of data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Collected {
    pub rows: Vec<Row>,
    pub session_error: Option<String>,
    pub pane_ids: Option<Vec<String>>,
    pub configured: Option<Configured>,
}

/// Cuts or pads plain text to exactly `width` characters; long text ends in `…`
/// (or starts with it, for paths, with `from_left`).
pub fn fit(text: &str, width: usize, from_left: bool) -> String {
    if width == 0 {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > width {
        return if width == 1 {
            "…".to_string()
        } else if from_left {
            format!(
                "…{}",
                chars[chars.len() - (width - 1)..]
                    .iter()
                    .collect::<String>()
            )
        } else {
            format!("{}…", chars[..width - 1].iter().collect::<String>())
        };
    }
    format!("{text}{}", " ".repeat(width - chars.len()))
}

/// A path with the home directory shortened to `~`.
pub fn short_path(text: &str, home: &str) -> String {
    if !home.is_empty() && (text == home || text.starts_with(&format!("{home}/"))) {
        format!("~{}", &text[home.len()..])
    } else {
        text.to_string()
    }
}

/// How the NONO column describes a row's live sessions: the text and whether it is green (running), yellow or gray.
pub fn session_cell(row: &Row) -> (String, Tone) {
    let Some(running) = row.running else {
        return ("unknown".into(), Tone::Gray);
    };
    let mut parts = Vec::new();
    if running {
        parts.push("client".to_string());
    }
    if row.server_running == Some(true) {
        parts.push("server".to_string());
    }
    if let Some(shells) = row.shells.filter(|shells| *shells > 0) {
        parts.push(format!("{shells} sh"));
    }
    if parts.is_empty() {
        return ("-".into(), Tone::Gray);
    }
    (
        parts.join("+"),
        if running { Tone::Green } else { Tone::Yellow },
    )
}

/// A colour intent; the UI maps it to terminal colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Gray,
    Green,
    Yellow,
    Red,
    Cyan,
    Magenta,
    Blue,
}

/// Whether prune would drop a row: its pane is gone and nothing of it runs.
pub fn is_stale(row: &Row) -> bool {
    row.pane_exists == Some(false)
        && row.running == Some(false)
        && row.server_running != Some(true)
        && row.shells.unwrap_or(0) == 0
}

/// How alive an agent is, for the colour of its box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// A sandbox of the agent is up (green).
    Running,
    /// Every sandbox is down but the pane still exists (yellow).
    Stopped,
    /// Every sandbox is down and the pane is gone (red); prune forgets it.
    Stale,
    /// nono could not be asked.
    Unknown,
}

impl Health {
    /// The word the overview shows.
    pub fn word(self) -> &'static str {
        match self {
            Health::Running => "running",
            Health::Stopped => "stopped",
            Health::Stale => "stale",
            Health::Unknown => "unknown",
        }
    }

    /// The colour of the word and of the box border.
    pub fn tone(self) -> Tone {
        match self {
            Health::Running => Tone::Green,
            Health::Stopped => Tone::Yellow,
            Health::Stale => Tone::Red,
            Health::Unknown => Tone::Gray,
        }
    }
}

/// Classifies a row: any live sandbox is running; otherwise the pane decides
/// between stopped (still there) and stale (gone).
pub fn health(row: &Row) -> Health {
    if row.running == Some(true) || row.server_running == Some(true) || row.shells.unwrap_or(0) > 0
    {
        Health::Running
    } else if is_stale(row) {
        Health::Stale
    } else if row.running == Some(false) {
        Health::Stopped
    } else {
        Health::Unknown
    }
}

/// The worktree (or project directory) name of a workspace root, the label the
/// Herdr sidebar shows, and the repository directory above it when the root
/// lives in a `worktrees/<repo>/<name>` layout.
pub fn worktree_name(local_path: &str) -> (String, Option<String>) {
    let path = Path::new(local_path.trim_end_matches('/'));
    let name = path.file_name().map_or_else(
        || "?".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let repo = path.parent().and_then(|repo| {
        let in_worktrees = repo
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|dir| dir == "worktrees");
        in_worktrees
            .then(|| {
                repo.file_name()
                    .map(|repo| repo.to_string_lossy().into_owned())
            })
            .flatten()
    });
    (name, repo)
}

fn basename(word: &str) -> &str {
    word.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}

/// The short form of a process command line for the sandbox boxes: the
/// executable's basename and its arguments. Interpreters show their script.
pub fn short_command(argv: &[String]) -> String {
    let Some(first) = argv.first() else {
        return "?".into();
    };
    let interpreter = matches!(basename(first), "node" | "python" | "python3" | "bun");
    let words: &[String] = if interpreter && argv.get(1).is_some_and(|word| !word.starts_with('-'))
    {
        &argv[1..]
    } else {
        argv
    };
    let mut shown = vec![basename(&words[0]).to_string()];
    shown.extend(words[1..].iter().cloned());
    shown.join(" ")
}

/// A profile name for display: the file name without `.json`.
pub fn profile_name(reference: &str) -> String {
    let name = basename(reference);
    name.strip_suffix(".json").unwrap_or(name).to_string()
}

/// What a profile summary says about a sandbox's network, in a few words.
pub fn network_label(
    summary: Option<&ProfileSummary>,
    client_side: bool,
    port: Option<u64>,
) -> String {
    let port_text = port.map_or_else(|| "its port".to_string(), |port| format!(":{port}"));
    let Some(summary) = summary else {
        return if client_side {
            format!("to {port_text}")
        } else {
            format!("on {port_text}")
        };
    };
    let on_port = if client_side {
        String::new()
    } else {
        format!("on {port_text}, ")
    };
    match summary.egress.as_str() {
        "blocked" => {
            if client_side {
                format!("no network but {port_text}")
            } else {
                format!("no network, on {port_text}")
            }
        }
        "allowlist" => {
            let count = summary.allow_domains.len();
            let domains = if summary.allow_domains.iter().any(|domain| domain == "*") {
                String::new()
            } else {
                format!(" {count} host{}", if count == 1 { "" } else { "s" })
            };
            format!(
                "{on_port}proxy egress{domains}{}",
                if summary.loopback { " + LOCALHOST" } else { "" }
            )
        }
        _ => format!("{on_port}OPEN egress + localhost"),
    }
}

/// The network line of the profiles view.
pub fn network_detail(summary: &ProfileSummary, side: &str) -> String {
    let localhost = if summary.loopback {
        "; LOCALHOST REACHABLE"
    } else {
        ""
    };
    match summary.egress.as_str() {
        "blocked" => if side == "client" {
            "blocked; the plugin opens its server's port only"
        } else {
            "blocked"
        }
        .to_string(),
        "allowlist" => {
            let hosts = &summary.allow_domains;
            let listing = if hosts.is_empty() {
                String::new()
            } else {
                format!(": {}", hosts.join(", "))
            };
            format!(
                "nono proxy to {} host{}{listing}{localhost}",
                hosts.len(),
                if hosts.len() == 1 { "" } else { "s" }
            )
        }
        _ => "OPEN egress, direct connects; LOCALHOST REACHABLE".to_string(),
    }
}

/// nono's directory of user profiles, the ones `nono profile list` shows under
/// "User" and a bare name like `my-opencode-server` refers to.
pub fn nono_user_profiles_dir(env: &Env) -> PathBuf {
    let config = env
        .get("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| home_dir(env).join(".config"), PathBuf::from);
    config.join("nono").join("profiles")
}

/// Where a profile reference comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    None,
    Shipped,
    File,
    User,
    Nono,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSource {
    pub kind: SourceKind,
    pub text: &'static str,
    pub file: Option<String>,
}

/// Where a profile reference comes from: a file shipped in the plugin's
/// `profiles/`, a profile file of your own, a nono user profile (by name, with
/// its file), or a profile built into nono or one of its packages.
pub fn profile_source(
    reference: Option<&str>,
    plugin_root: Option<&str>,
    user_profiles_dir: &Path,
) -> ProfileSource {
    let Some(reference) = reference.filter(|reference| !reference.is_empty()) else {
        return ProfileSource {
            kind: SourceKind::None,
            text: "none",
            file: None,
        };
    };
    if Path::new(reference).is_absolute() || reference.ends_with(".json") {
        let shipped = plugin_root.is_some_and(|root| {
            let parent = lexical_resolve(Path::new(reference))
                .parent()
                .map(Path::to_path_buf);
            parent == Some(lexical_resolve(Path::new(root)).join("profiles"))
        });
        return if shipped {
            ProfileSource {
                kind: SourceKind::Shipped,
                text: "shipped with the plugin",
                file: Some(reference.to_string()),
            }
        } else {
            ProfileSource {
                kind: SourceKind::File,
                text: "your profile file",
                file: Some(reference.to_string()),
            }
        };
    }
    let user = user_profiles_dir.join(format!("{reference}.json"));
    if user.exists() {
        return ProfileSource {
            kind: SourceKind::User,
            text: "nono user profile",
            file: Some(user.to_string_lossy().into_owned()),
        };
    }
    ProfileSource {
        kind: SourceKind::Nono,
        text: "built into nono or a nono package",
        file: None,
    }
}

/// `path.resolve` without touching the file system.
fn lexical_resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut resolved = PathBuf::from("/");
    for component in absolute.components() {
        match component {
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            std::path::Component::Normal(name) => resolved.push(name),
            _ => {}
        }
    }
    resolved
}

/// The processes running under one nono supervisor, in tree order, with their
/// depth below the supervisor and whether they carry nono's confinement marks.
pub fn sandbox_tree(supervisor_pid: Option<u32>, proc_root: &Path) -> Vec<TreeProcess> {
    let Some(supervisor) = supervisor_pid.filter(|pid| *pid > 0) else {
        return Vec::new();
    };
    if !procfs_available(proc_root) {
        return Vec::new();
    }
    let list = descendants(supervisor, proc_root);
    let mut depth: HashMap<u32, i64> = HashMap::from([(supervisor, -1)]);
    let mut children: HashMap<Option<u32>, Vec<usize>> = HashMap::new();
    for (index, info) in list.iter().enumerate() {
        let parent_depth = info
            .ppid
            .and_then(|ppid| depth.get(&ppid).copied())
            .unwrap_or(-1);
        depth.insert(info.pid, parent_depth + 1);
        children.entry(info.ppid).or_default().push(index);
    }
    // Depth-first, so each process sits right under its parent.
    let mut ordered = Vec::new();
    fn walk(
        pid: u32,
        list: &[crate::procfs::ProcessInfo],
        depth: &HashMap<u32, i64>,
        children: &HashMap<Option<u32>, Vec<usize>>,
        out: &mut Vec<TreeProcess>,
    ) {
        for index in children.get(&Some(pid)).into_iter().flatten() {
            let info = &list[*index];
            out.push(TreeProcess {
                pid: info.pid,
                depth: usize::try_from(depth.get(&info.pid).copied().unwrap_or(0)).unwrap_or(0),
                confined: looks_confined(info),
                argv: info.argv.clone(),
            });
            walk(info.pid, list, depth, children, out);
        }
    }
    walk(supervisor, &list, &depth, &children, &mut ordered);
    ordered
}

/// What a frame is collected from.
pub struct CollectInput<'a> {
    pub state_dir: &'a Path,
    pub nono: &'a NonoClient,
    pub herdr: &'a HerdrClient,
    pub proc_root: &'a Path,
    /// Resolved profiles by reference, kept across frames.
    pub profile_cache: &'a mut HashMap<String, Option<ProfileSummary>>,
    /// The client and server profile references the next launch uses.
    pub configured: Option<(Option<String>, Option<String>)>,
}

fn entry_text(entry: &Entry, key: &str) -> String {
    entry
        .get(key)
        .map_or_else(String::new, crate::util::js_string)
}

/// Gathers one row per mapping, merging live nono and Herdr information: one
/// `nono ps` and one `herdr pane list` per frame, however many mappings exist.
/// Running agents also get the process trees of their client and server
/// sandboxes and a summary of each sandbox's profile (cached by reference).
pub fn collect_sandboxes(input: CollectInput) -> Result<Collected> {
    let CollectInput {
        state_dir,
        nono,
        herdr,
        proc_root,
        profile_cache,
        configured,
    } = input;
    let state = load_state(state_dir)?;
    let (sessions, session_error) = match nono.list_sessions(false) {
        Ok(list) => (
            Some(
                list.into_iter()
                    .filter(|session| session.status.as_deref() != Some("exited"))
                    .collect::<Vec<_>>(),
            ),
            None,
        ),
        Err(error) => (None, Some(error.message.clone())),
    };
    let pane_ids = herdr.list_pane_ids().ok();
    let mut rows: Vec<Row> = state
        .panes
        .iter()
        .map(|(_, entry)| {
            let session_name = entry_text(entry, "sessionName");
            let supervisor = crate::util::positive_int(entry.get("supervisorPid"));
            let agent_session = sessions.as_ref().and_then(|list| {
                list.iter().find(|session| {
                    (supervisor.is_some() && session.supervisor_pid == supervisor)
                        || session.name.as_deref() == Some(session_name.as_str())
                })
            });
            let shell_name = shell_session_name(&session_name);
            let server_name = server_session_name(&session_name);
            let server_session = sessions.as_ref().and_then(|list| {
                list.iter()
                    .find(|session| session.name.as_deref() == Some(server_name.as_str()))
            });
            let pane_id = entry_text(entry, "paneId");
            let verification = entry.get("verification").filter(|value| !value.is_null());
            Row {
                pane_exists: pane_ids.as_ref().map(|ids| ids.contains(&pane_id)),
                pane_id,
                agent_kind: entry_text(entry, "agentKind"),
                lifecycle_state: entry_text(entry, "lifecycleState"),
                running: sessions.as_ref().map(|_| agent_session.is_some()),
                server_running: sessions.as_ref().map(|_| server_session.is_some()),
                shells: sessions.as_ref().map(|list| {
                    list.iter()
                        .filter(|session| session.name.as_deref() == Some(shell_name.as_str()))
                        .count()
                }),
                verified: verification
                    .and_then(|report| report.get("ok"))
                    .and_then(Value::as_bool),
                verification: verification.map(|report| summarize_verification(Some(report))),
                local_path: entry_text(entry, "localPath"),
                trees: Trees {
                    client: sandbox_tree(
                        agent_session.and_then(|session| session.supervisor_pid),
                        proc_root,
                    ),
                    server: sandbox_tree(
                        server_session.and_then(|session| session.supervisor_pid),
                        proc_root,
                    ),
                },
                network: Networks::default(),
                session_name,
                entry: entry.clone(),
            }
        })
        .collect();
    let mut summary = |reference: Option<&str>| -> Option<ProfileSummary> {
        let reference = reference.filter(|reference| !reference.is_empty())?;
        if !profile_cache.contains_key(reference) {
            let resolved = nono
                .show_profile(reference)
                .ok()
                .map(|profile| summarize_profile(&profile));
            profile_cache.insert(reference.to_string(), resolved);
        }
        profile_cache.get(reference).cloned().flatten()
    };
    for row in &mut rows {
        let client = row
            .entry
            .get("profile")
            .and_then(Value::as_str)
            .map(str::to_string);
        let server = row
            .entry
            .get("serverProfile")
            .and_then(Value::as_str)
            .map(str::to_string);
        row.network = Networks {
            client: summary(client.as_deref()),
            server: summary(server.as_deref()),
        };
    }
    let configured = configured.map(|(client, server)| Configured {
        client: ConfiguredProfile {
            summary: summary(client.as_deref()),
            reference: client,
        },
        server: ConfiguredProfile {
            summary: summary(server.as_deref()),
            reference: server,
        },
    });
    Ok(Collected {
        rows,
        session_error,
        pane_ids,
        configured,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procfs::fake::{write_fake_proc, FakeProcess};
    use crate::state::save_pane_entry;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    fn entry(value: Value) -> Entry {
        value.as_object().cloned().unwrap()
    }

    fn row(overrides: Value) -> Row {
        let mut row = Row {
            pane_id: "w1:p1".into(),
            pane_exists: Some(true),
            session_name: "herdr-opencode-abc123def456".into(),
            agent_kind: "opencode".into(),
            lifecycle_state: "running".into(),
            running: Some(true),
            server_running: Some(true),
            shells: Some(1),
            verified: Some(false),
            verification: Some("FAILED: Process 7 is not confined".into()),
            local_path: "/home/u/projects/app".into(),
            trees: Trees::default(),
            network: Networks::default(),
            entry: Entry::new(),
        };
        if let Some(object) = overrides.as_object() {
            for (key, value) in object {
                match key.as_str() {
                    "pane_exists" => row.pane_exists = value.as_bool(),
                    "running" => row.running = value.as_bool(),
                    "server_running" => row.server_running = value.as_bool(),
                    "shells" => row.shells = value.as_u64().map(|shells| shells as usize),
                    other => panic!("unknown override {other}"),
                }
            }
        }
        row
    }

    #[test]
    fn health_is_running_stopped_or_stale() {
        let down = json!({"running": false, "server_running": false, "shells": 0});
        let with = |extra: Value| {
            let mut merged = down.as_object().cloned().unwrap();
            merged.extend(extra.as_object().cloned().unwrap());
            row(Value::Object(merged))
        };
        assert_eq!(health(&with(json!({"pane_exists": true}))), Health::Stopped);
        assert_eq!(health(&with(json!({"pane_exists": false}))), Health::Stale);
        assert_eq!(
            health(&with(json!({"pane_exists": false, "running": true}))),
            Health::Running
        );
        assert_eq!(
            health(&with(json!({"server_running": true}))),
            Health::Running
        );
        assert_eq!(
            health(&with(json!({"pane_exists": false, "shells": 1}))),
            Health::Running
        );
        let mut unknown = with(json!({"pane_exists": true}));
        unknown.running = None;
        assert_eq!(health(&unknown), Health::Unknown);
        assert_eq!(Health::Stale.word(), "stale");
        assert_eq!(Health::Stopped.tone(), Tone::Yellow);
    }

    #[test]
    fn worktree_name_is_the_directory_and_the_repository_above_a_herdr_worktree() {
        assert_eq!(
            worktree_name("/home/u/.herdr/worktrees/herdr-nono-plugin/feat-tui/"),
            (
                "feat-tui".to_string(),
                Some("herdr-nono-plugin".to_string())
            )
        );
        assert_eq!(
            worktree_name("/home/u/projects/app"),
            ("app".to_string(), None)
        );
        assert_eq!(worktree_name(""), ("?".to_string(), None));
    }

    #[test]
    fn fit_and_short_path_and_short_command() {
        assert_eq!(fit("abcdef", 4, false), "abc…");
        assert_eq!(fit("/a/b/c/d", 5, true), "…/c/d");
        assert_eq!(fit("ab", 4, false), "ab  ");
        assert_eq!(fit("abc", 1, false), "…");
        assert_eq!(fit("abc", 0, false), "");
        assert_eq!(fit("héllo wörld", 6, false), "héllo…");
        assert_eq!(
            short_path("/home/u/projects/app", "/home/u"),
            "~/projects/app"
        );
        assert_eq!(short_path("/home/u", "/home/u"), "~");
        assert_eq!(short_path("/home/uu/x", "/home/u"), "/home/uu/x");
        assert_eq!(short_path("/x", ""), "/x");
        let words = |items: &[&str]| {
            items
                .iter()
                .map(|item| item.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            short_command(&words(&[
                "/home/u/.opencode/bin/opencode",
                "--server",
                "http://127.0.0.1:4242"
            ])),
            "opencode --server http://127.0.0.1:4242"
        );
        assert_eq!(
            short_command(&words(&[
                "/usr/bin/node",
                "/home/u/app/node_modules/.bin/vitest",
                "run"
            ])),
            "vitest run",
            "interpreters show their script"
        );
        assert_eq!(
            short_command(&words(&["/usr/bin/node", "-e", "x"])),
            "node -e x"
        );
        assert_eq!(short_command(&[]), "?");
        assert_eq!(
            profile_name("/p/profiles/herdr-opencode-client.json"),
            "herdr-opencode-client"
        );
        assert_eq!(profile_name("nolabs-ai/opencode"), "opencode");
    }

    #[test]
    fn stale_rows_have_a_gone_pane_and_nothing_running() {
        assert!(is_stale(&row(
            json!({"pane_exists": false, "running": false, "server_running": false, "shells": 0})
        )));
        assert!(!is_stale(&row(
            json!({"pane_exists": true, "running": false, "server_running": false, "shells": 0})
        )));
        assert!(!is_stale(&row(
            json!({"pane_exists": false, "running": true})
        )));
        assert!(!is_stale(&row(
            json!({"pane_exists": false, "running": false, "server_running": true})
        )));
        assert!(!is_stale(&row(
            json!({"pane_exists": false, "running": false, "server_running": false, "shells": 2})
        )));
        assert!(
            !is_stale(&row(
                json!({"pane_exists": null, "running": false, "server_running": false, "shells": 0})
            )),
            "an unknown pane is not stale"
        );
    }

    #[test]
    fn the_nono_cell_lists_what_runs() {
        assert_eq!(
            session_cell(&row(json!({}))),
            ("client+server+1 sh".to_string(), Tone::Green)
        );
        assert_eq!(
            session_cell(&row(
                json!({"running": false, "server_running": false, "shells": 0})
            )),
            ("-".to_string(), Tone::Gray)
        );
        assert_eq!(
            session_cell(&row(
                json!({"running": false, "server_running": true, "shells": 0})
            )),
            ("server".to_string(), Tone::Yellow)
        );
        assert_eq!(
            session_cell(&row(json!({"running": null}))),
            ("unknown".to_string(), Tone::Gray)
        );
    }

    fn summary(egress: &str, domains: &[&str], loopback: bool) -> ProfileSummary {
        let mut summary = summarize_profile(&json!({}));
        summary.egress = egress.into();
        summary.allow_domains = domains.iter().map(|domain| domain.to_string()).collect();
        summary.loopback = loopback;
        summary
    }

    #[test]
    fn network_labels_and_details_say_what_a_sandbox_can_reach() {
        assert_eq!(
            network_label(Some(&summary("blocked", &[], false)), true, Some(4242)),
            "no network but :4242"
        );
        assert_eq!(
            network_label(Some(&summary("blocked", &[], false)), false, Some(4242)),
            "no network, on :4242"
        );
        assert_eq!(
            network_label(Some(&summary("allowlist", &["*"], true)), false, Some(4242)),
            "on :4242, proxy egress + LOCALHOST"
        );
        assert_eq!(
            network_label(
                Some(&summary("allowlist", &["a.com", "b.com"], false)),
                false,
                Some(4242)
            ),
            "on :4242, proxy egress 2 hosts"
        );
        assert_eq!(
            network_label(Some(&summary("allowlist", &["a.com"], false)), true, None),
            "proxy egress 1 host"
        );
        assert_eq!(
            network_label(Some(&summary("open", &[], true)), false, Some(4242)),
            "on :4242, OPEN egress + localhost"
        );
        assert_eq!(network_label(None, true, Some(4242)), "to :4242");
        assert_eq!(network_label(None, false, None), "on its port");
        assert_eq!(
            network_detail(&summary("blocked", &[], false), "client"),
            "blocked; the plugin opens its server's port only"
        );
        assert_eq!(
            network_detail(&summary("blocked", &[], false), "server"),
            "blocked"
        );
        assert_eq!(
            network_detail(
                &summary("allowlist", &["api.githubcopilot.com", "github.com"], false),
                "server"
            ),
            "nono proxy to 2 hosts: api.githubcopilot.com, github.com"
        );
        assert_eq!(
            network_detail(&summary("allowlist", &["*"], true), "server"),
            "nono proxy to 1 host: *; LOCALHOST REACHABLE"
        );
        assert_eq!(
            network_detail(&summary("open", &[], true), "server"),
            "OPEN egress, direct connects; LOCALHOST REACHABLE"
        );
    }

    #[test]
    fn profile_source_tells_shipped_profiles_files_user_profiles_and_nonos_own_apart() {
        let user_dir = tempfile::tempdir().unwrap();
        std::fs::write(user_dir.path().join("my-server.json"), "{}").unwrap();
        let source =
            |reference: Option<&str>| profile_source(reference, Some("/p"), user_dir.path());
        assert_eq!(
            source(Some("/p/profiles/herdr-opencode-server.json")).kind,
            SourceKind::Shipped
        );
        assert_eq!(
            source(Some("/p/../p/profiles/x.json")).kind,
            SourceKind::Shipped
        );
        assert_eq!(source(Some("/p/other/x.json")).kind, SourceKind::File);
        assert_eq!(
            source(Some("/home/u/my.json")),
            ProfileSource {
                kind: SourceKind::File,
                text: "your profile file",
                file: Some("/home/u/my.json".into())
            }
        );
        let user = source(Some("my-server"));
        assert_eq!(
            (user.kind, user.text),
            (SourceKind::User, "nono user profile")
        );
        assert_eq!(
            user.file,
            Some(
                user_dir
                    .path()
                    .join("my-server.json")
                    .to_string_lossy()
                    .into_owned()
            )
        );
        assert_eq!(source(Some("node-dev")).kind, SourceKind::Nono);
        assert_eq!(source(None).kind, SourceKind::None);
        assert_eq!(source(Some("")).kind, SourceKind::None);
        assert_eq!(
            profile_source(Some("/p/profiles/x.json"), None, user_dir.path()).kind,
            SourceKind::File
        );
    }

    #[test]
    fn the_user_profiles_directory_follows_xdg_then_home() {
        let env = |pairs: &[(&str, &str)]| -> Env {
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect()
        };
        assert_eq!(
            nono_user_profiles_dir(&env(&[("HOME", "/home/u")])),
            Path::new("/home/u/.config/nono/profiles")
        );
        assert_eq!(
            nono_user_profiles_dir(&env(&[("HOME", "/home/u"), ("XDG_CONFIG_HOME", "/x")])),
            Path::new("/x/nono/profiles")
        );
    }

    #[test]
    fn sandbox_tree_lists_a_supervisors_processes_depth_first_with_their_confinement() {
        let root = tempfile::tempdir().unwrap();
        let process = |pid: u32, ppid: u32, argv: &[&'static str], confined: bool| FakeProcess {
            pid,
            ppid,
            argv: argv.to_vec(),
            no_new_privs: if confined { None } else { Some(false) },
            ..Default::default()
        };
        write_fake_proc(
            root.path(),
            &[
                process(110, 1, &["nono"], false),
                process(111, 110, &["opencode", "serve"], true),
                process(112, 111, &["bash", "-c", "a"], true),
                process(114, 111, &["bash", "-c", "b"], true),
                process(113, 112, &["sleep", "5"], false),
            ],
            &[],
        );
        let shape: Vec<(u32, usize, bool)> = sandbox_tree(Some(110), root.path())
            .iter()
            .map(|item| (item.pid, item.depth, item.confined))
            .collect();
        assert_eq!(
            shape,
            [
                (111, 0, true),
                (112, 1, true),
                (113, 2, false),
                (114, 1, true)
            ]
        );
        assert!(sandbox_tree(None, root.path()).is_empty());
        assert!(sandbox_tree(Some(110), &root.path().join("absent")).is_empty());
    }

    /// A fake nono: `ps --json` prints sessions.json and counts calls; `profile show --json <ref>` counts calls and prints
    /// a blocked profile for refs containing "client", an allow-everything one otherwise.
    fn fake_nono(dir: &Path) -> PathBuf {
        let script = dir.join("nono");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
case "$1" in
  ps) echo x >> '{dir}/ps-calls'; cat '{dir}/sessions.json' ;;
  profile) echo x >> '{dir}/profile-calls'
           case "$4" in
             *client*) echo '{{"name":"client","network":{{"block":true}}}}' ;;
             *) echo '{{"name":"server","network":{{"allow_domain":["*"]}}}}' ;;
           esac ;;
  *) exit 2 ;;
esac
"#,
                dir = dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// A fake herdr: `pane list` counts calls and lists the panes in `panes`.
    fn fake_herdr(dir: &Path) -> PathBuf {
        let script = dir.join("herdr");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
echo x >> '{dir}/herdr-calls'
echo '{{"result":{{"panes":[{{"pane_id":"w1:p1"}}]}}}}'
"#,
                dir = dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn lines_in(file: PathBuf) -> usize {
        std::fs::read_to_string(file).map_or(0, |text| text.lines().count())
    }

    #[test]
    fn collect_sandboxes_merges_mappings_live_sessions_and_panes_with_one_call_each() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        let name = "herdr-opencode-abc123def456";
        let base = |extra: Value| {
            let mut entry = entry(
                json!({"sessionName": name, "agentKind": "opencode", "localPath": "/w", "workdir": "/w", "lifecycleState": "running", "profile": "/p/client.json", "launchCount": 1}),
            );
            entry.extend(extra.as_object().cloned().unwrap());
            entry
        };
        save_pane_entry(&state_dir, "w1:p1", &base(json!({"port": 4242, "serverProfile": "/p/server.json", "verification": {"ok": false, "supported": true, "processes": [], "problems": ["Process 7 is not confined"]}}))).unwrap();
        save_pane_entry(&state_dir, "w1:p2", &base(json!({"sessionName": "herdr-opencode-000000000002", "lifecycleState": "exited", "profile": "/p/client.json"}))).unwrap();
        std::fs::write(
            dir.path().join("sessions.json"),
            json!([
                {"session_id": "s1", "name": name, "supervisor_pid": 5, "status": "running"},
                {"session_id": "s2", "name": format!("{name}-shell"), "status": "running"},
                {"session_id": "s4", "name": format!("{name}-server"), "status": "running"},
                {"session_id": "s3", "name": "herdr-opencode-000000000002", "status": "exited"},
            ])
            .to_string(),
        )
        .unwrap();
        let nono = NonoClient::new(
            fake_nono(dir.path()).to_str().unwrap(),
            Env::from([("PATH".to_string(), std::env::var("PATH").unwrap())]),
        );
        let herdr = HerdrClient::new(
            fake_herdr(dir.path()).to_str().unwrap(),
            Env::from([("PATH".to_string(), std::env::var("PATH").unwrap())]),
        );
        let mut cache = HashMap::new();
        let no_proc = dir.path().join("no-proc");
        let collect = |cache: &mut HashMap<String, Option<ProfileSummary>>| {
            collect_sandboxes(CollectInput {
                state_dir: &state_dir,
                nono: &nono,
                herdr: &herdr,
                proc_root: &no_proc,
                profile_cache: cache,
                configured: None,
            })
            .unwrap()
        };
        let data = collect(&mut cache);
        collect(&mut cache);
        assert_eq!(data.session_error, None);
        assert_eq!(data.pane_ids, Some(vec!["w1:p1".to_string()]));
        assert_eq!(
            lines_in(dir.path().join("ps-calls")),
            2,
            "one nono ps per frame"
        );
        assert_eq!(
            lines_in(dir.path().join("herdr-calls")),
            2,
            "one pane list per frame"
        );
        let by_pane: HashMap<&str, &Row> = data
            .rows
            .iter()
            .map(|row| (row.pane_id.as_str(), row))
            .collect();
        let first = by_pane["w1:p1"];
        assert_eq!(
            (
                first.running,
                first.server_running,
                first.shells,
                first.verified
            ),
            (Some(true), Some(true), Some(1), Some(false))
        );
        assert_eq!(
            first.verification.as_deref(),
            Some("FAILED: Process 7 is not confined")
        );
        let second = by_pane["w1:p2"];
        assert_eq!(
            (second.running, second.pane_exists),
            (Some(false), Some(false)),
            "an exited session does not count"
        );
        assert!(is_stale(second));
        assert!(!is_stale(first));
        assert_eq!(first.network.client.as_ref().unwrap().egress, "blocked");
        assert_eq!(first.network.server.as_ref().unwrap().egress, "allowlist");
        assert_eq!(
            lines_in(dir.path().join("profile-calls")),
            2,
            "each distinct profile is resolved once and cached across frames"
        );
        assert_eq!(first.trees, Trees::default(), "no /proc, no trees");
    }

    #[test]
    fn collect_sandboxes_reports_a_failing_nono_and_summarizes_the_next_launch_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let broken = NonoClient::new("/definitely/not/nono", Env::new());
        let herdr = HerdrClient::new("/definitely/not/herdr", Env::new());
        let mut cache = HashMap::new();
        let data = collect_sandboxes(CollectInput {
            state_dir: &dir.path().join("state"),
            nono: &broken,
            herdr: &herdr,
            proc_root: &dir.path().join("no-proc"),
            profile_cache: &mut cache,
            configured: Some((
                Some("/p/profiles/client.json".into()),
                Some("/p/profiles/server.json".into()),
            )),
        })
        .unwrap();
        assert!(data.rows.is_empty());
        assert!(data
            .session_error
            .unwrap()
            .starts_with("Could not run nono"));
        assert_eq!(
            data.pane_ids, None,
            "an unreachable Herdr leaves the pane list unknown"
        );
        let configured = data.configured.unwrap();
        assert_eq!(
            configured.client.reference.as_deref(),
            Some("/p/profiles/client.json")
        );
        assert_eq!(
            configured.client.summary, None,
            "a profile nono cannot resolve has no summary"
        );
        assert_eq!(cache.len(), 2, "failures are cached too");
    }

    #[test]
    fn collect_sandboxes_summarizes_the_configured_profiles() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sessions.json"), "[]").unwrap();
        let env = Env::from([("PATH".to_string(), std::env::var("PATH").unwrap())]);
        let nono = NonoClient::new(fake_nono(dir.path()).to_str().unwrap(), env.clone());
        let herdr = HerdrClient::new(fake_herdr(dir.path()).to_str().unwrap(), env);
        let mut cache = HashMap::new();
        let data = collect_sandboxes(CollectInput {
            state_dir: &dir.path().join("state"),
            nono: &nono,
            herdr: &herdr,
            proc_root: &dir.path().join("no-proc"),
            profile_cache: &mut cache,
            configured: Some((
                Some("/p/profiles/herdr-opencode-client.json".into()),
                Some("/p/profiles/herdr-opencode-server.json".into()),
            )),
        })
        .unwrap();
        let configured = data.configured.unwrap();
        assert_eq!(configured.client.summary.unwrap().egress, "blocked");
        assert_eq!(configured.server.summary.unwrap().allow_domains, ["*"]);
    }
}

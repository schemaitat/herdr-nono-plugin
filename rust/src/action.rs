//! Action dispatcher. Every action prints the result marker line first and
//! uses stderr for diagnostics, so `herdr plugin log list` stays parseable.

use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::agents::{resolve_agent, ResolvedAgent};
use crate::config::{load_config, LoadedConfig};
use crate::constants::{
    ACTION_IDS, BRIDGE_START_TIMEOUT_ENV, BRIDGE_START_TIMEOUT_MS, KEYBINDING_INSTALL_TIMEOUT_MS,
    MIN_NONO_VERSION, NONO_CALL_TIMEOUT_MS,
};
use crate::context::{
    git_toplevel, read_context, read_plugin_env, require_plugin_dirs, resolve_pane_id,
    resolve_workdir, resolve_workspace_root, Env, PluginDirs,
};
use crate::errors::{ErrorKind, PluginError, Result};
use crate::exec::{run_cli, ExecError, RunOptions};
use crate::herdr::{HerdrClient, SplitPane};
use crate::hostservice::{detect_opencode_service, host_service_warning, HostService};
use crate::lifecycle::{
    agent_for_entry, assert_workspace_root, bridge_is_running, find_executable, live_shells,
    shell_is_running, Lifecycle, LifecycleOptions,
};
use crate::naming::session_name_for;
use crate::nono::{summarize_profile, NonoClient, ProfileSummary};
use crate::probes::{run_probes, Check, ProbeOptions};
use crate::result::{emit_result, failure_payload};
use crate::shell::build_pane_command;
use crate::state::{
    delete_pane_entry, get_pane_entry, load_state, pane_lock_path, save_pane_entry, with_pane_lock,
    Entry,
};
use crate::util::{iso_now, random_hex};
use crate::verify::summarize_verification;

/// What a handler returns: the fields of the result line and the human lines after it.
struct ActionOutput {
    payload: Map<String, Value>,
    lines: Vec<String>,
}

impl ActionOutput {
    fn new(payload: Value) -> Self {
        ActionOutput {
            payload: payload.as_object().cloned().unwrap_or_default(),
            lines: Vec::new(),
        }
    }

    fn with_lines(mut self, lines: Vec<String>) -> Self {
        self.lines = lines;
        self
    }
}

struct Deps {
    env: Env,
    plugin: PluginDirs,
    context: Map<String, Value>,
    config: LoadedConfig,
    nono: NonoClient,
    herdr: HerdrClient,
    lifecycle: Lifecycle,
}

impl Deps {
    fn state_dir(&self) -> &Path {
        Path::new(&self.plugin.state_dir)
    }

    fn workspace_id(&self) -> Option<String> {
        self.context
            .get("workspace_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| self.env.get("HERDR_WORKSPACE_ID").cloned())
    }
}

fn target_error(message: String) -> PluginError {
    PluginError::new(ErrorKind::Target, message)
}

fn entry_str<'a>(entry: &'a Entry, key: &str) -> &'a str {
    entry.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn show(value: Option<&Value>) -> String {
    value.map_or_else(|| "undefined".to_string(), crate::util::js_string)
}

fn short_id(session_name: &str) -> String {
    session_name
        .rsplit('-')
        .next()
        .unwrap_or("")
        .chars()
        .take(6)
        .collect()
}

/// The mapping a pane-scoped action acts on, and how it was found.
struct Target {
    pane_id: String,
    entry: Entry,
    via_workspace: bool,
    orphan: bool,
}

fn describe_mappings(entries: &[&Entry]) -> String {
    entries
        .iter()
        .map(|entry| {
            format!(
                "{} ({})",
                entry_str(entry, "paneId"),
                entry_str(entry, "sessionName")
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Finds the mapping a pane-scoped action should act on: the focused pane's
/// own mapping; else the single mapping in the focused workspace (users tend
/// to run actions from the pane next to the agent); else the single mapping
/// whose pane Herdr no longer has, which happens after a Herdr restart. Such an
/// orphan is flagged so the caller can give it a new pane.
fn require_focused_mapping(deps: &Deps) -> Result<Target> {
    let focused = resolve_pane_id(&deps.context, &deps.env);
    if let Some(own) = get_pane_entry(deps.state_dir(), focused.as_deref())? {
        return Ok(Target {
            pane_id: focused.unwrap_or_default(),
            entry: own,
            via_workspace: false,
            orphan: false,
        });
    }
    let focused_text = focused.clone().unwrap_or_else(|| "(none)".to_string());
    let workspace_id = deps.workspace_id();
    let state = load_state(deps.state_dir())?;
    let all: Vec<&Entry> = state.panes.iter().map(|(_, entry)| entry).collect();
    let candidates: Vec<&Entry> = match &workspace_id {
        Some(workspace) => all
            .iter()
            .copied()
            .filter(|entry| {
                entry.get("workspaceId").and_then(Value::as_str) == Some(workspace.as_str())
                    || entry_str(entry, "paneId").starts_with(&format!("{workspace}:"))
            })
            .collect(),
        None => Vec::new(),
    };
    if candidates.len() == 1 {
        let entry = candidates[0];
        let pane_exists = deps.herdr.get_pane(entry_str(entry, "paneId"))?.is_some();
        eprintln!(
            "pane {focused_text} has no sandboxed agent; using the workspace's only mapping, pane {} ({}){}",
            entry_str(entry, "paneId"),
            entry_str(entry, "sessionName"),
            if pane_exists { "" } else { ", whose pane is gone" }
        );
        return Ok(Target {
            pane_id: entry_str(entry, "paneId").to_string(),
            entry: entry.clone(),
            via_workspace: pane_exists,
            orphan: !pane_exists,
        });
    }
    if candidates.len() > 1 {
        return Err(target_error(format!(
            "Pane {focused_text} has no sandboxed agent and this workspace has several: {}. Focus the pane you mean.",
            describe_mappings(&candidates)
        )));
    }
    if !all.is_empty() {
        let pane_ids = deps.herdr.list_pane_ids()?;
        let orphans: Vec<&Entry> = all
            .iter()
            .copied()
            .filter(|entry| !pane_ids.iter().any(|id| id == entry_str(entry, "paneId")))
            .collect();
        if orphans.len() == 1 {
            let entry = orphans[0];
            eprintln!(
                "pane {} no longer exists; adopting its mapping {}",
                entry_str(entry, "paneId"),
                entry_str(entry, "sessionName")
            );
            return Ok(Target {
                pane_id: entry_str(entry, "paneId").to_string(),
                entry: entry.clone(),
                via_workspace: false,
                orphan: true,
            });
        }
        if orphans.len() > 1 {
            return Err(target_error(format!(
                "Several mappings lost their panes: {}. Run prune-mappings, then try again.",
                describe_mappings(&orphans)
            )));
        }
        let first_workspace = entry_str(all[0], "paneId")
            .split(':')
            .next()
            .unwrap_or_default()
            .to_string();
        return Err(target_error(format!(
            "No sandboxed agent is mapped in workspace {}. Existing mappings: {}. Focus that workspace, for example \"herdr workspace focus {first_workspace}\", and try again.",
            workspace_id.as_deref().unwrap_or("(unknown)"),
            describe_mappings(&all)
        )));
    }
    Err(target_error(match &focused {
        Some(pane) => format!("No sandboxed agent is mapped to pane {pane} or to another pane in this workspace. Run start-agent first."),
        None => "No focused pane was provided, so there is no mapping to act on.".to_string(),
    }))
}

fn refuse_while_agent_runs(
    deps: &Deps,
    target: &Target,
    verb: &str,
    shells_block: bool,
) -> Result<()> {
    let entry = &target.entry;
    let conflict = |message: String| PluginError::new(ErrorKind::Conflict, message);
    if bridge_is_running(entry) {
        return Err(conflict(format!(
            "Pane {} still runs {} (bridge pid {}, since {}). Exit the agent, or run stop, before you {verb}.",
            target.pane_id,
            entry_str(entry, "sessionName"),
            show(entry.get("bridgePid")),
            show(entry.get("bridgeStartedAt"))
        )));
    }
    if shells_block {
        let shells = live_shells(entry);
        if !shells.is_empty() {
            let pids: Vec<String> = shells.iter().map(|shell| show(shell.get("pid"))).collect();
            let (what, them) = if shells.len() == 1 {
                ("an open-shell session".to_string(), "it")
            } else {
                (format!("{} open-shell sessions", shells.len()), "them")
            };
            return Err(conflict(format!(
                "Pane {} has {what} for {} (pid {}). Close {them} before you {verb}.",
                target.pane_id,
                entry_str(entry, "sessionName"),
                pids.join(", ")
            )));
        }
    }
    if target.orphan {
        return Ok(());
    }
    let agent = if target.via_workspace {
        deps.herdr.get_pane(&target.pane_id)?.and_then(|pane| {
            pane.get("agent")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
    } else {
        deps.context
            .get("focused_pane_agent")
            .and_then(Value::as_str)
            .filter(|agent| !agent.is_empty())
            .map(str::to_string)
    };
    if let Some(agent) = agent {
        return Err(conflict(format!(
            "Pane {} is still running agent \"{agent}\". Exit it before you {verb}. If nothing is running there, clear a stale report with \"herdr pane release-agent {} --source nono.sandbox --agent {agent}\".",
            target.pane_id, target.pane_id
        )));
    }
    Ok(())
}

/// How long to wait for a typed bridge command to acknowledge itself, in
/// milliseconds. An unset, empty or non-positive override means the default.
pub fn bridge_start_timeout(env: &Env) -> u64 {
    env.get(BRIDGE_START_TIMEOUT_ENV)
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|ms| ms.is_finite() && *ms > 0.0)
        .map_or(BRIDGE_START_TIMEOUT_MS, |ms| ms as u64)
}

/// The directory a pane for a mapping opens in: the recorded working
/// directory, else the workspace root, else nothing when both are gone.
pub fn entry_cwd(entry: &Entry) -> Option<String> {
    ["workdir", "localPath"]
        .iter()
        .filter_map(|key| entry.get(*key).and_then(Value::as_str))
        .find(|path| Path::new(path).exists())
        .map(str::to_string)
}

/// The command typed into a pane to run the bridge.
fn bridge_command(
    deps: &Deps,
    mode: &str,
    pane_id: &str,
    detection_kind: Option<&str>,
    launch_id: Option<&str>,
) -> Result<String> {
    let exe = std::env::current_exe().map_err(|error| {
        PluginError::new(
            ErrorKind::Startup,
            format!("Could not find the plugin binary to run the bridge with: {error}"),
        )
    })?;
    let mut argv: Vec<String> = [
        &exe.to_string_lossy(),
        "bridge",
        mode,
        "--state-dir",
        &deps.plugin.state_dir,
        "--config-dir",
        &deps.plugin.config_dir,
        "--pane-id",
        pane_id,
        "--plugin-root",
        &deps.plugin.env.plugin_root,
        "--herdr-bin",
        &deps.plugin.env.herdr_bin,
    ]
    .iter()
    .map(|word| word.to_string())
    .collect();
    argv.extend(["--nono-bin".to_string(), deps.nono.bin.clone()]);
    if let Some(launch_id) = launch_id {
        argv.extend(["--launch-id".to_string(), launch_id.to_string()]);
    }
    let env: Vec<(String, String)> = detection_kind
        .map(|kind| vec![("HERDR_AGENT".to_string(), kind.to_string())])
        .unwrap_or_default();
    build_pane_command(&argv, &env)
}

struct StartedBridge {
    pane_id: String,
    moved_to: Option<String>,
}

/// Types the bridge command into `pane_id` and waits until the bridge has
/// touched the mapping. A pane that is not at a shell prompt swallows typed
/// text, so when nothing happens the mapping is moved to a fresh pane next to
/// the user and started there.
fn start_bridge(
    deps: &Deps,
    pane_id: &str,
    mode: &str,
    agent: &ResolvedAgent,
    label: &str,
) -> Result<StartedBridge> {
    let launch_id = random_hex(6);
    deps.herdr.run_in_pane(
        pane_id,
        &bridge_command(
            deps,
            mode,
            pane_id,
            agent.herdr_detection_kind.as_deref(),
            Some(&launch_id),
        )?,
    )?;
    let acknowledged = || -> bool {
        get_pane_entry(deps.state_dir(), Some(pane_id))
            .ok()
            .flatten()
            .is_some_and(|entry| {
                entry.get("bridgeLaunchId").and_then(Value::as_str) == Some(launch_id.as_str())
            })
    };
    let deadline = Instant::now() + Duration::from_millis(bridge_start_timeout(&deps.env));
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(150));
        if acknowledged() {
            return Ok(StartedBridge {
                pane_id: pane_id.to_string(),
                moved_to: None,
            });
        }
    }
    let Some(entry) = get_pane_entry(deps.state_dir(), Some(pane_id))? else {
        return Ok(StartedBridge {
            pane_id: pane_id.to_string(),
            moved_to: None,
        });
    };
    if acknowledged() {
        return Ok(StartedBridge {
            pane_id: pane_id.to_string(),
            moved_to: None,
        });
    }
    if bridge_is_running(&entry) {
        eprintln!(
            "pane {pane_id} still runs an earlier bridge for {} (pid {}); leaving the mapping there",
            entry_str(&entry, "sessionName"),
            show(entry.get("bridgePid"))
        );
        return Ok(StartedBridge {
            pane_id: pane_id.to_string(),
            moved_to: None,
        });
    }
    eprintln!("pane {pane_id} did not run the bridge command; starting in a new pane instead");
    let target = Target {
        pane_id: pane_id.to_string(),
        entry,
        via_workspace: false,
        orphan: false,
    };
    let Some(fresh) = rehome_orphan(deps, &target, label, Some(&acknowledged))? else {
        eprintln!("pane {pane_id} ran the bridge command after all; keeping the agent there");
        return Ok(StartedBridge {
            pane_id: pane_id.to_string(),
            moved_to: None,
        });
    };
    if let Err(error) = deps
        .herdr
        .rename_pane(pane_id, &format!("{label} (moved to {fresh})"))
    {
        eprintln!("could not relabel pane {pane_id}: {}", error.message);
    }
    deps.herdr.run_in_pane(
        &fresh,
        &bridge_command(
            deps,
            mode,
            &fresh,
            agent.herdr_detection_kind.as_deref(),
            None,
        )?,
    )?;
    Ok(StartedBridge {
        pane_id: fresh.clone(),
        moved_to: Some(fresh),
    })
}

fn fresh_entry(
    session_name: &str,
    agent: &ResolvedAgent,
    local_path: &str,
    workdir: &str,
    source_pane_id: Option<&str>,
    workspace_id: Option<&str>,
) -> Entry {
    let Value::Object(entry) = json!({
        "sessionName": session_name,
        "workspaceId": workspace_id,
        "agentKind": agent.kind,
        "localPath": local_path,
        "workdir": workdir,
        "profile": agent.profile_ref,
        "serverProfile": agent.server_profile_ref,
        "lifecycleState": "provisional",
        "createdAt": iso_now(),
        "sourcePaneId": source_pane_id,
        "launchCount": 0,
        "lastError": null,
        "verification": null,
    }) else {
        unreachable!("an object literal")
    };
    entry
}

/// Extracts `major.minor.patch` from a version string.
pub fn parse_version(value: &str) -> Option<[u64; 3]> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let found = PATTERN
        .get_or_init(|| Regex::new(r"(\d+)\.(\d+)\.(\d+)").expect("a valid pattern"))
        .captures(value)?;
    Some([
        found[1].parse().ok()?,
        found[2].parse().ok()?,
        found[3].parse().ok()?,
    ])
}

/// A warning when the nono version is older than the one this plugin targets, else `None`.
pub fn nono_version_warning(version: Option<&str>) -> Option<String> {
    let found = parse_version(version?)?;
    let wanted = parse_version(MIN_NONO_VERSION)?;
    for index in 0..3 {
        if found[index] != wanted[index] {
            return (found[index] < wanted[index]).then(|| {
                format!(
                    "nono {}.{}.{} is older than {MIN_NONO_VERSION}; the plugin was written against the newer CLI and some flags may be missing.",
                    found[0], found[1], found[2]
                )
            });
        }
    }
    None
}

/// Opens the pane an agent will run in: a split next to the anchor pane, or a
/// new tab when `openIn` is "tab". Returns the new pane id, already labelled.
fn open_agent_pane(
    deps: &Deps,
    anchor_pane_id: Option<&str>,
    cwd: Option<&str>,
    label: &str,
) -> Result<String> {
    let config = &deps.config.config;
    let pane_id = if config.open_in == "tab" {
        deps.herdr
            .create_tab(deps.workspace_id().as_deref(), cwd, Some(label), true)?
            .pane_id
    } else {
        deps.herdr.split_pane(&SplitPane {
            pane_id: anchor_pane_id,
            direction: &config.pane_direction,
            ratio: config.pane_ratio,
            cwd,
            focus: true,
        })?
    };
    deps.herdr.rename_pane(&pane_id, label)?;
    Ok(pane_id)
}

/// Runs `body` holding the locks of every pane id, taken in a fixed order so
/// two mirrored moves cannot wait for each other.
fn with_locks<T>(
    state_dir: &Path,
    pane_ids: &[String],
    body: &mut dyn FnMut() -> Result<T>,
) -> Result<T> {
    match pane_ids.split_first() {
        None => body(),
        Some((first, rest)) => {
            with_pane_lock(state_dir, first, || with_locks(state_dir, rest, body))
        }
    }
}

/// Gives a mapping a new pane next to the focused one and moves the mapping
/// there. Returns the new pane id, or `None` when `abandon_if` reports, once the
/// pane exists, that the mapping must stay where it is; the unused pane is
/// closed again then.
fn rehome_orphan(
    deps: &Deps,
    target: &Target,
    label: &str,
    abandon_if: Option<&dyn Fn() -> bool>,
) -> Result<Option<String>> {
    let state_dir = deps.state_dir();
    let old_pane_id = target
        .entry
        .get("paneId")
        .and_then(Value::as_str)
        .unwrap_or(&target.pane_id)
        .to_string();
    let focused = resolve_pane_id(&deps.context, &deps.env);
    let pane_id = open_agent_pane(
        deps,
        focused.as_deref(),
        entry_cwd(&target.entry).as_deref(),
        label,
    )?;
    let close_spare = || {
        if let Err(error) = deps.herdr.close_pane(&pane_id) {
            eprintln!(
                "could not close the unused pane {pane_id}: {}",
                error.message
            );
        }
    };
    let abandoned = || pane_id != old_pane_id && abandon_if.is_some_and(|check| check());
    if abandoned() {
        close_spare();
        return Ok(None);
    }
    let mut ordered = vec![old_pane_id.clone(), pane_id.clone()];
    ordered.sort_by_key(|id| pane_lock_path(state_dir, id).ok());
    ordered.dedup();
    let moved = with_locks(state_dir, &ordered, &mut || {
        let Some(latest) = get_pane_entry(state_dir, Some(&old_pane_id))? else {
            return Err(PluginError::new(
                ErrorKind::Conflict,
                format!("The mapping of pane {old_pane_id} disappeared while a new pane was being opened for it; nothing was moved."),
            ));
        };
        if abandoned() {
            return Ok(None);
        }
        if bridge_is_running(&latest) {
            return Err(PluginError::new(
                ErrorKind::Conflict,
                format!("The mapping of pane {old_pane_id} is in use again (bridge {}); nothing was moved.", show(latest.get("bridgePid"))),
            ));
        }
        let mut next = latest.clone();
        next.insert("workspaceId".into(), json!(deps.workspace_id()));
        next.insert("sourcePaneId".into(), json!(focused));
        next.insert("adoptedFrom".into(), json!(old_pane_id));
        if pane_id != old_pane_id {
            if let Some(displaced) = get_pane_entry(state_dir, Some(&pane_id))? {
                if bridge_is_running(&displaced) || shell_is_running(&displaced) {
                    return Err(PluginError::new(
                        ErrorKind::Conflict,
                        format!("Herdr handed out pane {pane_id}, but its mapping to {} is still in use; nothing was moved.", entry_str(&displaced, "sessionName")),
                    ));
                }
            }
        }
        save_pane_entry(state_dir, &pane_id, &next)?;
        if pane_id != old_pane_id {
            delete_pane_entry(state_dir, &old_pane_id)?;
        }
        Ok(Some(next))
    });
    match moved {
        Err(error) => {
            close_spare();
            Err(error)
        }
        Ok(None) => {
            close_spare();
            Ok(None)
        }
        Ok(Some(_)) => Ok(Some(pane_id)),
    }
}

/// Pane label for an agent pane: the agent kind plus the session's short id, so
/// several agents in one workspace stay apart.
fn pane_label(agent_kind: &str, session_name: &str) -> String {
    format!("nono {agent_kind} {}", short_id(session_name))
}

/// The report lines printed by scripts/install-keybindings.sh.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeybindingReport {
    pub config_path: Option<String>,
    pub added: Vec<Binding>,
    pub existing: Vec<Binding>,
    pub warnings: Vec<String>,
    pub reloaded: bool,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Binding {
    pub key: String,
    pub action: String,
}

/// Parses the report lines printed by scripts/install-keybindings.sh.
pub fn parse_keybinding_report(text: &str) -> KeybindingReport {
    static PATTERNS: OnceLock<[Regex; 4]> = OnceLock::new();
    let [config, bound, already, warning] = PATTERNS.get_or_init(|| {
        [
            Regex::new(r"^config: (.+)$"),
            Regex::new(r"^bound (\S+) -> nono\.sandbox\.(\S+)$"),
            Regex::new(r"^already bound: nono\.sandbox\.(\S+) \((.+)\)$"),
            Regex::new(r"^warning: (.+)$"),
        ]
        .map(|pattern| pattern.expect("a valid pattern"))
    });
    let mut report = KeybindingReport::default();
    for line in text.split('\n') {
        if let Some(found) = config.captures(line) {
            report.config_path = Some(found[1].to_string());
        } else if let Some(found) = bound.captures(line) {
            report.added.push(Binding {
                key: found[1].to_string(),
                action: found[2].to_string(),
            });
        } else if let Some(found) = already.captures(line) {
            report.existing.push(Binding {
                key: found[2].to_string(),
                action: found[1].to_string(),
            });
        } else if let Some(found) = warning.captures(line) {
            report.warnings.push(found[1].to_string());
        } else if line == "reloaded" {
            report.reloaded = true;
        }
    }
    report
}

// ---- the actions ------------------------------------------------------------------

fn profile_json(reference: &str, summary: &ProfileSummary) -> Value {
    let mut object = Map::new();
    object.insert("ref".into(), json!(reference));
    if let Value::Object(fields) = serde_json::to_value(summary).unwrap_or(Value::Null) {
        object.extend(fields);
    }
    Value::Object(object)
}

fn resolve_profile(deps: &Deps, reference: &str) -> Result<ProfileSummary> {
    deps.nono
        .show_profile(reference)
        .map(|profile| summarize_profile(&profile))
        .map_err(|error| {
            let hint = if error.output.contains("nolabs-ai/opencode") {
                " Install the OpenCode pack with \"nono pull nolabs-ai/opencode\"."
            } else {
                ""
            };
            PluginError::new(
                ErrorKind::Config,
                format!(
                    "nono cannot resolve the profile {reference}: {}.{hint}",
                    error.message
                ),
            )
            .with_output(error.output.clone())
        })
}

fn describe_profile(label: &str, reference: &str, item: &ProfileSummary) -> String {
    let extends = if item.extends.is_empty() {
        "nothing".to_string()
    } else {
        item.extends.join(", ")
    };
    let domains = if item.allow_domains.is_empty() {
        String::new()
    } else {
        format!(" [{}]", item.allow_domains.join(", "))
    };
    format!(
        "{label}: {reference} (extends {extends}; egress {}{domains}; localhost {}; AF_UNIX mediation {})",
        item.egress,
        if item.loopback { "REACHABLE" } else { "unreachable" },
        item.af_unix_mediation
    )
}

fn doctor(deps: &Deps) -> Result<ActionOutput> {
    let version = deps.nono.version()?;
    let version_warning = nono_version_warning(version.version.as_deref());
    let agent = resolve_agent(&deps.config.config, &deps.plugin.env.plugin_root)?;
    let profile = resolve_profile(deps, &agent.profile_ref)?;
    let server_profile = match &agent.server_profile_ref {
        Some(reference) => Some(resolve_profile(deps, reference)?),
        None => None,
    };
    // The agent's tools run where its server runs.
    let (tools_ref, tools) = match (&agent.server_profile_ref, &server_profile) {
        (Some(reference), Some(summary)) => (reference.clone(), summary),
        _ => (agent.profile_ref.clone(), &profile),
    };
    let mut warnings: Vec<String> = Vec::new();
    warnings.extend(version_warning.clone());
    let command = agent.command.first().map_or("", String::as_str);
    let agent_binary = find_executable(command, &deps.env);
    if agent_binary.is_none() {
        warnings.push(format!(
            "The agent command \"{command}\" is not on the PATH Herdr gives plugins."
        ));
    }
    let mut checked: Vec<(&str, &ProfileSummary)> = vec![(agent.profile_ref.as_str(), &profile)];
    if let (Some(reference), Some(summary)) = (&agent.server_profile_ref, &server_profile) {
        checked.push((reference.as_str(), summary));
    }
    for (reference, item) in &checked {
        if item.af_unix_mediation != "pathname" {
            warnings.push(format!("Profile {reference} does not set linux.af_unix_mediation to \"pathname\", so the sandbox can connect to Herdr's control socket and other host sockets."));
        }
    }
    if server_profile.is_some() && profile.egress != "blocked" {
        warnings.push(format!("The client profile {} does not block the network; the client only needs its server's port.", agent.profile_ref));
    }
    if tools.egress == "open" {
        warnings.push(format!("Profile {tools_ref}, which the agent's tools run under, lets them connect to localhost services directly."));
    }
    for item in &tools.loopback_domains {
        warnings.push(format!(
            "Profile {tools_ref}, which the agent's tools run under, allows \"{}\", which {}; nono's proxy then forwards to localhost services.",
            item.domain, item.reason
        ));
    }
    let host_service: Option<HostService> = (agent.host_service.as_deref() == Some("opencode"))
        .then(|| detect_opencode_service(&deps.env, Path::new(crate::procfs::PROC_ROOT)));
    let service_reachable =
        host_service.as_ref().is_some_and(|service| service.running) && tools.loopback;
    let service_warning = host_service
        .as_ref()
        .filter(|_| service_reachable)
        .and_then(host_service_warning);
    warnings.extend(service_warning.clone());
    let mut probe_options = ProbeOptions::new(&deps.nono.bin, &tools_ref, &deps.env);
    probe_options.timeout = Duration::from_millis(NONO_CALL_TIMEOUT_MS);
    let (probes, probe_error): (Option<Vec<Check>>, Option<Value>) =
        match run_probes(&probe_options) {
            Ok(run) => (Some(run.checks), None),
            Err(error) => {
                warnings.push(format!("The escape probe could not run: {}", error.message));
                (
                    None,
                    Some(json!({"kind": error.kind_str(), "message": error.message})),
                )
            }
        };
    let failed_probes: Vec<&Check> = probes.iter().flatten().filter(|check| !check.ok).collect();
    for check in failed_probes
        .iter()
        .filter(|check| check.severity != "warning")
    {
        warnings.push(format!(
            "probe {}: {} ({})",
            check.check, check.result, check.why
        ));
    }
    let binary = std::env::current_exe()
        .map(|exe| exe.to_string_lossy().into_owned())
        .unwrap_or_default();
    let host_service_json = host_service.as_ref().map(|service| {
        let mut value = serde_json::to_value(service).unwrap_or(Value::Null);
        value["reachable"] = json!(service_reachable);
        value
    });
    let payload = json!({
        "nonoBin": deps.nono.bin,
        "nonoVersion": version.version.clone().unwrap_or_else(|| version.raw.clone()),
        "versionWarning": version_warning,
        "binary": binary,
        "binaryVersion": env!("CARGO_PKG_VERSION"),
        "pluginRoot": deps.plugin.env.plugin_root,
        "stateDir": deps.plugin.state_dir,
        "configDir": deps.plugin.config_dir,
        "agentKind": agent.kind,
        "agentBinary": agent_binary,
        "launchArgv": agent.launch_argv,
        "serverArgv": agent.server.as_ref().map(|server| &server.command),
        "profile": profile_json(&agent.profile_ref, &profile),
        "serverProfile": agent.server_profile_ref.as_ref().zip(server_profile.as_ref()).map(|(reference, summary)| profile_json(reference, summary)),
        "hostService": host_service_json,
        "probes": probes,
        "probeError": probe_error,
        "warnings": warnings,
    });
    let mut lines = vec![
        format!("nono executable: {} ({})", deps.nono.bin, version.raw),
        format!("binary: {binary} ({})", env!("CARGO_PKG_VERSION")),
        format!("plugin root: {}", deps.plugin.env.plugin_root),
        format!("state dir: {}", deps.plugin.state_dir),
        format!("config dir: {}", deps.plugin.config_dir),
        format!(
            "configured agent: {} ({}) at {}",
            agent.kind,
            agent.launch_argv.join(" "),
            agent_binary.as_ref().map_or_else(
                || "NOT FOUND".to_string(),
                |path| path.to_string_lossy().into_owned()
            )
        ),
        describe_profile(
            if server_profile.is_some() {
                "client profile"
            } else {
                "profile"
            },
            &agent.profile_ref,
            &profile,
        ),
    ];
    if let (Some(server), Some(summary), Some(reference)) =
        (&agent.server, &server_profile, &agent.server_profile_ref)
    {
        lines.push(format!("server: {}", server.command.join(" ")));
        lines.push(describe_profile("server profile", reference, summary));
    }
    if let Some(service) = host_service.as_ref().filter(|service| service.running) {
        let where_ = service.url.clone().unwrap_or_else(|| {
            format!(
                "port {}",
                service
                    .port
                    .map_or_else(|| "null".to_string(), |port| port.to_string())
            )
        });
        lines.push(format!(
            "OpenCode host service: pid {}, {where_}, {}",
            service
                .pid
                .map_or_else(|| "null".to_string(), |pid| pid.to_string()),
            if service_reachable {
                "REACHABLE from the agent's tools"
            } else {
                "not reachable from the agent's tools"
            }
        ));
    }
    lines.push(format!("escape probe with {tools_ref}:"));
    for check in probes.iter().flatten() {
        let mark = if check.ok {
            "ok  "
        } else if check.severity == "warning" {
            "warn"
        } else {
            "FAIL"
        };
        let why = if check.ok {
            String::new()
        } else {
            format!(" ({})", check.why)
        };
        lines.push(format!(
            "probe {mark} {}: {}{why}",
            check.check, check.result
        ));
    }
    for warning in &warnings {
        lines.push(format!("warning: {warning}"));
    }
    let refuse = deps.config.config.host_service_check == "refuse";
    let unconfined = |message: String| {
        PluginError::new(ErrorKind::Unconfined, message)
            .with_payload(payload.as_object().cloned().unwrap_or_default())
            .with_output(lines.join("\n"))
    };
    if service_warning.is_some() && refuse {
        let pid = host_service
            .as_ref()
            .and_then(|service| service.pid)
            .map_or_else(|| "undefined".to_string(), |pid| pid.to_string());
        return Err(unconfined(format!(
            "start-agent and reconnect will refuse to run: an OpenCode background service (pid {pid}) runs outside any sandbox and the agent's tools can reach it, so they could use it to run commands on the host. Stop it with \"opencode service stop\", and restrict network.allow_domain in the server profile to your provider's hosts."
        )));
    }
    if agent.host_service.as_deref() == Some("opencode") && tools.loopback && refuse {
        return Err(unconfined(format!(
            "start-agent and reconnect will refuse to run: the agent's tools can reach localhost under profile {tools_ref}, where an OpenCode host service can start at any time. Restrict network.allow_domain in the server profile to your provider's hosts."
        )));
    }
    let critical: Vec<String> = failed_probes
        .iter()
        .filter(|check| check.severity == "critical")
        .map(|check| format!("{} {}", check.check, check.result))
        .collect();
    if !critical.is_empty() {
        return Err(unconfined(format!(
            "The escape probe got through with profile {tools_ref}: {}.",
            critical.join(", ")
        )));
    }
    Ok(ActionOutput::new(payload).with_lines(lines))
}

fn install_keybindings(deps: &Deps) -> Result<ActionOutput> {
    let mut env = deps.env.clone();
    env.insert("HERDR_BIN_PATH".into(), deps.plugin.env.herdr_bin.clone());
    let root = Path::new(&deps.plugin.env.plugin_root);
    let options = RunOptions {
        env: Some(&env),
        cwd: Some(root),
        timeout: Duration::from_millis(KEYBINDING_INSTALL_TIMEOUT_MS),
        cancel: None,
    };
    let output = match run_cli("sh", &["scripts/install-keybindings.sh"], &options) {
        Ok(output) => output,
        Err(ExecError::TimedOut(partial)) => {
            return Err(PluginError::new(
                ErrorKind::Startup,
                format!(
                    "scripts/install-keybindings.sh did not finish within {}s (Herdr's config check or reload hung) and was killed.",
                    KEYBINDING_INSTALL_TIMEOUT_MS / 1000
                ),
            )
            .with_output(partial.output()));
        }
        Err(ExecError::Cancelled(partial)) => {
            return Err(PluginError::new(
                ErrorKind::Startup,
                "scripts/install-keybindings.sh was cancelled.",
            )
            .with_output(partial.output()))
        }
        Err(ExecError::Spawn(error)) => {
            return Err(PluginError::new(
                ErrorKind::Startup,
                format!("Could not run scripts/install-keybindings.sh: {error}"),
            ))
        }
    };
    let report = parse_keybinding_report(&output.stdout);
    if output.status != Some(0) {
        let restored = report
            .warnings
            .iter()
            .any(|warning| warning.contains("restored to its previous content"));
        let status = output
            .status
            .map_or_else(|| "null".to_string(), |code| code.to_string());
        return Err(PluginError::new(
            ErrorKind::Config,
            format!(
                "scripts/install-keybindings.sh exited with status {status}; {} {} and Herdr was not reloaded.",
                report.config_path.as_deref().unwrap_or("the Herdr config"),
                if restored { "was restored to its previous content" } else { "was left as it was" }
            ),
        )
        .with_output(output.output()));
    }
    let lines = output
        .output()
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    Ok(ActionOutput::new(serde_json::to_value(&report).unwrap_or(Value::Null)).with_lines(lines))
}

fn start_agent(deps: &Deps) -> Result<ActionOutput> {
    let Some(root) = resolve_workspace_root(&deps.context, &|dir| git_toplevel(dir)) else {
        return Err(target_error("The invocation context has no workspace or pane directory. Invoke start-agent from a workspace or pane.".into()));
    };
    let local_path = assert_workspace_root(&root.to_string_lossy())?;
    let workdir = resolve_workdir(&deps.context, &local_path);
    let source_pane_id = resolve_pane_id(&deps.context, &deps.env);
    let agent = resolve_agent(&deps.config.config, &deps.plugin.env.plugin_root)?;
    // Fail before touching Herdr when nono is missing.
    deps.nono.version()?;
    let local_text = local_path.to_string_lossy().into_owned();
    let workdir_text = workdir.to_string_lossy().into_owned();
    let session_name = session_name_for(
        &deps.config.config.session_name_prefix,
        &agent.kind,
        &local_text,
        source_pane_id.as_deref(),
    )?;
    let pane_id = open_agent_pane(
        deps,
        source_pane_id.as_deref(),
        Some(&workdir_text),
        &pane_label(&agent.kind, &session_name),
    )?;
    let entry = fresh_entry(
        &session_name,
        &agent,
        &local_text,
        &workdir_text,
        source_pane_id.as_deref(),
        deps.workspace_id().as_deref(),
    );
    with_pane_lock(deps.state_dir(), &pane_id, || {
        if let Some(stale) = get_pane_entry(deps.state_dir(), Some(&pane_id))? {
            if bridge_is_running(&stale) || shell_is_running(&stale) {
                let bridge = stale
                    .get("bridgePid")
                    .filter(|pid| !pid.is_null())
                    .map_or_else(|| "none".to_string(), |pid| show(Some(pid)));
                return Err(PluginError::new(
                    ErrorKind::Conflict,
                    format!("Herdr handed out pane {pane_id}, but its mapping to {} is still in use (bridge {bridge}). Try again in a moment.", entry_str(&stale, "sessionName")),
                ));
            }
            eprintln!(
                "pane {pane_id} was previously mapped to {}; replacing that mapping",
                entry_str(&stale, "sessionName")
            );
        }
        save_pane_entry(deps.state_dir(), &pane_id, &entry).map(drop)
    })?;
    deps.herdr.run_in_pane(
        &pane_id,
        &bridge_command(
            deps,
            "start",
            &pane_id,
            agent.herdr_detection_kind.as_deref(),
            None,
        )?,
    )?;
    deps.herdr.notify(
        "nono sandbox starting",
        &format!("{} in {session_name}", agent.title),
    );
    Ok(ActionOutput::new(json!({
        "paneId": pane_id, "sourcePaneId": source_pane_id, "sessionName": session_name, "agentKind": agent.kind,
        "localPath": local_text, "workdir": workdir_text, "profile": agent.profile_ref, "serverProfile": agent.server_profile_ref,
        "launchArgv": agent.launch_argv, "serverArgv": agent.server.as_ref().map(|server| &server.command), "openIn": deps.config.config.open_in,
    })))
}

fn reconnect(deps: &Deps) -> Result<ActionOutput> {
    let target = require_focused_mapping(deps)?;
    let entry = target.entry.clone();
    // An open shell is a separate sandbox; it does not stop the agent from coming back.
    refuse_while_agent_runs(deps, &target, "reconnect", false)?;
    let agent = agent_for_entry(&deps.config.config, &entry, &deps.plugin.env.plugin_root)?;
    let session_name = entry_str(&entry, "sessionName").to_string();
    let label = pane_label(&agent.kind, &session_name);
    let pane_id = if target.orphan {
        match rehome_orphan(deps, &target, &label, None)? {
            Some(fresh) => fresh,
            None => {
                return Err(PluginError::new(
                    ErrorKind::Conflict,
                    format!(
                        "The mapping of pane {} is in use again; nothing was started.",
                        target.pane_id
                    ),
                ))
            }
        }
    } else {
        target.pane_id.clone()
    };
    // Resume arguments only make sense once the agent has run in this pane.
    let launched = entry
        .get("launchCount")
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
        > 0.0;
    let mode = if launched { "connect" } else { "start" };
    let started = start_bridge(deps, &pane_id, mode, &agent, &label)?;
    Ok(ActionOutput::new(json!({
        "paneId": started.pane_id, "sessionName": session_name, "agentKind": entry.get("agentKind"), "mode": mode,
        "argv": if launched { &agent.resume_argv } else { &agent.launch_argv },
        "adoptedFrom": if target.orphan { json!(entry.get("paneId")) } else { Value::Null }, "movedTo": started.moved_to,
    })))
}

fn open_shell(deps: &Deps) -> Result<ActionOutput> {
    let target = require_focused_mapping(deps)?;
    let session_name = entry_str(&target.entry, "sessionName").to_string();
    // The shell opens below the pane the user is in, wherever the agent pane lives.
    let anchor =
        resolve_pane_id(&deps.context, &deps.env).unwrap_or_else(|| target.pane_id.clone());
    let shell_pane = deps.herdr.split_pane(&SplitPane {
        pane_id: Some(&anchor),
        direction: "down",
        ratio: 0.5,
        cwd: entry_cwd(&target.entry).as_deref(),
        focus: true,
    })?;
    deps.herdr.rename_pane(
        &shell_pane,
        &format!("nono shell {}", short_id(&session_name)),
    )?;
    deps.herdr.run_in_pane(
        &shell_pane,
        &bridge_command(deps, "shell", &target.pane_id, None, None)?,
    )?;
    Ok(ActionOutput::new(
        json!({"paneId": shell_pane, "mappedPaneId": target.pane_id, "sessionName": session_name}),
    ))
}

fn stop(deps: &Deps) -> Result<ActionOutput> {
    let target = require_focused_mapping(deps)?;
    let outcome = deps.lifecycle.stop(&target.pane_id, false)?;
    deps.herdr
        .notify("nono sandbox stopped", &outcome.session_name);
    Ok(ActionOutput::new(json!({
        "paneId": target.pane_id, "sessionName": outcome.session_name, "sessionId": outcome.session_id, "serverSessionIds": outcome.server_session_ids,
    })))
}

fn info(deps: &Deps) -> Result<ActionOutput> {
    let target = require_focused_mapping(deps)?;
    let description =
        serde_json::to_value(deps.lifecycle.describe(&target.pane_id)?).unwrap_or(Value::Null);
    let lines = vec![serde_json::to_string_pretty(&description).unwrap_or_default()];
    let mut payload = Map::new();
    payload.insert("paneId".into(), json!(target.pane_id));
    if let Value::Object(fields) = description {
        payload.extend(fields);
    }
    Ok(ActionOutput { payload, lines })
}

fn verify_sandbox(deps: &Deps) -> Result<ActionOutput> {
    let target = require_focused_mapping(deps)?;
    let outcome = deps.lifecycle.verify(&target.pane_id)?;
    let report = outcome.report;
    let session_name = outcome.entry.as_ref().map_or_else(String::new, |entry| {
        entry_str(entry, "sessionName").to_string()
    });
    let report_json = serde_json::to_value(&report).unwrap_or(Value::Null);
    let mut lines = vec![format!(
        "{session_name}: {}",
        summarize_verification(Some(&report_json))
    )];
    for item in &report.processes {
        lines.push(format!(
            "  {} {:>7} {:<6} {:<6} {}",
            if item.confined {
                "confined  "
            } else {
                "UNCONFINED"
            },
            item.pid,
            item.sandbox,
            item.role,
            item.command
        ));
    }
    lines.extend(
        report
            .problems
            .iter()
            .map(|problem| format!("problem: {problem}")),
    );
    lines.extend(
        report
            .warnings
            .iter()
            .map(|warning| format!("warning: {warning}")),
    );
    let payload = json!({"paneId": target.pane_id, "sessionName": session_name, "verified": report.ok, "report": report_json});
    if report.ok == Some(false) {
        return Err(PluginError::new(
            ErrorKind::Unconfined,
            format!(
                "Sandbox verification failed for {session_name}: {}",
                report.problems.join(" ")
            ),
        )
        .with_payload(payload.as_object().cloned().unwrap_or_default())
        .with_output(lines.join("\n")));
    }
    Ok(ActionOutput::new(payload).with_lines(lines))
}

fn prune_mappings(deps: &Deps) -> Result<ActionOutput> {
    let pane_ids = deps.herdr.list_pane_ids().map_err(|error| {
        let mut wrapped = PluginError::new(
            error.kind,
            format!("Cannot prune without Herdr's pane list: {}", error.message),
        )
        .with_output(error.output.clone());
        wrapped.cause = None;
        wrapped
    })?;
    let outcome = deps.lifecycle.prune(&pane_ids)?;
    let mut lines = vec![format!(
        "pruned {} mapping{}",
        outcome.pruned.len(),
        if outcome.pruned.len() == 1 { "" } else { "s" }
    )];
    lines.extend(
        outcome
            .pruned
            .iter()
            .map(|item| format!("  {}\t{}", item.pane_id, item.session_name)),
    );
    lines.extend(outcome.kept.iter().map(|item| {
        format!(
            "kept {}\t{}\t{}",
            item.pane_id, item.session_name, item.reason
        )
    }));
    Ok(ActionOutput::new(serde_json::to_value(&outcome).unwrap_or(Value::Null)).with_lines(lines))
}

fn sandboxes(deps: &Deps) -> Result<ActionOutput> {
    deps.herdr
        .open_plugin_pane(&deps.plugin.env.plugin_id, "sandboxes", &[], true)?;
    Ok(ActionOutput::new(json!({"entrypoint": "sandboxes"})))
}

fn list_sandboxes(deps: &Deps) -> Result<ActionOutput> {
    let outcome = deps.lifecycle.list_all()?;
    let (pane_ids, pane_error) = match deps.herdr.list_pane_ids() {
        Ok(ids) => (Some(ids), None),
        Err(error) => (None, Some(error.message.clone())),
    };
    let mappings: Vec<Value> = outcome
        .mappings
        .into_iter()
        .map(|mut item| {
            let exists = pane_ids.as_ref().map(|ids| {
                ids.iter()
                    .any(|id| Some(id.as_str()) == item.get("paneId").and_then(Value::as_str))
            });
            item["paneExists"] = json!(exists);
            item["paneError"] = json!(pane_error);
            item
        })
        .collect();
    let mut lines: Vec<String> = if mappings.is_empty() {
        vec!["No sandboxed agents are mapped.".to_string()]
    } else {
        mappings
            .iter()
            .map(|item| {
                let field = |key: &str| show(item.get(key));
                let pane_note = match item["paneExists"].as_bool() {
                    Some(false) => " (pane gone)",
                    None => " (pane ?)",
                    Some(true) => "",
                };
                let running = match item["running"].as_bool() {
                    None => "unknown",
                    Some(true) => "running",
                    Some(false) => "not running",
                };
                let verification = item
                    .get("verification")
                    .and_then(Value::as_str)
                    .unwrap_or("not verified");
                format!(
                    "{}{pane_note}\t{}\t{}\t{}\t{running}\t{verification}\t{}",
                    field("paneId"),
                    field("sessionName"),
                    field("agentKind"),
                    field("lifecycleState"),
                    field("localPath")
                )
            })
            .collect()
    };
    if let Some(error) = outcome.session_error.as_ref() {
        lines.push(format!(
            "nono ps failed ({}): {}",
            show(error.get("kind")),
            show(error.get("message"))
        ));
    }
    Ok(
        ActionOutput::new(json!({"mappings": mappings, "sessionError": outcome.session_error}))
            .with_lines(lines),
    )
}

fn forget_mapping(deps: &Deps) -> Result<ActionOutput> {
    let target = require_focused_mapping(deps)?;
    refuse_while_agent_runs(deps, &target, "forget the mapping", true)?;
    let removed = with_pane_lock(deps.state_dir(), &target.pane_id, || {
        if let Some(now) = get_pane_entry(deps.state_dir(), Some(&target.pane_id))? {
            if bridge_is_running(&now) || shell_is_running(&now) {
                return Err(PluginError::new(
                    ErrorKind::Conflict,
                    format!(
                        "Pane {} started {} again meanwhile; nothing was forgotten.",
                        target.pane_id,
                        entry_str(&now, "sessionName")
                    ),
                ));
            }
        }
        delete_pane_entry(deps.state_dir(), &target.pane_id)
    })?;
    Ok(ActionOutput::new(
        json!({"paneId": target.pane_id, "sessionName": entry_str(&target.entry, "sessionName"), "removed": removed}),
    ))
}

fn dispatch(action: &str, deps: &Deps) -> Result<ActionOutput> {
    match action {
        "doctor" => doctor(deps),
        "install-keybindings" => install_keybindings(deps),
        "start-agent" => start_agent(deps),
        "reconnect" => reconnect(deps),
        "open-shell" => open_shell(deps),
        "stop" => stop(deps),
        "info" => info(deps),
        "verify-sandbox" => verify_sandbox(deps),
        "prune-mappings" => prune_mappings(deps),
        "sandboxes" => sandboxes(deps),
        "list-sandboxes" => list_sandboxes(deps),
        "forget-mapping" => forget_mapping(deps),
        other => Err(target_error(format!(
            "Unknown action \"{other}\". Known actions: {}.",
            ACTION_IDS.join(", ")
        ))),
    }
}

fn run(action: &str, env: &Env) -> Result<ActionOutput> {
    let plugin = require_plugin_dirs(read_plugin_env(env))?;
    if !ACTION_IDS.contains(&action) {
        return Err(target_error(format!(
            "Unknown action \"{action}\". Known actions: {}.",
            ACTION_IDS.join(", ")
        )));
    }
    let context = read_context(env)?;
    let config = load_config(Path::new(&plugin.config_dir), env)?;
    let nono = NonoClient::new(config.nono_bin.clone(), env.clone());
    let herdr = HerdrClient::new(plugin.env.herdr_bin.clone(), env.clone());
    let mut options = LifecycleOptions::new(
        &plugin.state_dir,
        config.config.clone(),
        plugin.env.plugin_root.clone(),
        nono.clone(),
        env.clone(),
    );
    options.herdr = Some(herdr.clone());
    let lifecycle = Lifecycle::new(options);
    let deps = Deps {
        env: env.clone(),
        plugin,
        context,
        config,
        nono,
        herdr,
        lifecycle,
    };
    dispatch(action, &deps)
}

/// Runs the action named by HERDR_PLUGIN_ACTION_ID and returns the exit code.
pub fn main(env: &Env) -> i32 {
    let action = read_plugin_env(env).action_id;
    let name = action.clone().unwrap_or_else(|| "null".to_string());
    match run(&name, env) {
        Ok(output) => {
            let mut payload = Map::new();
            payload.insert("action".into(), json!(action));
            payload.insert("ok".into(), json!(true));
            payload.extend(output.payload);
            emit_result(&payload, &output.lines);
            0
        }
        Err(error) => {
            emit_result(&failure_payload(action.as_deref(), &error), &[]);
            eprintln!("{}", error.message);
            if !error.output.trim().is_empty() {
                eprintln!("{}", error.output.trim());
            }
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_helpers() {
        let env =
            |value: &str| Env::from([(BRIDGE_START_TIMEOUT_ENV.to_string(), value.to_string())]);
        assert_eq!(bridge_start_timeout(&Env::new()), 4000);
        assert_eq!(bridge_start_timeout(&env("25")), 25);
        assert_eq!(bridge_start_timeout(&env("-1")), 4000);
        assert_eq!(bridge_start_timeout(&env("")), 4000);
        let entry = |value: Value| value.as_object().cloned().unwrap();
        assert_eq!(
            entry_cwd(&entry(
                json!({"workdir": "/definitely/gone", "localPath": "/"})
            ))
            .as_deref(),
            Some("/")
        );
        assert_eq!(
            entry_cwd(&entry(
                json!({"workdir": "/gone", "localPath": "/also/gone"})
            )),
            None
        );
        assert_eq!(
            entry_cwd(&entry(json!({"workdir": "/tmp", "localPath": "/"}))).as_deref(),
            Some("/tmp")
        );
        assert_eq!(nono_version_warning(Some("0.78.0")), None);
        assert_eq!(nono_version_warning(Some("1.0.0")), None);
        assert_eq!(nono_version_warning(None), None);
        assert_eq!(nono_version_warning(Some("junk")), None);
        assert_eq!(
            nono_version_warning(Some("0.70.1")).unwrap(),
            "nono 0.70.1 is older than 0.78.0; the plugin was written against the newer CLI and some flags may be missing."
        );
        assert!(nono_version_warning(Some("0.77.9"))
            .unwrap()
            .contains("older"));
        assert_eq!(parse_version("nono 0.78.0 (abc)"), Some([0, 78, 0]));
        assert_eq!(
            pane_label("opencode", "herdr-opencode-abc123def456"),
            "nono opencode abc123"
        );
        assert_eq!(short_id("s-1"), "1");
    }

    #[test]
    fn the_keybinding_report_is_parsed_line_by_line() {
        let report = parse_keybinding_report(
            "config: /c\nbound prefix+shift+a -> nono.sandbox.start-agent\nalready bound: nono.sandbox.reconnect (prefix+shift+b)\nwarning: w\nreloaded\nnoise\n",
        );
        assert_eq!(
            report,
            KeybindingReport {
                config_path: Some("/c".into()),
                added: vec![Binding {
                    key: "prefix+shift+a".into(),
                    action: "start-agent".into()
                }],
                existing: vec![Binding {
                    key: "prefix+shift+b".into(),
                    action: "reconnect".into()
                }],
                warnings: vec!["w".into()],
                reloaded: true,
            }
        );
        assert_eq!(
            serde_json::to_value(parse_keybinding_report("")).unwrap(),
            json!({"configPath": null, "added": [], "existing": [], "warnings": [], "reloaded": false})
        );
    }

    #[test]
    fn an_unknown_action_and_missing_directories_are_reported() {
        let env = Env::from([
            ("HERDR_PLUGIN_STATE_DIR".to_string(), "/s".to_string()),
            ("HERDR_PLUGIN_CONFIG_DIR".to_string(), "/c".to_string()),
        ]);
        let error = run("fetch-changes", &env).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Target);
        assert!(
            error.message.starts_with(
                "Unknown action \"fetch-changes\". Known actions: doctor, install-keybindings,"
            ),
            "{}",
            error.message
        );
        let no_dirs = run("doctor", &Env::new()).err().unwrap();
        assert_eq!(no_dirs.kind, ErrorKind::Startup);
        assert!(no_dirs.message.contains("HERDR_PLUGIN_STATE_DIR"));
    }
}

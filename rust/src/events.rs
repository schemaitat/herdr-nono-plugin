//! Event hook for `worktree.removed`: stops the sandboxed agents that were
//! started for the removed worktree and forgets their mappings, unless the
//! user set `cleanupOnWorktreeRemoved` to false. Nothing is deleted: a nono
//! sandbox keeps no state of its own, and the agent's conversation stays in
//! the agent's own storage.

use std::io::Write;
use std::path::Path;

use serde_json::{Map, Value};

use crate::config::load_config;
use crate::context::{read_event_payload, read_plugin_env, Env};
use crate::errors::Result;
use crate::herdr::HerdrClient;
use crate::lifecycle::{bridge_is_running, shell_is_running, Lifecycle, LifecycleOptions};
use crate::nono::NonoClient;
use crate::state::{
    delete_pane_entry, entries_for_local_path, get_pane_entry, load_state, with_pane_lock,
};

/// Extracts the removed worktree path from the event payload.
pub fn removed_worktree_path(payload: &Map<String, Value>) -> Option<String> {
    let path_at = |keys: &[&str]| -> Option<String> {
        let mut current = payload.get(*keys.first()?)?;
        for key in &keys[1..] {
            current = current.get(*key)?;
        }
        current.as_str().map(str::to_string)
    };
    path_at(&["data", "worktree", "path"]).or_else(|| path_at(&["worktree", "path"]))
}

fn print_line(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn handle_event_unsafe(env: &Env) -> Result<i32> {
    let plugin_env = read_plugin_env(env);
    if plugin_env.event_name.as_deref() != Some("worktree.removed") {
        print_line(&format!(
            "ignoring event {}",
            plugin_env.event_name.as_deref().unwrap_or("(none)")
        ));
        return Ok(0);
    }
    let (Some(state_dir), Some(config_dir)) = (
        plugin_env.state_dir.clone().filter(|dir| !dir.is_empty()),
        plugin_env.config_dir.clone().filter(|dir| !dir.is_empty()),
    ) else {
        print_line("no plugin state directory; nothing to clean up");
        return Ok(0);
    };
    let loaded = load_config(Path::new(&config_dir), env)?;
    if !loaded.config.cleanup_on_worktree_removed {
        print_line("cleanupOnWorktreeRemoved is false; leaving the mappings alone");
        return Ok(0);
    }
    let Some(removed_path) =
        removed_worktree_path(&read_event_payload(env)?).filter(|path| !path.is_empty())
    else {
        print_line("event carries no worktree path; nothing to clean up");
        return Ok(0);
    };
    let state_dir = Path::new(&state_dir);
    let state = load_state(state_dir)?;
    let matches: Vec<(String, crate::state::Entry)> =
        entries_for_local_path(&state, Path::new(&removed_path))
            .into_iter()
            .map(|(pane_id, entry)| (pane_id.to_string(), entry.clone()))
            .collect();
    if matches.is_empty() {
        print_line(&format!("no sandboxed agent mapped to {removed_path}"));
        return Ok(0);
    }
    let herdr = HerdrClient::new(plugin_env.herdr_bin.clone(), env.clone());
    let nono = NonoClient::new(loaded.nono_bin.clone(), env.clone());
    let mut options = LifecycleOptions::new(
        state_dir,
        loaded.config,
        plugin_env.plugin_root.clone(),
        nono,
        env.clone(),
    );
    options.log = Box::new(print_line);
    options.herdr = Some(herdr.clone());
    let lifecycle = Lifecycle::new(options);
    let mut failures = 0;
    let mut cleaned = Vec::new();
    for (pane_id, entry) in &matches {
        let session_name = entry
            .get("sessionName")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let outcome = (|| -> Result<()> {
            if bridge_is_running(entry) {
                // The agent's working tree is gone; its session has nothing left to work on.
                lifecycle.stop(pane_id, false)?;
                print_line(&format!("stopped {session_name}"));
            }
            let removed = with_pane_lock(state_dir, pane_id, || {
                if let Some(now) = get_pane_entry(state_dir, Some(pane_id))? {
                    if bridge_is_running(&now) || shell_is_running(&now) {
                        return Ok(false);
                    }
                }
                delete_pane_entry(state_dir, pane_id)
            })?;
            if removed {
                cleaned.push(session_name.to_string());
                print_line(&format!(
                    "forgot mapping for pane {pane_id} ({session_name})"
                ));
            } else {
                print_line(&format!("kept mapping for pane {pane_id} ({session_name}): its agent or a shell still runs"));
            }
            Ok(())
        })();
        if let Err(error) = outcome {
            failures += 1;
            eprintln!(
                "could not clean up {session_name} for pane {pane_id}: {}",
                error.message
            );
        }
    }
    if !cleaned.is_empty() {
        herdr.notify(
            "nono sandboxes cleaned up",
            &format!(
                "{} (worktree {removed_path} was removed)",
                cleaned.join(", ")
            ),
        );
    }
    Ok(if failures == 0 { 0 } else { 1 })
}

/// Handles the event and returns the exit code. Any unexpected failure is
/// reported as one line on stderr instead of a stack trace.
pub fn handle_event(env: &Env) -> i32 {
    match handle_event_unsafe(env) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("worktree cleanup failed: {}", error.message);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn removed_worktree_path_reads_herdrs_payload_shape() {
        assert_eq!(
            removed_worktree_path(&payload(json!({"data": {"worktree": {"path": "/w"}}})))
                .as_deref(),
            Some("/w")
        );
        assert_eq!(
            removed_worktree_path(&payload(json!({"worktree": {"path": "/v"}}))).as_deref(),
            Some("/v")
        );
        assert_eq!(removed_worktree_path(&payload(json!({}))), None);
        assert_eq!(
            removed_worktree_path(&payload(json!({"data": {"worktree": {"path": 5}}}))),
            None
        );
    }
}

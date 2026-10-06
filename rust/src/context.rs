//! Access to the environment and JSON context Herdr injects into plugin
//! commands, plus the rules that turn that context into the directory a
//! sandbox may write (the workspace root).

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value};

use crate::constants::PLUGIN_ID;
use crate::errors::{ErrorKind, PluginError, Result};

/// An environment as a plain map, so tests can pass their own.
pub type Env = HashMap<String, String>;

/// The current process environment (non-UTF-8 entries are dropped).
pub fn process_env() -> Env {
    std::env::vars().collect()
}

/// The Herdr-injected plugin environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginEnv {
    pub action_id: Option<String>,
    pub event_name: Option<String>,
    pub plugin_id: String,
    pub plugin_root: String,
    pub state_dir: Option<String>,
    pub config_dir: Option<String>,
    pub herdr_bin: String,
}

/// A [`PluginEnv`] whose state and config directories are known to be set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDirs {
    pub env: PluginEnv,
    pub state_dir: String,
    pub config_dir: String,
}

fn current_dir_string() -> String {
    std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_string())
}

/// Reads the Herdr-injected plugin environment.
pub fn read_plugin_env(env: &Env) -> PluginEnv {
    let get = |name: &str| env.get(name).cloned();
    PluginEnv {
        action_id: get("HERDR_PLUGIN_ACTION_ID"),
        event_name: get("HERDR_PLUGIN_EVENT"),
        plugin_id: get("HERDR_PLUGIN_ID").unwrap_or_else(|| PLUGIN_ID.to_string()),
        plugin_root: get("HERDR_PLUGIN_ROOT").unwrap_or_else(current_dir_string),
        state_dir: get("HERDR_PLUGIN_STATE_DIR"),
        config_dir: get("HERDR_PLUGIN_CONFIG_DIR"),
        herdr_bin: get("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".to_string()),
    }
}

/// Ensures the state and config directories Herdr promises are present.
pub fn require_plugin_dirs(plugin_env: PluginEnv) -> Result<PluginDirs> {
    // JS treats an empty value like a missing one (`!pluginEnv.stateDir`).
    let state_dir = plugin_env
        .state_dir
        .clone()
        .filter(|dir| !dir.is_empty())
        .ok_or_else(|| {
            PluginError::new(
                ErrorKind::Startup,
                "HERDR_PLUGIN_STATE_DIR is not set. Run this command through Herdr.",
            )
        })?;
    let config_dir = plugin_env
        .config_dir
        .clone()
        .filter(|dir| !dir.is_empty())
        .ok_or_else(|| {
            PluginError::new(
                ErrorKind::Startup,
                "HERDR_PLUGIN_CONFIG_DIR is not set. Run this command through Herdr.",
            )
        })?;
    Ok(PluginDirs {
        env: plugin_env,
        state_dir,
        config_dir,
    })
}

fn read_json_env(env: &Env, name: &str) -> Result<Map<String, Value>> {
    let Some(raw) = env.get(name).filter(|raw| !raw.is_empty()) else {
        return Ok(Map::new());
    };
    let parsed: Value = serde_json::from_str(raw).map_err(|error| {
        PluginError::new(
            ErrorKind::Startup,
            format!("{name} is not valid JSON: {error}"),
        )
        .with_cause(error)
    })?;
    Ok(match parsed {
        Value::Object(map) => map,
        _ => Map::new(),
    })
}

/// Parses HERDR_PLUGIN_CONTEXT_JSON.
pub fn read_context(env: &Env) -> Result<Map<String, Value>> {
    read_json_env(env, "HERDR_PLUGIN_CONTEXT_JSON")
}

/// Parses HERDR_PLUGIN_EVENT_JSON for event hooks.
pub fn read_event_payload(env: &Env) -> Result<Map<String, Value>> {
    read_json_env(env, "HERDR_PLUGIN_EVENT_JSON")
}

/// Picks the pane an action was invoked for.
pub fn resolve_pane_id(context: &Map<String, Value>, env: &Env) -> Option<String> {
    context
        .get("focused_pane_id")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string)
        })
        .or_else(|| env.get("HERDR_PANE_ID").cloned())
}

/// `path.resolve`: absolute and lexically normalised, symlinks untouched.
fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut resolved = PathBuf::from("/");
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => resolved.push(name),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    resolved
}

/// Canonical absolute form of a path: symlinks resolved when the path exists,
/// plain resolution otherwise. A path that is gone (a removed worktree) or not
/// there yet is canonicalised through its longest ancestor that exists, so both
/// spellings of a symlinked prefix agree.
pub fn canonical_path(value: impl AsRef<Path>) -> PathBuf {
    let resolved = resolve(value.as_ref());
    if let Ok(real) = std::fs::canonicalize(&resolved) {
        return real;
    }
    match (resolved.parent(), resolved.file_name()) {
        (Some(parent), Some(name)) => canonical_path(parent).join(name),
        _ => resolved,
    }
}

/// Whether `candidate` is `root` or lies inside it, comparing canonical paths.
pub fn is_inside(root: impl AsRef<Path>, candidate: impl AsRef<Path>) -> bool {
    // Component-wise, so a directory named `..cache` is inside and `/repo-other` is not.
    canonical_path(candidate).starts_with(canonical_path(root))
}

/// Returns the git top-level directory containing `dir`, or `None`.
pub fn git_toplevel(dir: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", dir, "rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let top = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!top.is_empty()).then_some(top)
}

fn non_empty_string<'a>(context: &'a Map<String, Value>, path: &[&str]) -> Option<&'a str> {
    let mut current = context.get(*path.first()?)?;
    for key in &path[1..] {
        current = current.get(*key)?;
    }
    current.as_str().filter(|text| !text.is_empty())
}

/// Picks the directory the sandbox grants read-write (the workspace root).
/// Order: the workspace's worktree checkout, the workspace directory, the git
/// top level of the focused pane's directory, then that directory itself. The
/// pane's own directory comes last on purpose: it changes with every `cd`, and
/// granting only a subdirectory would hide `.git` from the agent.
pub fn resolve_workspace_root(
    context: &Map<String, Value>,
    toplevel: &dyn Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    if let Some(checkout) = non_empty_string(context, &["worktree", "checkout_path"]) {
        return Some(canonical_path(checkout));
    }
    if let Some(workspace) = non_empty_string(context, &["workspace_cwd"]) {
        return Some(canonical_path(workspace));
    }
    let pane_cwd = non_empty_string(context, &["focused_pane_cwd"])?;
    Some(canonical_path(
        toplevel(pane_cwd).as_deref().unwrap_or(pane_cwd),
    ))
}

/// Picks the directory the agent starts in: the focused pane's directory when
/// it lies inside the workspace root, otherwise the root itself.
pub fn resolve_workdir(context: &Map<String, Value>, workspace_root: &Path) -> PathBuf {
    match non_empty_string(context, &["focused_pane_cwd"]) {
        Some(pane_cwd) if is_inside(workspace_root, pane_cwd) => canonical_path(pane_cwd),
        _ => canonical_path(workspace_root),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::symlink;

    fn context(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    fn no_repo(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn plugin_env_defaults_and_overrides() {
        let mut env = Env::new();
        let defaults = read_plugin_env(&env);
        assert_eq!(defaults.plugin_id, "nono.sandbox");
        assert_eq!(defaults.herdr_bin, "herdr");
        assert_eq!(defaults.state_dir, None);
        env.insert("HERDR_PLUGIN_ID".into(), "other".into());
        env.insert("HERDR_PLUGIN_ROOT".into(), "/plugin".into());
        env.insert("HERDR_PLUGIN_STATE_DIR".into(), "/state".into());
        env.insert("HERDR_PLUGIN_CONFIG_DIR".into(), "/config".into());
        env.insert("HERDR_BIN_PATH".into(), "/bin/herdr".into());
        env.insert("HERDR_PLUGIN_ACTION_ID".into(), "info".into());
        let read = read_plugin_env(&env);
        assert_eq!(
            (
                read.plugin_id.as_str(),
                read.plugin_root.as_str(),
                read.herdr_bin.as_str()
            ),
            ("other", "/plugin", "/bin/herdr")
        );
        let dirs = require_plugin_dirs(read).unwrap();
        assert_eq!(
            (dirs.state_dir.as_str(), dirs.config_dir.as_str()),
            ("/state", "/config")
        );
    }

    #[test]
    fn require_plugin_dirs_names_the_missing_variable() {
        let error = require_plugin_dirs(read_plugin_env(&Env::new())).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Startup);
        assert!(error.message.contains("HERDR_PLUGIN_STATE_DIR"));
        let mut env = Env::new();
        env.insert("HERDR_PLUGIN_STATE_DIR".into(), "/s".into());
        assert!(require_plugin_dirs(read_plugin_env(&env))
            .unwrap_err()
            .message
            .contains("HERDR_PLUGIN_CONFIG_DIR"));
    }

    #[test]
    fn context_json_is_parsed_and_bad_json_is_a_startup_error() {
        let mut env = Env::new();
        assert!(read_context(&env).unwrap().is_empty());
        env.insert(
            "HERDR_PLUGIN_CONTEXT_JSON".into(),
            r#"{"focused_pane_id":"p1"}"#.into(),
        );
        assert_eq!(
            resolve_pane_id(&read_context(&env).unwrap(), &env).as_deref(),
            Some("p1")
        );
        env.insert("HERDR_PLUGIN_CONTEXT_JSON".into(), "[1]".into());
        assert!(
            read_context(&env).unwrap().is_empty(),
            "a non-object is treated as empty"
        );
        env.insert("HERDR_PLUGIN_CONTEXT_JSON".into(), "{nope".into());
        let error = read_context(&env).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Startup);
        assert!(error
            .message
            .starts_with("HERDR_PLUGIN_CONTEXT_JSON is not valid JSON: "));
        env.insert("HERDR_PLUGIN_EVENT_JSON".into(), r#"{"a":1}"#.into());
        assert_eq!(read_event_payload(&env).unwrap()["a"], json!(1));
    }

    #[test]
    fn pane_id_falls_back_to_the_environment() {
        let mut env = Env::new();
        assert_eq!(resolve_pane_id(&Map::new(), &env), None);
        env.insert("HERDR_PANE_ID".into(), "from-env".into());
        assert_eq!(
            resolve_pane_id(&Map::new(), &env).as_deref(),
            Some("from-env")
        );
        assert_eq!(
            resolve_pane_id(&context(json!({"focused_pane_id": null})), &env).as_deref(),
            Some("from-env")
        );
        assert_eq!(
            resolve_pane_id(&context(json!({"focused_pane_id": "ctx"})), &env).as_deref(),
            Some("ctx")
        );
    }

    #[test]
    fn is_inside_treats_the_root_itself_and_descendants_as_inside() {
        assert!(is_inside("/repo", "/repo"));
        assert!(is_inside("/repo", "/repo/src/x"));
        assert!(!is_inside("/repo", "/repo-other"));
        assert!(
            is_inside("/repo", "/repo/..cache"),
            "a directory name starting with dots is not a traversal"
        );
        assert!(!is_inside("/repo", "/repo/../elsewhere"));
        assert!(!is_inside("/repo", "/"));
        assert!(!is_inside("/repo/src", "/repo"));
    }

    #[test]
    fn resolve_workspace_root_prefers_the_worktree_checkout_then_the_workspace_then_the_pane() {
        let root = |value: Value, toplevel: &dyn Fn(&str) -> Option<String>| {
            resolve_workspace_root(&context(value), toplevel)
        };
        assert_eq!(
            root(
                json!({"worktree": {"checkout_path": "/wt"}, "workspace_cwd": "/ws", "focused_pane_cwd": "/ws/src"}),
                &no_repo
            ),
            Some(PathBuf::from("/wt"))
        );
        assert_eq!(
            root(
                json!({"workspace_cwd": "/ws", "focused_pane_cwd": "/elsewhere"}),
                &no_repo
            ),
            Some(PathBuf::from("/ws"))
        );
        assert_eq!(
            root(json!({"focused_pane_cwd": "/repo/src"}), &|_| Some(
                "/repo".into()
            )),
            Some(PathBuf::from("/repo"))
        );
        assert_eq!(
            root(json!({"focused_pane_cwd": "/scratch"}), &no_repo),
            Some(PathBuf::from("/scratch"))
        );
        assert_eq!(
            root(
                json!({"worktree": {"checkout_path": ""}, "workspace_cwd": ""}),
                &no_repo
            ),
            None
        );
        assert_eq!(root(json!({}), &no_repo), None);
    }

    #[test]
    fn resolve_workdir_keeps_the_pane_directory_only_when_it_lies_inside_the_root() {
        let workdir = |value: Value, root: &str| resolve_workdir(&context(value), Path::new(root));
        assert_eq!(
            workdir(json!({"focused_pane_cwd": "/ws/src"}), "/ws"),
            PathBuf::from("/ws/src")
        );
        assert_eq!(
            workdir(json!({"focused_pane_cwd": "/other"}), "/ws"),
            PathBuf::from("/ws")
        );
        assert_eq!(workdir(json!({}), "/ws"), PathBuf::from("/ws"));
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn workspace_root_and_workdir_are_canonical_even_when_the_context_uses_a_symlinked_spelling() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        git(&repo, &["init", "-q"]);
        let link = root.join("alias");
        symlink(&repo, &link).unwrap();
        let pane = link.join("src");
        let via_link =
            resolve_workspace_root(&context(json!({"focused_pane_cwd": pane})), &|dir| {
                git_toplevel(dir)
            })
            .unwrap();
        assert_eq!(via_link, repo);
        assert_eq!(
            resolve_workdir(&context(json!({"focused_pane_cwd": pane})), &via_link),
            repo.join("src")
        );
        let by_workspace = resolve_workspace_root(
            &context(json!({"workspace_cwd": link, "focused_pane_cwd": repo.join("src")})),
            &no_repo,
        );
        assert_eq!(by_workspace, Some(repo.clone()));
        assert!(is_inside(&link, repo.join("src")));
    }

    #[test]
    fn git_toplevel_finds_the_repository_root_and_returns_none_elsewhere() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        git(&repo, &["init", "-q"]);
        let top = git_toplevel(repo.join("src").to_str().unwrap()).unwrap();
        assert!(top.ends_with("repo"), "{top}");
        assert_eq!(git_toplevel("/"), None);
    }

    #[test]
    fn canonical_path_resolves_a_symlinked_prefix_even_when_the_path_itself_is_gone() {
        let temp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(temp.path()).unwrap();
        std::fs::create_dir(dir.join("real")).unwrap();
        symlink(dir.join("real"), dir.join("alias")).unwrap();
        assert_eq!(canonical_path(dir.join("alias")), dir.join("real"));
        assert_eq!(
            canonical_path(dir.join("alias/gone/deeper")),
            dir.join("real/gone/deeper"),
            "the longest existing ancestor is canonicalised"
        );
        assert_eq!(
            canonical_path(dir.join("alias/../alias/gone")),
            dir.join("real/gone")
        );
        assert_eq!(
            canonical_path("/definitely/not/here"),
            PathBuf::from("/definitely/not/here")
        );
        assert_eq!(canonical_path("/a/./b/../c"), PathBuf::from("/a/c"));
    }
}

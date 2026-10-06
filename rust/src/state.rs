//! Pane-to-session mapping store: one JSON file per pane under
//! HERDR_PLUGIN_STATE_DIR/panes/, each rewritten atomically. Per-pane files
//! mean a bridge updating one pane can never clobber another pane's mapping.
//!
//! Entries are JSON objects kept as [`Entry`] maps, so fields this version does
//! not know (written by a newer or older plugin) survive a read-modify-write.

use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Map, Value};

use crate::constants::{LIFECYCLE_STATES, LOCK_WAIT_ENV, LOCK_WAIT_MS, PANES_DIR, STATE_VERSION};
use crate::context::canonical_path;
use crate::errors::{ErrorKind, PluginError, Result};
use crate::procfs::{process_alive, stat_fields_after_name, PROC_ROOT};
use crate::util::{iso_now, random_hex, sha256_hex};

/// One pane's mapping.
pub type Entry = Map<String, Value>;

/// Every mapping, in mapping-file name order.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub version: u32,
    pub panes: Vec<(String, Entry)>,
}

#[cfg(test)]
impl State {
    pub fn get(&self, pane_id: &str) -> Option<&Entry> {
        self.panes
            .iter()
            .find(|(id, _)| id == pane_id)
            .map(|(_, entry)| entry)
    }

    pub fn pane_ids(&self) -> Vec<&str> {
        self.panes.iter().map(|(id, _)| id.as_str()).collect()
    }
}

/// Returns the directory holding the per-pane files.
pub fn panes_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(PANES_DIR)
}

/// Returns the file that stores one pane's mapping. The name keeps the pane id
/// readable and appends a hash so distinct ids can never collide.
pub fn pane_entry_path(state_dir: &Path, pane_id: &str) -> Result<PathBuf> {
    if pane_id.is_empty() {
        return Err(PluginError::new(
            ErrorKind::Target,
            "A pane id is required to address a sandbox mapping.",
        ));
    }
    // One underscore per UTF-16 code unit, as the JS regex replaces them, so file names agree.
    let readable: String = pane_id
        .encode_utf16()
        .map(|unit| match u8::try_from(unit) {
            Ok(byte) if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') => {
                byte as char
            }
            _ => '_',
        })
        .take(64)
        .collect();
    let digest = &sha256_hex(pane_id)[..10];
    Ok(panes_dir(state_dir).join(format!("{readable}-{digest}.json")))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Writes JSON atomically by writing a sibling temp file and renaming it.
pub fn write_json_atomic(file: &Path, value: &Value) -> Result<()> {
    let startup = |error: std::io::Error| {
        PluginError::new(
            ErrorKind::Startup,
            format!("Could not write {}: {error}", file.display()),
        )
        .with_cause(error)
    };
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(startup)?;
    }
    let temp = with_suffix(
        file,
        &format!(".{}.{}.tmp", std::process::id(), random_hex(4)),
    );
    let written = (|| -> std::io::Result<()> {
        let mut handle = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        let text = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
        handle.write_all(text.as_bytes())?;
        handle.write_all(b"\n")?;
        drop(handle);
        std::fs::rename(&temp, file)
    })();
    written.map_err(|error| {
        if let Err(cleanup) = std::fs::remove_file(&temp) {
            if cleanup.kind() != std::io::ErrorKind::NotFound {
                eprintln!("could not remove temp file {}: {cleanup}", temp.display());
            }
        }
        startup(error)
    })
}

fn read_entry_file(file: &Path) -> Result<Entry> {
    let unreadable = |detail: String, cause: Box<dyn std::error::Error + Send + Sync>| {
        let mut error = PluginError::new(
            ErrorKind::Startup,
            format!("Mapping file {} is unreadable: {detail}", file.display()),
        );
        error.cause = Some(cause);
        error
    };
    let text = std::fs::read_to_string(file)
        .map_err(|error| unreadable(error.to_string(), Box::new(error)))?;
    let parsed: Value = serde_json::from_str(&text)
        .map_err(|error| unreadable(error.to_string(), Box::new(error)))?;
    let unsupported = || {
        PluginError::new(
            ErrorKind::Startup,
            format!(
                "Mapping file {} has an unsupported format. Expected version {STATE_VERSION}.",
                file.display()
            ),
        )
    };
    let Value::Object(entry) = parsed else {
        return Err(unsupported());
    };
    let version_ok = entry.get("version").and_then(Value::as_f64) == Some(f64::from(STATE_VERSION));
    if !version_ok || !entry.get("paneId").is_some_and(Value::is_string) {
        return Err(unsupported());
    }
    Ok(entry)
}

fn pane_id_of(entry: &Entry) -> &str {
    entry
        .get("paneId")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Loads every mapping.
pub fn load_state(state_dir: &Path) -> Result<State> {
    let mut panes: Vec<(String, Entry)> = Vec::new();
    let dir = panes_dir(state_dir);
    if dir.exists() {
        let listing = std::fs::read_dir(&dir).map_err(|error| {
            PluginError::new(
                ErrorKind::Startup,
                format!("Mapping directory {} is unreadable: {error}", dir.display()),
            )
            .with_cause(error)
        })?;
        let mut names: Vec<String> = listing
            .flatten()
            .filter_map(|item| item.file_name().into_string().ok())
            .filter(|name| name.ends_with(".json"))
            .collect();
        names.sort();
        for name in names {
            let entry = match read_entry_file(&dir.join(&name)) {
                Ok(entry) => entry,
                // Forgotten, pruned or moved between the directory listing and the read.
                Err(error) if error.cause_is_io(std::io::ErrorKind::NotFound) => continue,
                Err(error) => return Err(error),
            };
            let pane_id = pane_id_of(&entry).to_string();
            match panes.iter_mut().find(|(id, _)| *id == pane_id) {
                Some(slot) => slot.1 = entry,
                None => panes.push((pane_id, entry)),
            }
        }
    }
    Ok(State {
        version: STATE_VERSION,
        panes,
    })
}

/// Inserts or replaces the entry for a pane.
pub fn save_pane_entry(state_dir: &Path, pane_id: &str, entry: &Entry) -> Result<Entry> {
    let lifecycle_ok = entry
        .get("lifecycleState")
        .and_then(Value::as_str)
        .is_some_and(|state| LIFECYCLE_STATES.contains(&state));
    if !lifecycle_ok {
        let shown = match entry.get("lifecycleState") {
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
            None => "undefined".to_string(),
        };
        return Err(PluginError::new(
            ErrorKind::Startup,
            format!("Refusing to store unknown lifecycle state \"{shown}\"."),
        ));
    }
    let mut stored = entry.clone();
    stored.insert("version".into(), Value::from(STATE_VERSION));
    stored.insert("paneId".into(), Value::from(pane_id));
    stored.insert("updatedAt".into(), Value::from(iso_now()));
    // `revision` changes on every save, unlike `updatedAt`, which two saves in the same millisecond share.
    stored.insert("revision".into(), Value::from(random_hex(6)));
    let file = pane_entry_path(state_dir, pane_id)?;
    // Every writer takes the mapping lock, so a compare-and-delete under the same
    // lock can never unlink a file another process replaced in between.
    with_pane_lock(state_dir, pane_id, || {
        write_json_atomic(&file, &Value::Object(stored.clone()))
    })?;
    Ok(stored)
}

/// Applies a partial update to an existing entry.
pub fn update_pane_entry(state_dir: &Path, pane_id: &str, patch: &Entry) -> Result<Entry> {
    // Read, merge and write under one lock, or a concurrent writer's fields (a
    // bridge's pid, a deletion claim) would be overwritten with a stale copy.
    with_pane_lock(state_dir, pane_id, || {
        let mut merged = require_pane_entry(state_dir, Some(pane_id))?;
        for (key, value) in patch {
            merged.insert(key.clone(), value.clone());
        }
        save_pane_entry(state_dir, pane_id, &merged)
    })
}

/// Path of the lock file that serialises read-check-write sequences on a
/// pane's mapping (next to the mapping, so it lives and dies with the state dir).
pub fn pane_lock_path(state_dir: &Path, pane_id: &str) -> Result<PathBuf> {
    Ok(with_suffix(&pane_entry_path(state_dir, pane_id)?, ".lock"))
}

thread_local! {
    /// Lock files this thread holds right now, so nested sections do not wait for themselves.
    static HELD_LOCKS: RefCell<HashSet<PathBuf>> = RefCell::new(HashSet::new());
}

/// How long a caller waits for a live lock owner: [`LOCK_WAIT_MS`] unless [`LOCK_WAIT_ENV`] overrides it.
fn default_lock_wait_ms() -> u64 {
    std::env::var(LOCK_WAIT_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|ms| ms.is_finite() && *ms > 0.0)
        .map_or(LOCK_WAIT_MS, |ms| ms as u64)
}

/// A token that identifies one incarnation of a process: its start time as the
/// kernel reports it (field 22 of /proc/PID/stat). A recycled pid carries a
/// different token, so records that store a pid store this next to it. `None`
/// when the process is gone or the platform cannot say.
pub fn process_start_token(pid: u32) -> Option<String> {
    let stat =
        std::fs::read_to_string(Path::new(PROC_ROOT).join(pid.to_string()).join("stat")).ok()?;
    // The first word after the name is field 3 (state); the start time is field 22.
    let start_time = *stat_fields_after_name(&stat).get(19)?;
    (!start_time.is_empty() && start_time.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| format!("linux:{start_time}"))
}

/// This process's own start token, computed once: a process's start time never changes.
pub fn own_start_token() -> Option<String> {
    static TOKEN: OnceLock<Option<String>> = OnceLock::new();
    TOKEN
        .get_or_init(|| process_start_token(std::process::id()))
        .clone()
}

/// After this long a lock whose owner is alive but cannot be identified is treated as abandoned; sections are held for milliseconds.
const LOCK_UNVERIFIED_MAX_MS: u64 = 60_000;

/// A lock file's content: the owner's pid and start token plus the file's age.
struct LockOwner {
    text: String,
    pid: Option<u32>,
    token: Option<String>,
    age_ms: u64,
}

/// Reads a lock file. `None` when the file is gone or unreadable right now.
fn read_lock_owner(lock: &Path) -> Option<LockOwner> {
    let text = std::fs::read_to_string(lock).ok()?.trim().to_string();
    // "<pid> <token>": the token is everything after the first whitespace, whatever it contains.
    let (pid_text, token) = match text.find(char::is_whitespace) {
        Some(split) => (
            &text[..split],
            Some(text[split..].trim().to_string()).filter(|token| !token.is_empty()),
        ),
        None => (text.as_str(), None),
    };
    let pid = pid_text.parse::<u32>().ok().filter(|pid| *pid > 0);
    let age_ms = std::fs::metadata(lock)
        .and_then(|meta| meta.modified())
        .map(|modified| {
            SystemTime::now()
                .duration_since(modified)
                .unwrap_or_default()
                .as_millis() as u64
        })
        .unwrap_or(0);
    Some(LockOwner {
        token,
        pid,
        age_ms,
        text,
    })
}

/// Whether a lock owner is gone: its pid is dead, its pid now belongs to a
/// different incarnation (start token mismatch), or it cannot be identified and
/// the lock is far older than any section is ever held. An empty or garbled
/// lock counts once it is older than the wait.
fn lock_owner_gone(owner: Option<&LockOwner>, wait_ms: u64) -> bool {
    let Some(owner) = owner else {
        return false;
    };
    let Some(pid) = owner.pid else {
        return owner.age_ms > wait_ms;
    };
    if !process_alive(pid) {
        return true;
    }
    if let Some(token) = owner.token.as_deref().filter(|token| *token != "-") {
        if let Some(current) = process_start_token(pid) {
            return current != token;
        }
    }
    owner.age_ms > wait_ms.max(LOCK_UNVERIFIED_MAX_MS)
}

/// Removes a stale lock. Reclaimers are serialised by their own exclusive
/// guard file, and the lock is inspected again under that guard: it is removed
/// only while it still names the dead owner (or is still empty and old). A
/// fresh lock a faster contender created in the meantime therefore survives,
/// because a lock can only be replaced by a reclaimer, and reclaimers wait for
/// each other.
fn reclaim_stale_lock(lock: &Path, seen_text: &str, wait_ms: u64) {
    let guard = with_suffix(lock, ".reclaim");
    let mut handle = match OpenOptions::new().write(true).create_new(true).open(&guard) {
        Ok(handle) => handle,
        Err(error) => {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                break_abandoned_guard(&guard, wait_ms);
            }
            return;
        }
    };
    let _ = writeln!(handle, "{}", std::process::id());
    drop(handle);
    // Only the very lock that was inspected, still with its dead owner, is removed.
    let owner = read_lock_owner(lock);
    if owner.as_ref().is_some_and(|owner| owner.text == seen_text)
        && lock_owner_gone(owner.as_ref(), wait_ms)
    {
        let _ = std::fs::remove_file(lock);
    }
    let _ = std::fs::remove_file(&guard);
}

/// A reclaim guard is held for microseconds. One whose owner is dead, or one
/// older than the lock wait (its pid may have been recycled), was left behind
/// by a reclaimer that died and is removed.
fn break_abandoned_guard(guard: &Path, wait_ms: u64) {
    let Ok(text) = std::fs::read_to_string(guard) else {
        return;
    };
    let owner_dead = text
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 0)
        .is_some_and(|pid| !process_alive(pid));
    let age_ms = std::fs::metadata(guard)
        .and_then(|meta| meta.modified())
        .map(|modified| {
            SystemTime::now()
                .duration_since(modified)
                .unwrap_or_default()
                .as_millis() as u64
        })
        .unwrap_or(0);
    if owner_dead || age_ms > wait_ms {
        let _ = std::fs::remove_file(guard);
    }
}

/// Removes this process's lock, and only this process's: a lock reclaimed and
/// re-created by someone else in the meantime is left alone.
fn release_lock(lock: &Path) {
    if read_lock_owner(lock).and_then(|owner| owner.pid) == Some(std::process::id()) {
        let _ = std::fs::remove_file(lock);
    }
}

/// Releases a held lock when the section ends, including by panic.
struct HeldLock<'a>(&'a Path);

impl Drop for HeldLock<'_> {
    fn drop(&mut self) {
        HELD_LOCKS.with(|held| held.borrow_mut().remove(self.0));
        release_lock(self.0);
    }
}

/// Runs `section` while holding an exclusive lock on a pane's mapping, so a
/// bridge acknowledging itself and an action changing the mapping cannot
/// interleave their read-check-write sequences. The lock is a file created with
/// O_EXCL that holds the owner's pid and start token; a lock whose owner is
/// gone, or that stayed empty longer than the wait, is broken. Waits up to
/// [`LOCK_WAIT_MS`] (or [`LOCK_WAIT_ENV`]) for a live owner.
pub fn with_pane_lock<T>(
    state_dir: &Path,
    pane_id: &str,
    section: impl FnOnce() -> Result<T>,
) -> Result<T> {
    with_pane_lock_wait(state_dir, pane_id, default_lock_wait_ms(), section)
}

/// [`with_pane_lock`] with an explicit wait in milliseconds.
pub fn with_pane_lock_wait<T>(
    state_dir: &Path,
    pane_id: &str,
    wait_ms: u64,
    section: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let lock = pane_lock_path(state_dir, pane_id)?;
    with_lock_file(
        &lock,
        &format!("The mapping of pane {pane_id}"),
        wait_ms,
        section,
    )
}

/// The lock primitive behind [`with_pane_lock`]. `what` describes the locked thing in the conflict message.
fn with_lock_file<T>(
    lock: &Path,
    what: &str,
    wait_ms: u64,
    section: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if HELD_LOCKS.with(|held| held.borrow().contains(lock)) {
        // Re-entrant within one thread: a save inside a locked section must not wait for itself.
        return section();
    }
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            PluginError::new(
                ErrorKind::Startup,
                format!("Could not lock {}: {error}", lock.display()),
            )
            .with_cause(error)
        })?;
    }
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    loop {
        match OpenOptions::new().write(true).create_new(true).open(lock) {
            Ok(mut handle) => {
                let written = writeln!(
                    handle,
                    "{} {}",
                    std::process::id(),
                    own_start_token().as_deref().unwrap_or("-")
                );
                drop(handle);
                if let Err(error) = written {
                    let _ = std::fs::remove_file(lock);
                    return Err(PluginError::new(
                        ErrorKind::Startup,
                        format!("Could not lock {}: {error}", lock.display()),
                    )
                    .with_cause(error));
                }
                HELD_LOCKS.with(|held| held.borrow_mut().insert(lock.to_path_buf()));
                let _held = HeldLock(lock);
                return section();
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(PluginError::new(
                    ErrorKind::Startup,
                    format!("Could not lock {}: {error}", lock.display()),
                )
                .with_cause(error));
            }
        }
        let owner = read_lock_owner(lock);
        if lock_owner_gone(owner.as_ref(), wait_ms) {
            if let Some(owner) = &owner {
                reclaim_stale_lock(lock, &owner.text, wait_ms);
            }
            // No early retry: the deadline and the pause below also bound a reclaim
            // that makes no progress, so a waiter can never spin or hang here.
        }
        if Instant::now() >= deadline {
            let holder = owner
                .and_then(|owner| owner.pid)
                .map_or_else(|| "unknown".to_string(), |pid| pid.to_string());
            return Err(PluginError::new(
                ErrorKind::Conflict,
                format!("{what} is locked by process {holder}; try again in a moment."),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Removes the entry for a pane only if it is still the one the caller read
/// (same `revision` and `updatedAt`), so a mapping rewritten in the meantime
/// (Herdr handed the pane id to a new agent) survives a cleanup that decided
/// on stale data. Returns whether the entry was removed.
pub fn delete_pane_entry_if_unchanged(
    state_dir: &Path,
    pane_id: &str,
    seen: &Entry,
) -> Result<bool> {
    with_pane_lock(state_dir, pane_id, || {
        let Some(current) = get_pane_entry(state_dir, Some(pane_id))? else {
            return Ok(false);
        };
        if current.get("revision") != seen.get("revision")
            || current.get("updatedAt") != seen.get("updatedAt")
        {
            return Ok(false);
        }
        delete_pane_entry(state_dir, pane_id)
    })
}

/// Removes the entry for a pane if present, under the mapping lock. Returns
/// whether an entry existed.
pub fn delete_pane_entry(state_dir: &Path, pane_id: &str) -> Result<bool> {
    let file = pane_entry_path(state_dir, pane_id)?;
    with_pane_lock(state_dir, pane_id, || {
        if !file.exists() {
            return Ok(false);
        }
        std::fs::remove_file(&file).map_err(|error| {
            PluginError::new(
                ErrorKind::Startup,
                format!("Could not remove {}: {error}", file.display()),
            )
            .with_cause(error)
        })?;
        Ok(true)
    })
}

/// Returns the entry for a pane or `None`.
pub fn get_pane_entry(state_dir: &Path, pane_id: Option<&str>) -> Result<Option<Entry>> {
    let Some(pane_id) = pane_id.filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let file = pane_entry_path(state_dir, pane_id)?;
    if !file.exists() {
        return Ok(None);
    }
    let entry = read_entry_file(&file)?;
    if pane_id_of(&entry) != pane_id {
        return Err(PluginError::new(
            ErrorKind::Startup,
            format!(
                "Mapping file {} belongs to pane {}, not {pane_id}.",
                file.display(),
                pane_id_of(&entry)
            ),
        ));
    }
    Ok(Some(entry))
}

/// Returns the entry for a pane or a `target` error.
pub fn require_pane_entry(state_dir: &Path, pane_id: Option<&str>) -> Result<Entry> {
    get_pane_entry(state_dir, pane_id)?.ok_or_else(|| {
        PluginError::new(
            ErrorKind::Target,
            match pane_id.filter(|id| !id.is_empty()) {
                Some(id) => {
                    format!("No sandboxed agent is mapped to pane {id}. Run start-agent first.")
                }
                None => "No focused pane was provided, so there is no sandbox mapping to act on."
                    .to_string(),
            },
        )
    })
}

/// Finds every entry whose mount root equals the given path.
pub fn entries_for_local_path<'a>(
    state: &'a State,
    local_path: &Path,
) -> Vec<(&'a str, &'a Entry)> {
    let wanted = canonical_path(local_path);
    state
        .panes
        .iter()
        .filter(|(_, entry)| {
            entry
                .get("localPath")
                .and_then(Value::as_str)
                .is_some_and(|path| canonical_path(path) == wanted)
        })
        .map(|(id, entry)| (id.as_str(), entry))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::symlink;

    fn entry(value: Value) -> Entry {
        value.as_object().cloned().unwrap()
    }

    fn running(session: &str) -> Entry {
        entry(
            json!({"sessionName": session, "localPath": "/w", "workdir": "/w", "agentKind": "opencode", "lifecycleState": "running"}),
        )
    }

    /// Runs `body` with HERDR_NONO_LOCK_WAIT_MS set; the environment is process-wide, so such tests take turns.
    fn with_lock_wait_env<T>(value: &str, body: impl FnOnce() -> T) -> T {
        static ENV_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _turn = ENV_TURN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::env::set_var(LOCK_WAIT_ENV, value);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
        std::env::remove_var(LOCK_WAIT_ENV);
        outcome.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    }

    #[test]
    fn the_lock_wait_environment_variable_overrides_the_default() {
        assert_eq!(with_lock_wait_env("250", default_lock_wait_ms), 250);
        assert_eq!(
            with_lock_wait_env("nope", default_lock_wait_ms),
            LOCK_WAIT_MS
        );
        assert_eq!(with_lock_wait_env("0", default_lock_wait_ms), LOCK_WAIT_MS);
    }

    fn fresh() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn lock_of(dir: &Path, pane: &str) -> PathBuf {
        pane_lock_path(dir, pane).unwrap()
    }

    fn json_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(panes_dir(dir))
            .unwrap()
            .map(|item| item.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn load_state_returns_an_empty_store_when_nothing_was_saved() {
        let state = load_state(fresh().path()).unwrap();
        assert_eq!(state.version, 1);
        assert!(state.panes.is_empty());
    }

    #[test]
    fn save_update_get_and_delete_round_trip_with_one_file_per_pane_and_no_temp_files() {
        let dir = fresh();
        let dir = dir.path();
        let stored = save_pane_entry(dir, "ws:1:3", &entry(json!({"sessionName": "s-1", "lifecycleState": "provisional", "localPath": "/repo"}))).unwrap();
        assert_eq!(stored["paneId"], "ws:1:3");
        assert_eq!(stored["version"], 1);
        assert!(stored["updatedAt"].as_str().unwrap().ends_with('Z'));
        assert_eq!(
            get_pane_entry(dir, Some("ws:1:3")).unwrap().unwrap()["sessionName"],
            "s-1"
        );
        update_pane_entry(dir, "ws:1:3", &entry(json!({"lifecycleState": "running"}))).unwrap();
        assert_eq!(
            require_pane_entry(dir, Some("ws:1:3")).unwrap()["lifecycleState"],
            "running"
        );
        save_pane_entry(
            dir,
            "ws:1:4",
            &entry(
                json!({"sessionName": "s-2", "lifecycleState": "running", "localPath": "/repo"}),
            ),
        )
        .unwrap();
        let files = json_files(dir);
        assert_eq!(
            files.len(),
            2,
            "{files:?} holds two mappings and no lock or temp files"
        );
        assert!(
            files
                .iter()
                .all(|name| name.ends_with(".json") && name.starts_with("ws_1_")),
            "{files:?}"
        );
        assert_eq!(
            pane_entry_path(dir, "ws:1:3")
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap(),
            files
                .iter()
                .find(|name| name.starts_with("ws_1_3"))
                .unwrap()
        );
        assert_eq!(load_state(dir).unwrap().pane_ids(), ["ws:1:3", "ws:1:4"]);
        assert!(delete_pane_entry(dir, "ws:1:3").unwrap());
        assert!(!delete_pane_entry(dir, "ws:1:3").unwrap());
        assert_eq!(get_pane_entry(dir, Some("ws:1:3")).unwrap(), None);
        assert_eq!(get_pane_entry(dir, None).unwrap(), None);
        assert_eq!(load_state(dir).unwrap().pane_ids(), ["ws:1:4"]);
    }

    #[test]
    fn pane_ids_that_look_like_prototype_keys_are_stored_safely() {
        let dir = fresh();
        save_pane_entry(dir.path(), "__proto__", &running("s-p")).unwrap();
        let state = load_state(dir.path()).unwrap();
        assert_eq!(state.get("__proto__").unwrap()["sessionName"], "s-p");
        assert_eq!(
            get_pane_entry(dir.path(), Some("constructor")).unwrap(),
            None
        );
    }

    #[test]
    fn save_pane_entry_refuses_unknown_lifecycle_states() {
        let dir = fresh();
        for bad in [json!({"lifecycleState": "bogus"}), json!({})] {
            let error = save_pane_entry(dir.path(), "p", &entry(bad)).unwrap_err();
            assert!(
                error.message.contains("unknown lifecycle state"),
                "{}",
                error.message
            );
        }
        assert!(save_pane_entry(dir.path(), "p", &entry(json!({})))
            .unwrap_err()
            .message
            .contains("\"undefined\""));
    }

    #[test]
    fn require_pane_entry_explains_the_missing_mapping() {
        let dir = fresh();
        let missing = require_pane_entry(dir.path(), Some("pane-9")).unwrap_err();
        assert_eq!(missing.kind, ErrorKind::Target);
        assert!(missing.message.contains("pane-9"));
        let unfocused = require_pane_entry(dir.path(), None).unwrap_err();
        assert_eq!(unfocused.kind, ErrorKind::Target);
        assert!(unfocused.message.contains("No focused pane"));
    }

    #[test]
    fn load_state_rejects_unsupported_formats() {
        let dir = fresh();
        std::fs::create_dir(panes_dir(dir.path())).unwrap();
        let file = panes_dir(dir.path()).join("x.json");
        std::fs::write(&file, r#"{"version": 99, "paneId": "x"}"#).unwrap();
        assert!(load_state(dir.path())
            .unwrap_err()
            .message
            .contains("unsupported format"));
        std::fs::write(&file, r#"{"version": 1}"#).unwrap();
        assert!(load_state(dir.path())
            .unwrap_err()
            .message
            .contains("unsupported format"));
        std::fs::write(&file, "[1]").unwrap();
        assert!(load_state(dir.path())
            .unwrap_err()
            .message
            .contains("unsupported format"));
        std::fs::write(&file, "nope").unwrap();
        assert!(load_state(dir.path())
            .unwrap_err()
            .message
            .contains("unreadable"));
    }

    #[test]
    fn entries_for_local_path_matches_resolved_and_symlinked_paths() {
        let state = State {
            version: 1,
            panes: vec![
                ("a".into(), entry(json!({"localPath": "/repo/x/../x"}))),
                ("b".into(), entry(json!({"localPath": "/repo/y"}))),
                ("c".into(), entry(json!({}))),
            ],
        };
        let ids = |found: Vec<(&str, &Entry)>| {
            found
                .into_iter()
                .map(|(id, _)| id.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(entries_for_local_path(&state, Path::new("/repo/x"))),
            ["a"]
        );
        let temp = fresh();
        let dir = std::fs::canonicalize(temp.path()).unwrap();
        std::fs::create_dir(dir.join("real")).unwrap();
        symlink(dir.join("real"), dir.join("alias")).unwrap();
        let linked = State {
            version: 1,
            panes: vec![("r".into(), entry(json!({"localPath": dir.join("real")})))],
        };
        assert_eq!(
            ids(entries_for_local_path(&linked, &dir.join("alias"))),
            ["r"]
        );
    }

    #[test]
    fn entries_for_local_path_still_matches_a_removed_worktree_recorded_under_a_symlinked_prefix() {
        let temp = fresh();
        let dir = std::fs::canonicalize(temp.path()).unwrap();
        let state_dir = dir.join("state");
        std::fs::create_dir_all(dir.join("real")).unwrap();
        symlink(dir.join("real"), dir.join("alias")).unwrap();
        // The worktree existed when the mapping was written, so its canonical spelling was stored; now it is gone.
        let gone = dir.join("real/gone");
        save_pane_entry(&state_dir, "pane-1", &entry(json!({"sessionName": "herdr-x-1", "localPath": gone, "workdir": gone, "agentKind": "opencode", "lifecycleState": "running"}))).unwrap();
        let state = load_state(&state_dir).unwrap();
        assert_eq!(
            entries_for_local_path(&state, &dir.join("alias/gone")).len(),
            1
        );
        assert!(entries_for_local_path(&state, &dir.join("alias/other")).is_empty());
    }

    #[test]
    fn delete_pane_entry_if_unchanged_only_removes_the_entry_the_caller_read() {
        let dir = fresh();
        let dir = dir.path();
        save_pane_entry(dir, "pane-1", &running("herdr-x-1")).unwrap();
        let seen = get_pane_entry(dir, Some("pane-1")).unwrap().unwrap();
        let mut rewritten = seen.clone();
        rewritten.insert("sessionName".into(), json!("herdr-x-2"));
        save_pane_entry(dir, "pane-1", &rewritten).unwrap();
        assert!(
            !delete_pane_entry_if_unchanged(dir, "pane-1", &seen).unwrap(),
            "rewritten since it was read, even within the same millisecond"
        );
        let current = get_pane_entry(dir, Some("pane-1")).unwrap().unwrap();
        assert_eq!(current["sessionName"], "herdr-x-2");
        assert_ne!(current["revision"], seen["revision"]);
        assert!(delete_pane_entry_if_unchanged(dir, "pane-1", &current).unwrap());
        assert_eq!(get_pane_entry(dir, Some("pane-1")).unwrap(), None);
        assert!(
            !delete_pane_entry_if_unchanged(dir, "pane-1", &current).unwrap(),
            "already gone"
        );
    }

    #[test]
    fn with_pane_lock_runs_the_callback_under_a_lock_file_breaks_stale_locks_and_gives_up_on_a_live_one(
    ) {
        let temp = fresh();
        let dir = temp.path();
        let lock = lock_of(dir, "pane-1");
        let value = with_pane_lock(dir, "pane-1", || {
            assert!(lock.exists(), "held while the callback runs");
            Ok(42)
        })
        .unwrap();
        assert_eq!(value, 42);
        assert!(!lock.exists(), "released afterwards");
        let failed: Result<()> = with_pane_lock(dir, "pane-1", || {
            Err(PluginError::new(ErrorKind::Unknown, "boom"))
        });
        assert_eq!(failed.unwrap_err().message, "boom");
        assert!(!lock.exists(), "released after an error too");
        std::fs::write(&lock, "2147483647\n").unwrap();
        assert_eq!(
            with_pane_lock(dir, "pane-1", || Ok("broke the stale lock")).unwrap(),
            "broke the stale lock",
            "a lock whose owner is gone is taken over"
        );
        std::fs::write(&lock, format!("{}\n", std::process::id())).unwrap();
        let started = Instant::now();
        let error = with_pane_lock_wait(dir, "pane-1", 150, || Ok("never")).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Conflict);
        assert!(
            error.message.contains("locked by process"),
            "{}",
            error.message
        );
        assert!(
            started.elapsed() >= Duration::from_millis(150),
            "waited for the live owner before giving up"
        );
        assert!(lock.exists(), "a live owner's lock is left alone");
    }

    #[test]
    fn a_panicking_section_still_releases_the_lock() {
        let temp = fresh();
        let dir = temp.path();
        let outcome = std::panic::catch_unwind(|| {
            let _ = with_pane_lock(dir, "pane-1", || -> Result<()> { panic!("boom") });
        });
        assert!(outcome.is_err());
        assert!(!lock_of(dir, "pane-1").exists());
        assert_eq!(
            with_pane_lock(dir, "pane-1", || Ok(1)).unwrap(),
            1,
            "and it is not still marked as held"
        );
    }

    #[test]
    fn the_mapping_lock_is_reentrant_and_every_writer_and_deleter_takes_it() {
        let temp = fresh();
        let dir = temp.path();
        let lock = lock_of(dir, "pane-1");
        let seen = with_pane_lock(dir, "pane-1", || {
            save_pane_entry(dir, "pane-1", &running("herdr-x-1"))?;
            assert!(
                lock.exists(),
                "a save inside a locked section reuses the lock instead of waiting for it"
            );
            with_pane_lock(dir, "pane-1", || get_pane_entry(dir, Some("pane-1")))
        })
        .unwrap()
        .unwrap();
        assert!(!lock.exists());
        assert_eq!(seen["sessionName"], "herdr-x-1");
        std::fs::write(&lock, format!("{}\n", std::process::id())).unwrap();
        let (save, delete) = with_lock_wait_env("100", || {
            (
                save_pane_entry(dir, "pane-1", &running("herdr-x-2")),
                delete_pane_entry_if_unchanged(dir, "pane-1", &seen),
            )
        });
        assert_eq!(
            save.unwrap_err().kind,
            ErrorKind::Conflict,
            "a save waits for a foreign live lock rather than racing it"
        );
        assert_eq!(delete.unwrap_err().kind, ErrorKind::Conflict);
        assert_eq!(
            get_pane_entry(dir, Some("pane-1")).unwrap().unwrap()["sessionName"],
            "herdr-x-1",
            "nothing changed while the lock was foreign"
        );
    }

    #[test]
    fn update_pane_entry_reads_and_writes_under_the_lock_so_a_concurrent_bridge_pid_is_never_overwritten(
    ) {
        let temp = fresh();
        let dir = temp.path();
        save_pane_entry(dir, "pane-1", &running("herdr-x-1")).unwrap();
        std::fs::write(lock_of(dir, "pane-1"), format!("{}\n", std::process::id())).unwrap();
        let update = with_lock_wait_env("150", || {
            update_pane_entry(dir, "pane-1", &entry(json!({"lifecycleState": "exited"})))
        });
        assert_eq!(
            update.unwrap_err().kind,
            ErrorKind::Conflict,
            "the read waits for the lock, not only the write"
        );
        assert_eq!(
            get_pane_entry(dir, Some("pane-1")).unwrap().unwrap()["lifecycleState"],
            "running"
        );
    }

    #[test]
    fn two_contenders_reclaiming_the_same_stale_lock_never_hold_it_at_the_same_time() {
        let temp = fresh();
        let dir = temp.path().to_path_buf();
        let lock = lock_of(&dir, "pane-1");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, "2147483647\n").unwrap();
        let spans: Vec<(Instant, Instant)> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        with_pane_lock_wait(&dir, "pane-1", 10_000, || {
                            let start = Instant::now();
                            std::thread::sleep(Duration::from_millis(150));
                            Ok((start, Instant::now()))
                        })
                        .unwrap()
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect()
        });
        let (a, b) = (spans[0], spans[1]);
        let disjoint = a.1 <= b.0 || b.1 <= a.0;
        assert!(disjoint, "the two hold intervals overlap: {a:?} {b:?}");
        assert!(!lock.exists(), "the lock is released at the end");
    }

    #[test]
    fn a_stale_lock_is_reclaimed_even_when_a_dead_reclaimer_left_its_guard_behind() {
        let temp = fresh();
        let dir = temp.path();
        let lock = lock_of(dir, "pane-1");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        let guard = with_suffix(&lock, ".reclaim");
        std::fs::write(&lock, "2147483647\n").unwrap();
        std::fs::write(&guard, "2147483646\n").unwrap();
        assert_eq!(
            with_pane_lock(dir, "pane-1", || Ok("reclaimed")).unwrap(),
            "reclaimed"
        );
        assert!(!lock.exists());
        assert!(!guard.exists(), "the abandoned guard is gone too");
        std::fs::write(&lock, format!("{}\n", std::process::id())).unwrap();
        std::fs::write(&guard, format!("{}\n", std::process::id())).unwrap();
        let error = with_pane_lock_wait(dir, "pane-1", 150, || Ok("never")).unwrap_err();
        assert_eq!(
            error.kind,
            ErrorKind::Conflict,
            "a live owner's lock is never reclaimed"
        );
        assert!(lock.exists());
    }

    #[test]
    fn a_lock_whose_pid_was_recycled_is_reclaimed_and_a_live_reclaim_guard_never_makes_a_waiter_hang(
    ) {
        let temp = fresh();
        let dir = temp.path();
        let lock = lock_of(dir, "pane-1");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, format!("{} linux:0\n", std::process::id())).unwrap();
        assert_eq!(
            with_pane_lock(dir, "pane-1", || Ok("reclaimed")).unwrap(),
            "reclaimed",
            "a live pid with a different start token is a different process"
        );
        assert!(!lock.exists());
        std::fs::write(&lock, "2147483647 -\n").unwrap();
        std::fs::write(
            with_suffix(&lock, ".reclaim"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        let started = Instant::now();
        let outcome = with_pane_lock_wait(dir, "pane-1", 300, || Ok("acquired"));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the waiter returns, it does not spin forever"
        );
        match outcome {
            Ok(value) => assert_eq!(value, "acquired"),
            Err(error) => assert_eq!(error.kind, ErrorKind::Conflict),
        }
    }

    #[test]
    fn process_start_tokens_carry_no_whitespace_and_a_lock_naming_a_live_owner_by_its_real_token_is_honoured(
    ) {
        let temp = fresh();
        let dir = temp.path();
        let lock = lock_of(dir, "pane-1");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        let token = process_start_token(std::process::id()).expect("a Linux /proc");
        assert!(
            token.starts_with("linux:") && !token.contains(char::is_whitespace),
            "{token}"
        );
        assert_eq!(own_start_token().as_deref(), Some(token.as_str()));
        std::fs::write(&lock, format!("{} {token}\n", std::process::id())).unwrap();
        let error = with_pane_lock_wait(dir, "pane-1", 200, || Ok("never")).unwrap_err();
        assert_eq!(
            error.kind,
            ErrorKind::Conflict,
            "our own live incarnation holds the lock, so it is not reclaimed"
        );
        assert!(lock.exists());
        std::fs::write(
            &lock,
            format!("{} ps:Sun_Sep_13_12:34:56_2026\n", std::process::id()),
        )
        .unwrap();
        assert_eq!(
            with_pane_lock(dir, "pane-1", || Ok("reclaimed")).unwrap(),
            "reclaimed",
            "a token from another incarnation, whatever its shape, marks the lock stale"
        );
        assert_eq!(process_start_token(2_147_483_647), None);
    }

    #[test]
    fn load_state_skips_a_mapping_removed_during_the_listing_but_still_reports_a_broken_one() {
        let temp = fresh();
        let dir = temp.path();
        save_pane_entry(dir, "pane-1", &running("herdr-x-1")).unwrap();
        let panes = panes_dir(dir);
        symlink(
            panes.join("vanished.json"),
            panes.join("gone-0123456789.json"),
        )
        .unwrap();
        assert_eq!(
            load_state(dir).unwrap().pane_ids(),
            ["pane-1"],
            "a file that is gone by the time it is read is not an error"
        );
        std::fs::write(panes.join("broken-0123456789.json"), "{ not json").unwrap();
        let error = load_state(dir).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Startup);
        assert!(error.message.contains("unreadable"));
    }

    #[test]
    fn pane_entry_file_names_equal_the_js_golden_vectors() {
        let golden: Value =
            serde_json::from_str(include_str!("../tests/fixtures/golden.json")).unwrap();
        for vector in golden["paneEntryFile"].as_array().unwrap() {
            let path =
                pane_entry_path(Path::new("/s"), vector["paneId"].as_str().unwrap()).unwrap();
            assert_eq!(
                path.file_name().unwrap().to_str().unwrap(),
                vector["file"].as_str().unwrap(),
                "{vector}"
            );
        }
        assert_eq!(
            pane_entry_path(Path::new("/s"), "").unwrap_err().kind,
            ErrorKind::Target
        );
    }

    #[test]
    fn a_js_written_mapping_loads_and_saves_back_unchanged() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("rust/tests/fixtures/state-v1");
        let name = "pane-1-370bd5d12a.json";
        let original = std::fs::read_to_string(fixtures.join("panes").join(name)).unwrap();
        let state = load_state(&fixtures).unwrap();
        let loaded = state.get("pane-1").expect("the fixture's pane");
        assert_eq!(
            loaded["extraFromFuture"],
            json!({"nested": []}),
            "fields this version does not know are kept"
        );
        // Serialising what was loaded reproduces the JS output byte for byte.
        let temp = fresh();
        let copy = temp.path().join("copy.json");
        write_json_atomic(&copy, &Value::Object(loaded.clone())).unwrap();
        assert_eq!(
            std::fs::read_to_string(&copy).unwrap(),
            original,
            "no diff after a JS-to-Rust round trip"
        );
        // And a full save keeps every field but the two the save refreshes.
        let dir = temp.path().join("state");
        let stored = save_pane_entry(&dir, "pane-1", loaded).unwrap();
        for (key, value) in loaded {
            if key != "updatedAt" && key != "revision" {
                assert_eq!(stored[key], *value, "{key}");
            }
        }
        assert_eq!(
            stored.keys().collect::<Vec<_>>(),
            loaded.keys().collect::<Vec<_>>(),
            "key order is stable"
        );
        assert_eq!(json_files(&dir), [name]);
    }
}

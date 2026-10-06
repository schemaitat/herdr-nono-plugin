//! Calls back into Herdr through the CLI at HERDR_BIN_PATH, which is the
//! portable plugin API (the socket transport differs per OS).

use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use serde_json::Value;

use crate::context::{process_env, Env};
use crate::errors::{ErrorKind, PluginError, Result};
use crate::exec::{run_cli, CancelToken, ExecError, RunOptions};

/// A wedged Herdr server must not hang plugin commands or the agent launch.
pub const HERDR_TIMEOUT: Duration = Duration::from_millis(10_000);

/// Whether Herdr's error output says a pane id is unknown.
pub fn is_pane_not_found(output: &str) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| {
            Regex::new(r"(?i)pane_not_found|pane [^ ]+ not found|no such pane|unknown pane")
                .expect("a valid pattern")
        })
        .is_match(output)
}

/// What a Herdr CLI call printed.
#[derive(Debug, Clone)]
pub struct HerdrOutput {
    pub stdout: String,
    /// The parsed stdout when it is JSON.
    pub json: Option<Value>,
}

/// How to split a pane.
#[derive(Debug, Clone)]
pub struct SplitPane<'a> {
    pub pane_id: Option<&'a str>,
    pub direction: &'a str,
    pub ratio: f64,
    pub cwd: Option<&'a str>,
    pub focus: bool,
}

/// The ids of a freshly created tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedTab {
    pub tab_id: String,
    pub pane_id: String,
}

/// A client bound to one Herdr executable.
#[derive(Debug, Clone)]
pub struct HerdrClient {
    pub bin: String,
    env: Env,
    cancel: Option<CancelToken>,
}

/// Reads `path` (a chain of object keys) out of a JSON value.
fn at<'a>(json: Option<&'a Value>, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(json?, |value, key| value.get(*key))
}

fn non_empty_str(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

impl HerdrClient {
    pub fn new(bin: impl Into<String>, env: Env) -> Self {
        HerdrClient {
            bin: bin.into(),
            env,
            cancel: None,
        }
    }

    /// A client for `bin` with this process's environment.
    pub fn from_process(bin: impl Into<String>) -> Self {
        Self::new(bin, process_env())
    }

    /// Calls made through this client end when the token is cancelled.
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Runs a Herdr CLI command and parses its JSON response when present.
    pub fn run(&self, args: &[&str], step: &str) -> Result<HerdrOutput> {
        let options = RunOptions {
            env: Some(&self.env),
            cwd: None,
            timeout: HERDR_TIMEOUT,
            cancel: self.cancel.as_ref(),
        };
        let output = match run_cli(&self.bin, args, &options) {
            Ok(output) => output,
            Err(ExecError::Spawn(error)) => {
                return Err(PluginError::new(
                    ErrorKind::Startup,
                    format!(
                        "Could not run the Herdr CLI (\"{}\") while {step}: {error}",
                        self.bin
                    ),
                )
                .with_cause(error));
            }
            Err(ExecError::TimedOut(_)) => {
                return Err(PluginError::new(
                    ErrorKind::Startup,
                    format!(
                        "Could not run the Herdr CLI (\"{}\") while {step}: it did not answer within {}s and was killed.",
                        self.bin,
                        HERDR_TIMEOUT.as_secs()
                    ),
                ));
            }
            Err(ExecError::Cancelled(_)) => {
                return Err(PluginError::new(
                    ErrorKind::Startup,
                    format!("Cancelled while {step}."),
                ));
            }
        };
        if output.status != Some(0) {
            let exit = match (output.status, output.signal) {
                (Some(code), _) => format!("exit {code}"),
                (None, Some(signal)) => format!("signal {signal}"),
                (None, None) => "an unknown end".to_string(),
            };
            let words = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
            return Err(PluginError::new(
                ErrorKind::Unknown,
                format!("herdr {words} failed while {step} ({exit})."),
            )
            .with_output(output.output()));
        }
        let json = serde_json::from_str(&output.stdout).ok();
        Ok(HerdrOutput {
            stdout: output.stdout,
            json,
        })
    }

    /// Splits a pane and returns the new pane id.
    pub fn split_pane(&self, split: &SplitPane) -> Result<String> {
        let ratio = split.ratio.to_string();
        let mut args = vec!["pane", "split"];
        args.extend(split.pane_id);
        args.extend(["--direction", split.direction, "--ratio", &ratio]);
        if let Some(cwd) = split.cwd {
            args.extend(["--cwd", cwd]);
        }
        args.push(if split.focus { "--focus" } else { "--no-focus" });
        let output = self.run(&args, "splitting the pane")?;
        non_empty_str(
            at(output.json.as_ref(), &["result", "pane", "pane_id"])
                .or_else(|| at(output.json.as_ref(), &["pane", "pane_id"])),
        )
        .ok_or_else(|| {
            PluginError::new(
                ErrorKind::Unknown,
                "herdr pane split did not return a pane id.",
            )
            .with_output(output.stdout)
        })
    }

    /// Reads a pane's live record (`herdr pane get`); `None` when Herdr does not know the pane.
    pub fn get_pane(&self, pane_id: &str) -> Result<Option<Value>> {
        match self.run(&["pane", "get", pane_id], "reading the pane") {
            Ok(output) => Ok(at(output.json.as_ref(), &["result", "pane"])
                .filter(|pane| !pane.is_null())
                .cloned()),
            Err(error) if is_pane_not_found(&error.output) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Creates a tab and returns its id plus its root pane id.
    pub fn create_tab(
        &self,
        workspace_id: Option<&str>,
        cwd: Option<&str>,
        label: Option<&str>,
        focus: bool,
    ) -> Result<CreatedTab> {
        let mut args = vec!["tab", "create"];
        if let Some(workspace) = workspace_id {
            args.extend(["--workspace", workspace]);
        }
        if let Some(cwd) = cwd {
            args.extend(["--cwd", cwd]);
        }
        if let Some(label) = label {
            args.extend(["--label", label]);
        }
        args.push(if focus { "--focus" } else { "--no-focus" });
        let output = self.run(&args, "creating a tab")?;
        let json = output.json.as_ref();
        let tab_id = non_empty_str(
            at(json, &["result", "tab", "tab_id"]).or_else(|| at(json, &["tab", "tab_id"])),
        );
        let pane_id = non_empty_str(
            at(json, &["result", "root_pane", "pane_id"])
                .or_else(|| at(json, &["root_pane", "pane_id"])),
        );
        match (tab_id, pane_id) {
            (Some(tab_id), Some(pane_id)) => Ok(CreatedTab { tab_id, pane_id }),
            _ => Err(PluginError::new(
                ErrorKind::Unknown,
                "herdr tab create did not return a tab and pane id.",
            )
            .with_output(output.stdout)),
        }
    }

    /// Reports an agent's state on behalf of an agent Herdr cannot detect itself.
    pub fn report_agent(
        &self,
        pane_id: &str,
        source: &str,
        agent: &str,
        state: &str,
        message: Option<&str>,
    ) -> Result<()> {
        let mut args = vec![
            "pane",
            "report-agent",
            pane_id,
            "--source",
            source,
            "--agent",
            agent,
            "--state",
            state,
        ];
        if let Some(message) = message.filter(|message| !message.is_empty()) {
            args.extend(["--message", message]);
        }
        self.run(&args, "reporting the agent state").map(drop)
    }

    /// Ends the plugin's authority over a pane's agent state.
    pub fn release_agent(&self, pane_id: &str, source: &str, agent: &str) -> Result<()> {
        self.run(
            &[
                "pane",
                "release-agent",
                pane_id,
                "--source",
                source,
                "--agent",
                agent,
            ],
            "releasing the agent state",
        )
        .map(drop)
    }

    /// Renames a pane label.
    pub fn rename_pane(&self, pane_id: &str, label: &str) -> Result<()> {
        self.run(&["pane", "rename", pane_id, label], "renaming the pane")
            .map(drop)
    }

    /// Types a command into a pane and submits it.
    pub fn run_in_pane(&self, pane_id: &str, command: &str) -> Result<()> {
        self.run(
            &["pane", "run", pane_id, command],
            "running a command in the pane",
        )
        .map(drop)
    }

    /// Shows a toast. Failures are reported on stderr and never abort the action.
    pub fn notify(&self, title: &str, body: &str) -> bool {
        match self.run(
            &[
                "notification",
                "show",
                title,
                "--body",
                body,
                "--sound",
                "none",
            ],
            "showing a notification",
        ) {
            Ok(_) => true,
            Err(error) => {
                eprintln!("notification skipped: {}", error.message);
                false
            }
        }
    }

    /// Lists the ids of every pane Herdr knows, in one `herdr pane list` call.
    pub fn list_pane_ids(&self) -> Result<Vec<String>> {
        let output = self.run(&["pane", "list"], "listing panes")?;
        let json = output.json.as_ref();
        let panes = at(json, &["result", "panes"])
            .or_else(|| at(json, &["panes"]))
            .and_then(Value::as_array);
        match panes {
            Some(panes) => Ok(panes
                .iter()
                .filter_map(|pane| non_empty_str(pane.get("pane_id")))
                .collect()),
            None => Err(PluginError::new(
                ErrorKind::Unknown,
                "herdr pane list did not return a pane list.",
            )
            .with_output(output.stdout)),
        }
    }

    /// Closes a pane (`herdr pane close`).
    pub fn close_pane(&self, pane_id: &str) -> Result<()> {
        self.run(&["pane", "close", pane_id], "closing the pane")
            .map(drop)
    }

    /// Opens a manifest-declared plugin pane (used for the confirmation popup).
    pub fn open_plugin_pane(
        &self,
        plugin_id: &str,
        entrypoint_id: &str,
        env: &[(&str, &str)],
        focus: bool,
    ) -> Result<()> {
        let pairs: Vec<String> = env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        let mut args = vec![
            "plugin",
            "pane",
            "open",
            "--plugin",
            plugin_id,
            "--entrypoint",
            entrypoint_id,
        ];
        for pair in &pairs {
            args.extend(["--env", pair]);
        }
        args.push(if focus { "--focus" } else { "--no-focus" });
        self.run(&args, "opening the plugin pane").map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    /// A fake `herdr` shell script that logs its argv (one line per call) and
    /// answers `pane split`, `pane get`, `pane list` and `tab create`.
    fn fake_herdr(dir: &Path) -> (PathBuf, PathBuf) {
        let log = dir.join("log");
        let script = dir.join("herdr");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
case "$1 $2" in
  "pane split") echo '{{"result":{{"pane":{{"pane_id":"pane-new-1"}}}}}}' ;;
  "tab create") echo '{{"result":{{"tab":{{"tab_id":"tab-1"}},"root_pane":{{"pane_id":"pane-t"}}}}}}' ;;
  "pane get") if [ "$3" = "gone" ]; then echo '{{"error":{{"code":"pane_not_found"}}}}' >&2; exit 1; fi
              echo '{{"result":{{"pane":{{"pane_id":"'"$3"'","agent_status":"unknown"}}}}}}' ;;
  "pane list") echo '{{"result":{{"panes":[{{"pane_id":"a:1"}},{{"pane_id":""}},{{"x":1}},{{"pane_id":"b:2"}}]}}}}' ;;
  "pane close") echo 'plain text, not JSON' ;;
  "pane rename") echo oops >&2; exit 7 ;;
  "pane run") echo '{{"result":{{"ok":true}}}}' ;;
  "notification show") echo nope >&2; exit 1 ;;
  *) echo '{{"result":{{"ok":true}}}}' ;;
esac
"#,
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script, log)
    }

    fn client(dir: &Path) -> (HerdrClient, PathBuf) {
        let (script, log) = fake_herdr(dir);
        let env = Env::from([("PATH".to_string(), std::env::var("PATH").unwrap())]);
        (HerdrClient::new(script.to_str().unwrap(), env), log)
    }

    fn calls(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn pane_not_found_pattern() {
        for text in [
            "pane_not_found",
            "Pane abc:1 not found",
            "no such pane",
            "UNKNOWN PANE x",
        ] {
            assert!(is_pane_not_found(text), "{text}");
        }
        for text in ["pane found", "something else", ""] {
            assert!(!is_pane_not_found(text), "{text}");
        }
    }

    #[test]
    fn split_pane_builds_the_arguments_and_returns_the_new_pane() {
        let dir = tempfile::tempdir().unwrap();
        let (herdr, log) = client(dir.path());
        let id = herdr
            .split_pane(&SplitPane {
                pane_id: Some("p1"),
                direction: "right",
                ratio: 0.5,
                cwd: Some("/w"),
                focus: true,
            })
            .unwrap();
        assert_eq!(id, "pane-new-1");
        herdr
            .split_pane(&SplitPane {
                pane_id: None,
                direction: "down",
                ratio: 0.3,
                cwd: None,
                focus: false,
            })
            .unwrap();
        assert_eq!(
            calls(&log),
            [
                "pane split p1 --direction right --ratio 0.5 --cwd /w --focus",
                "pane split --direction down --ratio 0.3 --no-focus"
            ]
        );
    }

    #[test]
    fn create_tab_returns_both_ids() {
        let dir = tempfile::tempdir().unwrap();
        let (herdr, log) = client(dir.path());
        let tab = herdr
            .create_tab(Some("w1"), Some("/w"), Some("agent"), false)
            .unwrap();
        assert_eq!(
            tab,
            CreatedTab {
                tab_id: "tab-1".into(),
                pane_id: "pane-t".into()
            }
        );
        assert_eq!(
            calls(&log),
            ["tab create --workspace w1 --cwd /w --label agent --no-focus"]
        );
    }

    #[test]
    fn get_pane_reads_the_record_and_maps_pane_not_found_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let (herdr, _) = client(dir.path());
        let pane = herdr.get_pane("p9").unwrap().unwrap();
        assert_eq!(pane["pane_id"], "p9");
        assert_eq!(herdr.get_pane("gone").unwrap(), None);
    }

    #[test]
    fn list_pane_ids_skips_entries_without_an_id_and_rejects_other_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let (herdr, _) = client(dir.path());
        assert_eq!(herdr.list_pane_ids().unwrap(), ["a:1", "b:2"]);
        let shapeless = HerdrClient::new("/bin/echo", Env::new());
        let error = shapeless.list_pane_ids().unwrap_err();
        assert!(
            error.message.contains("did not return a pane list"),
            "{}",
            error.message
        );
    }

    #[test]
    fn failures_carry_the_step_the_exit_and_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let (herdr, _) = client(dir.path());
        let error = herdr.rename_pane("p1", "x").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unknown);
        assert_eq!(
            error.message,
            "herdr pane rename failed while renaming the pane (exit 7)."
        );
        assert_eq!(error.output, "oops\n");
        assert!(
            !herdr.notify("t", "b"),
            "a failed toast is reported, not raised"
        );
        assert!(herdr.close_pane("p1").is_ok(), "non-JSON output is fine");
        assert!(herdr.run_in_pane("p1", "ls").is_ok());
    }

    #[test]
    fn an_unrunnable_cli_is_a_startup_error() {
        let herdr = HerdrClient::new("/definitely/not/herdr", Env::new());
        let error = herdr.close_pane("p1").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Startup);
        assert!(
            error.message.starts_with(
                "Could not run the Herdr CLI (\"/definitely/not/herdr\") while closing the pane: "
            ),
            "{}",
            error.message
        );
    }

    #[test]
    fn report_release_and_plugin_pane_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let (herdr, log) = client(dir.path());
        herdr
            .report_agent("p1", "nono.sandbox", "opencode", "working", Some("hi"))
            .unwrap();
        herdr
            .report_agent("p1", "nono.sandbox", "opencode", "idle", None)
            .unwrap();
        herdr
            .release_agent("p1", "nono.sandbox", "opencode")
            .unwrap();
        herdr
            .open_plugin_pane("nono.sandbox", "sandboxes", &[("A", "1"), ("B", "2")], true)
            .unwrap();
        assert_eq!(
            calls(&log),
            [
                "pane report-agent p1 --source nono.sandbox --agent opencode --state working --message hi",
                "pane report-agent p1 --source nono.sandbox --agent opencode --state idle",
                "pane release-agent p1 --source nono.sandbox --agent opencode",
                "plugin pane open --plugin nono.sandbox --entrypoint sandboxes --env A=1 --env B=2 --focus",
            ]
        );
    }

    #[test]
    fn a_cancelled_client_stops_calling() {
        let token = CancelToken::new();
        token.cancel();
        let herdr = HerdrClient::new("/bin/true", Env::new()).with_cancel(token);
        assert!(herdr
            .close_pane("p")
            .unwrap_err()
            .message
            .starts_with("Cancelled"));
    }
}

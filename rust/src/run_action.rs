//! `herdr-nono run-action <action> [seconds]`: runs one plugin action through
//! Herdr and waits for its result, which `scripts/run-action.sh` wraps.
//!
//! `herdr plugin action invoke` returns as soon as Herdr has started the action;
//! the outcome lands in the plugin log a moment later. This waits for the log
//! entry of this very invocation (matched by the log id invoke prints, so an
//! older run of the same action is never mistaken for it; default 90 seconds),
//! prints the `HERDR_SANDBOX_RESULT` line and the action's stderr, and exits 0
//! when the result says ok, 1 when it does not, 2 on misuse or timeout.

use std::time::Duration;

use serde_json::Value;

use crate::constants::PLUGIN_ID;
use crate::context::Env;
use crate::exec::{run_cli, ExecError, RunOptions};

/// How long one Herdr CLI call may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// The seconds to wait, from the optional argument.
fn parse_limit(text: Option<&str>) -> u64 {
    text.and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(90)
}

/// The log id `herdr plugin action invoke` printed.
fn invoked_log_id(output: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(output).ok()?;
    parsed
        .pointer("/result/log/log_id")?
        .as_str()
        .map(str::to_string)
}

/// The finished log entry for this invocation: `None` while it is missing or
/// still running, else its first stdout line, its stderr and whether it succeeded.
struct Finished {
    first_line: String,
    stderr: String,
    ok: bool,
}

fn finished_entry(output: &str, action: &str, log_id: Option<&str>) -> Option<Finished> {
    let parsed: Value = serde_json::from_str(output).ok()?;
    let logs = parsed.pointer("/result/logs")?.as_array()?;
    let entry = logs.iter().find(|item| match log_id {
        Some(id) => item.get("log_id").and_then(Value::as_str) == Some(id),
        None => item.get("action_id").and_then(Value::as_str) == Some(action),
    })?;
    let status = entry
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status == "running" {
        return None;
    }
    let text = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let first_line = text("stdout")
        .split('\n')
        .next()
        .unwrap_or_default()
        .to_string();
    let ok = status == "succeeded" && first_line.contains("\"ok\":true");
    Some(Finished {
        first_line,
        stderr: text("stderr"),
        ok,
    })
}

fn herdr(bin: &str, env: &Env, args: &[&str]) -> Option<String> {
    let options = RunOptions {
        env: Some(env),
        cwd: None,
        timeout: CALL_TIMEOUT,
        cancel: None,
    };
    match run_cli(bin, args, &options) {
        Ok(output) if output.status == Some(0) => Some(output.stdout),
        Ok(_) | Err(ExecError::TimedOut(_) | ExecError::Cancelled(_) | ExecError::Spawn(_)) => None,
    }
}

/// Entry point; returns the exit code.
pub fn run_action_command(args: &[String], env: &Env) -> i32 {
    let Some(action) = args.first().filter(|action| !action.is_empty()) else {
        eprintln!("usage: run-action.sh <action> [seconds]");
        return 2;
    };
    let limit = parse_limit(args.get(1).map(String::as_str));
    let bin = env
        .get("HERDR_BIN_PATH")
        .filter(|bin| !bin.is_empty())
        .map_or("herdr", String::as_str);
    let Some(invoked) = herdr(
        bin,
        env,
        &["plugin", "action", "invoke", action, "--plugin", PLUGIN_ID],
    ) else {
        eprintln!("could not start {action}; is the plugin installed and enabled?");
        return 2;
    };
    let log_id = invoked_log_id(&invoked);
    for elapsed in 1..=limit {
        std::thread::sleep(Duration::from_secs(1));
        if let Some(listing) = herdr(
            bin,
            env,
            &[
                "plugin", "log", "list", "--plugin", PLUGIN_ID, "--limit", "20",
            ],
        ) {
            if let Some(done) = finished_entry(&listing, action, log_id.as_deref()) {
                if !done.stderr.trim().is_empty() {
                    eprintln!("{}", done.stderr.trim());
                }
                println!("{}", done.first_line);
                return if done.ok { 0 } else { 1 };
            }
        }
        if elapsed == 5 {
            eprintln!(
                "still running; stop waits for the agent to exit, doctor runs an escape probe"
            );
        }
    }
    eprintln!("{action} did not finish within {limit} seconds");
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limit_defaults_to_90_seconds() {
        assert_eq!(parse_limit(None), 90);
        assert_eq!(parse_limit(Some("5")), 5);
        assert_eq!(parse_limit(Some("junk")), 90);
        assert_eq!(parse_limit(Some("-3")), 90);
    }

    #[test]
    fn the_log_id_comes_from_the_invoke_result() {
        assert_eq!(
            invoked_log_id(r#"{"result":{"log":{"log_id":"plugin-log-7"}}}"#).as_deref(),
            Some("plugin-log-7")
        );
        assert_eq!(invoked_log_id("not json"), None);
        assert_eq!(invoked_log_id(r#"{"result":{}}"#), None);
    }

    fn listing(entries: &str) -> String {
        format!(r#"{{"result":{{"logs":{entries}}}}}"#)
    }

    #[test]
    fn only_the_entry_of_this_invocation_counts() {
        let output = listing(
            r#"[{"action_id":"info","log_id":"plugin-log-7","status":"succeeded","stdout":"HERDR_SANDBOX_RESULT: {\"ok\":true}\nmore\n","stderr":"note\n"},
               {"action_id":"info","log_id":"plugin-log-6","status":"succeeded","stdout":"HERDR_SANDBOX_RESULT: {\"ok\":true,\"old\":true}\n","stderr":""}]"#,
        );
        let done = finished_entry(&output, "info", Some("plugin-log-7")).unwrap();
        assert_eq!(done.first_line, "HERDR_SANDBOX_RESULT: {\"ok\":true}");
        assert_eq!(done.stderr, "note\n");
        assert!(done.ok);
        assert!(
            finished_entry(&output, "info", Some("plugin-log-9")).is_none(),
            "an older run of the action is not this one"
        );
        let by_action = finished_entry(&output, "info", None).unwrap();
        assert_eq!(
            by_action.first_line, "HERDR_SANDBOX_RESULT: {\"ok\":true}",
            "without a log id the first entry of the action is used"
        );
    }

    #[test]
    fn a_failed_or_not_ok_result_is_not_ok_and_a_running_one_is_not_finished() {
        let failed = listing(
            r#"[{"action_id":"stop","log_id":"l1","status":"failed","stdout":"HERDR_SANDBOX_RESULT: {\"ok\":false}\n","stderr":""}]"#,
        );
        assert!(!finished_entry(&failed, "stop", Some("l1")).unwrap().ok);
        let lying = listing(
            r#"[{"action_id":"stop","log_id":"l1","status":"succeeded","stdout":"HERDR_SANDBOX_RESULT: {\"ok\":false}\n","stderr":""}]"#,
        );
        assert!(
            !finished_entry(&lying, "stop", Some("l1")).unwrap().ok,
            "a succeeded process with ok:false still fails"
        );
        let running = listing(
            r#"[{"action_id":"doctor","log_id":"l1","status":"running","stdout":"","stderr":""}]"#,
        );
        assert!(finished_entry(&running, "doctor", Some("l1")).is_none());
        assert!(finished_entry("garbage", "doctor", Some("l1")).is_none());
    }

    #[test]
    fn a_missing_action_is_a_usage_error() {
        assert_eq!(run_action_command(&[], &Env::new()), 2);
        assert_eq!(run_action_command(&[String::new()], &Env::new()), 2);
    }

    #[test]
    fn an_unrunnable_herdr_is_reported() {
        let env = Env::from([(
            "HERDR_BIN_PATH".to_string(),
            "/definitely/not/herdr".to_string(),
        )]);
        assert_eq!(run_action_command(&["info".to_string()], &env), 2);
    }
}

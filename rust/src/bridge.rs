//! Runs inside the Herdr pane. Modes: `start` (first launch of the agent),
//! `connect` (launch it again with the adapter's resume arguments) and `shell`
//! (a shell in a sandbox with the agent's policy).
//! Invoked through `herdr-nono bridge <mode> --state-dir DIR --config-dir DIR
//! --pane-id ID --plugin-root DIR [--herdr-bin BIN] [--nono-bin BIN] [--launch-id ID]`.

use std::io::Write;

use crate::config::load_config;
use crate::context::Env;
use crate::errors::{ErrorKind, PluginError, Result};
use crate::herdr::HerdrClient;
use crate::lifecycle::{Lifecycle, LifecycleOptions};
use crate::nono::NonoClient;

const MODES: [&str; 3] = ["start", "connect", "shell"];

/// The parsed bridge argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeArgs {
    pub mode: String,
    pub state_dir: String,
    pub config_dir: String,
    pub pane_id: String,
    pub plugin_root: String,
    pub herdr_bin: String,
    pub nono_bin: Option<String>,
    pub launch_id: Option<String>,
}

/// Parses the bridge argv.
pub fn parse_bridge_args(argv: &[String]) -> Result<BridgeArgs> {
    let startup = |message: String| PluginError::new(ErrorKind::Startup, message);
    let mode = argv.first().map_or("undefined", String::as_str);
    if !MODES.contains(&mode) {
        return Err(startup(format!(
            "Unknown bridge mode \"{mode}\". Expected one of {}.",
            MODES.join(", ")
        )));
    }
    let (mut state_dir, mut config_dir, mut pane_id, mut plugin_root) = (None, None, None, None);
    let (mut herdr_bin, mut nono_bin, mut launch_id) = ("herdr".to_string(), None, None);
    let rest = &argv[1..];
    for pair in rest.chunks(2) {
        let (flag, value) = (pair[0].as_str(), pair.get(1));
        let Some(value) = value else {
            return Err(startup(format!("Unexpected bridge argument \"{flag}\".")));
        };
        match flag {
            "--state-dir" => state_dir = Some(value.clone()),
            "--config-dir" => config_dir = Some(value.clone()),
            "--pane-id" => pane_id = Some(value.clone()),
            "--plugin-root" => plugin_root = Some(value.clone()),
            "--herdr-bin" => herdr_bin = value.clone(),
            "--nono-bin" => nono_bin = Some(value.clone()),
            "--launch-id" => launch_id = Some(value.clone()),
            other => return Err(startup(format!("Unexpected bridge argument \"{other}\"."))),
        }
    }
    let require = |value: Option<String>, name: &str| {
        value
            .filter(|text| !text.is_empty())
            .ok_or_else(|| startup(format!("Missing bridge option for {name}.")))
    };
    Ok(BridgeArgs {
        mode: mode.to_string(),
        state_dir: require(state_dir, "stateDir")?,
        config_dir: require(config_dir, "configDir")?,
        pane_id: require(pane_id, "paneId")?,
        plugin_root: require(plugin_root, "pluginRoot")?,
        herdr_bin,
        nono_bin,
        launch_id,
    })
}

/// A line on stdout, flushed at once so it interleaves correctly with the agent's output.
fn print_line(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn run(argv: &[String], env: &Env) -> Result<i32> {
    let args = parse_bridge_args(argv)?;
    let mut loaded = load_config(std::path::Path::new(&args.config_dir), env)?;
    if let Some(nono_bin) = &args.nono_bin {
        // The action already resolved the executable; the pane's shell may not have the same environment.
        loaded.nono_bin = nono_bin.clone();
        loaded.config.nono_bin = Some(nono_bin.clone());
    }
    let herdr = HerdrClient::new(args.herdr_bin.clone(), env.clone());
    let nono = NonoClient::new(loaded.nono_bin.clone(), env.clone());
    let mut options = LifecycleOptions::new(
        &args.state_dir,
        loaded.config,
        args.plugin_root.clone(),
        nono,
        env.clone(),
    );
    options.log = Box::new(print_line);
    options.herdr = Some(herdr);
    let lifecycle = Lifecycle::new(options);
    if args.mode == "shell" {
        return lifecycle.shell(&args.pane_id);
    }
    // Actions wait for this acknowledgement, and the pid it records marks the
    // mapping busy until this process gives it back on the way out.
    lifecycle.acknowledge_bridge(&args.pane_id, args.launch_id.as_deref())?;
    let outcome = lifecycle.launch(&args.pane_id, args.mode == "connect");
    lifecycle.release_bridge(&args.pane_id);
    Ok(outcome?.exit_code)
}

/// Entry point; returns the process exit code.
pub fn run_bridge(argv: &[String], env: &Env) -> i32 {
    match run(argv, env) {
        Ok(code) => code,
        Err(error) => {
            print_line(&format!("error: {}", error.message));
            if !error.output.trim().is_empty() {
                print_line(error.output.trim());
            }
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn parse_bridge_args_requires_the_plugin_root_and_rejects_unknown_options() {
        let parsed = parse_bridge_args(&words(&[
            "start",
            "--state-dir",
            "s",
            "--config-dir",
            "c",
            "--pane-id",
            "p",
            "--plugin-root",
            "r",
        ]))
        .unwrap();
        assert_eq!(parsed.plugin_root, "r");
        assert_eq!(
            (parsed.herdr_bin.as_str(), parsed.nono_bin, parsed.launch_id),
            ("herdr", None, None)
        );
        let missing = parse_bridge_args(&words(&[
            "start",
            "--state-dir",
            "s",
            "--config-dir",
            "c",
            "--pane-id",
            "p",
        ]))
        .unwrap_err();
        assert_eq!(missing.message, "Missing bridge option for pluginRoot.");
        assert_eq!(missing.kind, ErrorKind::Startup);
        assert_eq!(
            parse_bridge_args(&words(&["start", "--sbx-bin", "x"]))
                .unwrap_err()
                .message,
            "Unexpected bridge argument \"--sbx-bin\"."
        );
        assert_eq!(
            parse_bridge_args(&words(&["start", "--state-dir"]))
                .unwrap_err()
                .message,
            "Unexpected bridge argument \"--state-dir\"."
        );
        assert_eq!(
            parse_bridge_args(&words(&["attach"])).unwrap_err().message,
            "Unknown bridge mode \"attach\". Expected one of start, connect, shell."
        );
        assert_eq!(parse_bridge_args(&[]).unwrap_err().kind, ErrorKind::Startup);
    }

    #[test]
    fn the_optional_arguments_are_read() {
        let parsed = parse_bridge_args(&words(&[
            "connect",
            "--state-dir",
            "s",
            "--config-dir",
            "c",
            "--pane-id",
            "p",
            "--plugin-root",
            "r",
            "--herdr-bin",
            "/h",
            "--nono-bin",
            "/n",
            "--launch-id",
            "abc",
        ]))
        .unwrap();
        assert_eq!(parsed.mode, "connect");
        assert_eq!(
            (
                parsed.herdr_bin.as_str(),
                parsed.nono_bin.as_deref(),
                parsed.launch_id.as_deref()
            ),
            ("/h", Some("/n"), Some("abc"))
        );
    }
}

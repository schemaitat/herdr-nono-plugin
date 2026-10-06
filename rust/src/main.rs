//! The herdr-nono plugin binary: actions, the in-pane bridge, the event hook
//! and the sandboxes overlay, one subcommand each.

// The foundation modules are consumed by the clients, lifecycle and actions
// that later phases of the rewrite add; drop this once the actions use them.
#![allow(dead_code)]

mod agents;
mod bridge;
mod config;
mod constants;
mod context;
mod describe;
mod errors;
mod events;
mod exec;
mod herdr;
mod hostservice;
mod lifecycle;
mod naming;
mod nono;
mod probes;
mod procfs;
mod result;
mod shell;
mod signals;
mod state;
mod util;
mod verify;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "herdr-nono",
    version,
    about = "Run coding agents inside nono sandboxes from Herdr."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Arguments are passed through untouched: each subcommand parses its own.
#[derive(clap::Args)]
struct Passthrough {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the plugin action named by HERDR_PLUGIN_ACTION_ID.
    Action(Passthrough),
    /// Run inside a Herdr pane: `start`, `connect` or `shell`.
    Bridge(Passthrough),
    /// Handle the Herdr event named by HERDR_PLUGIN_EVENT.
    Events(Passthrough),
    /// Show the interactive sandboxes overlay.
    Pane(Passthrough),
    /// Run the escape probe checks; only meaningful inside a sandbox.
    #[command(hide = true)]
    Probe(Passthrough),
    /// Run the escape probe in a throwaway sandbox and print the checks as JSON.
    #[command(hide = true)]
    ProbeRun {
        /// The nono executable.
        #[arg(long, default_value = "nono")]
        nono_bin: String,
        /// The nono profile name or file the probe runs under.
        #[arg(long)]
        profile: String,
        /// Run this executable inside the sandbox instead of a copy of this binary.
        #[arg(long)]
        probe_exe: Option<std::path::PathBuf>,
    },
    /// Print the action ids, config keys and constants as JSON.
    #[command(hide = true)]
    Describe,
}

fn not_implemented(name: &str) -> ExitCode {
    eprintln!("herdr-nono {name}: not implemented yet");
    ExitCode::FAILURE
}

/// Exits with the code of a finished command, the way the Node scripts set `process.exitCode`.
fn exit_with(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code.rem_euclid(256)).unwrap_or(1))
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Describe => {
            println!(
                "{}",
                serde_json::to_string_pretty(&describe::describe())
                    .expect("a JSON value serialises")
            );
            ExitCode::SUCCESS
        }
        Command::Action(_) => not_implemented("action"),
        Command::Bridge(passthrough) => exit_with(bridge::run_bridge(
            &passthrough.args,
            &context::process_env(),
        )),
        Command::Events(_) => exit_with(events::handle_event(&context::process_env())),
        Command::Pane(_) => not_implemented("pane"),
        Command::Probe(passthrough) => probes::probe_main(&passthrough.args),
        Command::ProbeRun {
            nono_bin,
            profile,
            probe_exe,
        } => {
            let env = context::process_env();
            let mut options = probes::ProbeOptions::new(&nono_bin, &profile, &env);
            options.probe_exe = probe_exe.as_deref();
            match probes::run_probes(&options) {
                Ok(run) => {
                    println!(
                        "{}",
                        serde_json::to_string(&run).expect("a probe run serialises")
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("{}", error.message);
                    if !error.output.trim().is_empty() {
                        eprintln!("{}", error.output.trim());
                    }
                    ExitCode::FAILURE
                }
            }
        }
    }
}

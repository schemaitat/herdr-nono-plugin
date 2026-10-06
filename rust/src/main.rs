//! The herdr-nono plugin binary: actions, the in-pane bridge, the event hook
//! and the sandboxes overlay, one subcommand each.

// The foundation modules are consumed by the clients, lifecycle and actions
// that later phases of the rewrite add; drop this once the actions use them.
#![allow(dead_code)]

mod config;
mod constants;
mod context;
mod describe;
mod errors;
mod naming;
mod procfs;
mod result;
mod shell;
mod state;
mod util;

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
    /// Run the escape probe; only meaningful inside a sandbox.
    #[command(hide = true)]
    Probe(Passthrough),
    /// Print the action ids, config keys and constants as JSON.
    #[command(hide = true)]
    Describe,
}

fn not_implemented(name: &str) -> ExitCode {
    eprintln!("herdr-nono {name}: not implemented yet");
    ExitCode::FAILURE
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
        Command::Bridge(_) => not_implemented("bridge"),
        Command::Events(_) => not_implemented("events"),
        Command::Pane(_) => not_implemented("pane"),
        Command::Probe(_) => not_implemented("probe"),
    }
}

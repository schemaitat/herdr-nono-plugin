//! The sandboxes overlay: an interactive terminal UI listing every mapping with
//! its pane, nono sandboxes and verification, a details panel for the selected
//! one, and keys to jump, verify, stop, prune and clean up all. Refreshes until `q`. Built on
//! ratatui; the screen is in [`ui`], the key handling in [`app`], and the
//! commands that talk to nono and Herdr run on a [`worker`] thread.

pub mod app;
pub mod model;
pub mod ui;
pub mod worker;

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Terminal;

use self::app::{App, Effect, Key};
use self::model::{collect_sandboxes, nono_user_profiles_dir, CollectInput};
use self::ui::{draw, DrawContext, ProfilesInfo, ViewState};
use self::worker::{Msg, Worker, WorkerConfig, OVERLAY_NONO_TIMEOUT};
use crate::agents::resolve_agent;
use crate::config::{config_path, load_config, PluginConfig};
use crate::context::{process_env, read_plugin_env, require_plugin_dirs, Env};
use crate::errors::Result;
use crate::herdr::HerdrClient;
use crate::hostservice::home_dir;
use crate::nono::NonoClient;
use crate::util::local_clock;

/// How often the overlay refreshes.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

/// How long a startup error stays on screen.
pub const HOLD: Duration = Duration::from_secs(15);

/// How long quitting waits for the worker to stop its commands.
const QUIT_GRACE: Duration = Duration::from_millis(150);

fn clock_now() -> String {
    local_clock(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64),
    )
}

/// Renders the screen to plain text lines on a `width` x `height` grid.
pub fn render_lines(view: &ViewState, ctx: &DrawContext, width: u16, height: u16) -> Vec<String> {
    let mut terminal =
        Terminal::new(TestBackend::new(width, height)).expect("a test backend cannot fail");
    terminal
        .draw(|frame| draw(frame, view, ctx))
        .expect("a test backend cannot fail");
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// What the overlay was started with.
struct Setup {
    state_dir: PathBuf,
    config: PluginConfig,
    nono_bin: String,
    herdr_bin: String,
    plugin_root: String,
    env: Env,
    profiles: ProfilesInfo,
    configured: Option<(Option<String>, Option<String>)>,
}

fn setup(env: &Env) -> Result<Setup> {
    let plugin = require_plugin_dirs(read_plugin_env(env))?;
    let loaded = load_config(Path::new(&plugin.config_dir), env)?;
    // An unknown agentKind leaves the configured profiles unknown; the profiles view says so.
    let configured = resolve_agent(&loaded.config, &plugin.env.plugin_root)
        .ok()
        .map(|agent| {
            (
                Some(agent.profile_ref.clone()),
                agent.server_profile_ref.clone(),
            )
        });
    Ok(Setup {
        state_dir: PathBuf::from(&plugin.state_dir),
        profiles: ProfilesInfo {
            plugin_root: Some(plugin.env.plugin_root.clone()),
            config_file: Some(
                config_path(Path::new(&plugin.config_dir))
                    .to_string_lossy()
                    .into_owned(),
            ),
            user_profiles_dir: nono_user_profiles_dir(env),
        },
        nono_bin: loaded.nono_bin,
        herdr_bin: plugin.env.herdr_bin.clone(),
        plugin_root: plugin.env.plugin_root,
        config: loaded.config,
        env: env.clone(),
        configured,
    })
}

/// Keeps an error on screen until a key is pressed or the time is up, because an
/// overlay closes together with its process.
fn hold_for_key(hold: Duration) {
    println!(
        "press any key to close (closes by itself in {}s)",
        hold.as_secs()
    );
    let _ = std::io::stdout().flush();
    if !std::io::stdin().is_terminal() {
        return;
    }
    if ratatui::crossterm::terminal::enable_raw_mode().is_err() {
        return;
    }
    let deadline = Instant::now() + hold;
    while Instant::now() < deadline {
        match event::poll(Duration::from_millis(100)) {
            Ok(true) => {
                if matches!(event::read(), Ok(Event::Key(_))) {
                    break;
                }
            }
            Ok(false) => {}
            Err(_) => break,
        }
    }
    let _ = ratatui::crossterm::terminal::disable_raw_mode();
}

fn to_key(code: KeyCode, modifiers: KeyModifiers) -> Option<Key> {
    match code {
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        KeyCode::PageUp => Some(Key::PageUp),
        KeyCode::PageDown => Some(Key::PageDown),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Esc => Some(Key::Escape),
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => Some(Key::CtrlC),
        KeyCode::Char(c) => Some(Key::Char(c.to_lowercase().next().unwrap_or(c))),
        _ => None,
    }
}

fn draw_context(setup_home: &str) -> DrawContext {
    DrawContext {
        clock: clock_now(),
        interval_secs: REFRESH_INTERVAL.as_secs(),
        home: setup_home.to_string(),
    }
}

/// The size of the frame `--once` prints: the terminal's when stdout is one,
/// else `COLUMNS` and `LINES`, else 110 x 32.
fn frame_size(env: &Env) -> (u16, u16) {
    if std::io::stdout().is_terminal() {
        if let Ok((width, height)) = ratatui::crossterm::terminal::size() {
            if width > 0 && height > 0 {
                return (width, height);
            }
        }
    }
    size_from_env(env)
}

/// `COLUMNS` and `LINES`, else 110 x 32.
fn size_from_env(env: &Env) -> (u16, u16) {
    let from_env = |name: &str, fallback: u16| {
        env.get(name)
            .and_then(|value| value.trim().parse::<u16>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(fallback)
    };
    (from_env("COLUMNS", 110), from_env("LINES", 32))
}

/// Renders a single plain frame to stdout: what `--once` prints.
fn run_once(setup: &Setup) -> i32 {
    let nono = NonoClient::new(setup.nono_bin.clone(), setup.env.clone())
        .with_timeout(OVERLAY_NONO_TIMEOUT);
    let herdr = HerdrClient::new(setup.herdr_bin.clone(), setup.env.clone());
    let mut view = ViewState::new(setup.profiles.clone());
    let mut cache = HashMap::new();
    match collect_sandboxes(CollectInput {
        state_dir: &setup.state_dir,
        nono: &nono,
        herdr: &herdr,
        proc_root: Path::new(crate::procfs::PROC_ROOT),
        profile_cache: &mut cache,
        configured: setup.configured.clone(),
    }) {
        Ok(data) => view.apply(data),
        Err(error) => {
            view.status = Some(ui::Status::new(
                ui::StatusKind::Error,
                format!("could not read the sandboxes: {}", error.message),
            ))
        }
    }
    let home = home_dir(&setup.env).to_string_lossy().into_owned();
    let (width, height) = frame_size(&setup.env);
    let lines = render_lines(&view, &draw_context(&home), width, height);
    println!("{}", lines.join("\n"));
    0
}

/// Restores the terminal when the overlay ends, however it ends.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

fn run_interactive(setup: Setup) -> i32 {
    let home = home_dir(&setup.env).to_string_lossy().into_owned();
    let mut worker = Worker::spawn(WorkerConfig {
        state_dir: setup.state_dir.clone(),
        config: setup.config.clone(),
        nono_bin: setup.nono_bin.clone(),
        herdr_bin: setup.herdr_bin.clone(),
        plugin_root: setup.plugin_root.clone(),
        env: setup.env.clone(),
        interval: REFRESH_INTERVAL,
        proc_root: PathBuf::from(crate::procfs::PROC_ROOT),
        configured: setup.configured.clone(),
    });
    let mut app = App::new(ViewState::new(setup.profiles.clone()));
    // Alternate screen, raw input and a hidden cursor; all restored on the way out.
    let mut terminal = ratatui::init();
    let _guard = TerminalGuard;
    let mut last_clock = String::new();
    let mut dirty = true;
    'event_loop: loop {
        let ctx = draw_context(&home);
        if dirty || ctx.clock != last_clock {
            if terminal.draw(|frame| draw(frame, &app.view, &ctx)).is_err() {
                break;
            }
            last_clock = ctx.clock.clone();
            dirty = false;
        }
        match event::poll(Duration::from_millis(50)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    if let Some(key) = to_key(key.code, key.modifiers) {
                        match app.on_key(key) {
                            Effect::Quit => break 'event_loop,
                            Effect::Start(job) => worker.submit(job),
                            Effect::None => {}
                        }
                        dirty = true;
                    }
                }
                Ok(Event::Resize(..)) => dirty = true,
                Ok(_) => {}
                Err(_) => break,
            },
            Ok(false) => {}
            Err(_) => break,
        }
        while let Ok(message) = worker.results.try_recv() {
            match message {
                Msg::Snapshot(snapshot) => app.on_snapshot(*snapshot),
                Msg::JobDone(status) => app.on_job_done(status),
                Msg::Jumped => break 'event_loop,
            }
            dirty = true;
        }
    }
    // Cancel what is in flight before the terminal is handed back.
    worker.shutdown(QUIT_GRACE);
    0
}

/// Shows an unexpected crash on screen for a moment, because Herdr closes the
/// overlay as soon as this process exits.
fn install_crash_hold() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        println!("the sandboxes overlay crashed: {info}");
        hold_for_key(HOLD);
        previous(info);
    }));
}

/// Entry point of `herdr-nono pane [--once]`; returns the exit code.
pub fn run_pane(args: &[String], env: &Env) -> i32 {
    let once = args.iter().any(|arg| arg == "--once");
    let setup = match setup(env) {
        Ok(setup) => setup,
        Err(error) => {
            println!("{}", error.message);
            if !once {
                hold_for_key(HOLD);
            }
            return 1;
        }
    };
    if once {
        return run_once(&setup);
    }
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        // Not a terminal (a script or a pipe): one plain frame is the useful thing to print.
        return run_once(&setup);
    }
    install_crash_hold();
    run_interactive(setup)
}

/// The process environment, for the entry point in main.
pub fn run_pane_from_process(args: &[String]) -> i32 {
    run_pane(args, &process_env())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fallback_frame_size_comes_from_the_environment() {
        let env = |pairs: &[(&str, &str)]| -> Env {
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect()
        };
        // Under `cargo test` stdout is not a terminal, so the environment decides.
        assert_eq!(
            size_from_env(&env(&[("COLUMNS", "200"), ("LINES", "50")])),
            (200, 50)
        );
        assert_eq!(size_from_env(&env(&[])), (110, 32));
        assert_eq!(
            size_from_env(&env(&[("COLUMNS", "junk"), ("LINES", "0")])),
            (110, 32)
        );
    }
}

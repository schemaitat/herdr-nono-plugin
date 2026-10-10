//! The overlay's state machine: keys in, effects out. It owns the view state and
//! never touches the terminal or runs a command, so every key flow is testable
//! by feeding keys and the results of the jobs they start.

use super::model::{is_stale, Collected};
use super::ui::{Mode, Status, StatusKind, ViewState};
use crate::verify::summarize_verification;

/// A key the overlay understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    PageUp,
    PageDown,
    Enter,
    Escape,
    CtrlC,
    /// A character, lower-cased.
    Char(char),
}

/// Work that talks to nono or Herdr, run off the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    Refresh,
    Verify {
        pane_id: String,
        session_name: String,
    },
    Stop {
        pane_id: String,
        session_name: String,
    },
    Prune {
        pane_ids: Vec<String>,
    },
    /// Bring the agent's pane into view; the overlay closes when it worked.
    Jump {
        pane_id: String,
    },
    /// Stop every running agent, then forget every mapping.
    CleanAll {
        running: Vec<(String, String)>,
    },
}

impl Job {
    /// What the status line says while the job runs.
    pub fn label(&self) -> String {
        match self {
            Job::Refresh => "refreshing…".to_string(),
            Job::Verify { session_name, .. } => format!("verifying {session_name}…"),
            Job::Stop { session_name, .. } => format!("stopping {session_name}…"),
            Job::Prune { .. } => "pruning…".to_string(),
            Job::Jump { pane_id } => format!("jumping to {pane_id}…"),
            Job::CleanAll { .. } => "cleaning up…".to_string(),
        }
    }
}

/// What the event loop must do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    Quit,
    /// Run the job; the status line already says so.
    Start(Job),
}

pub struct App {
    pub view: ViewState,
    pending: Option<Job>,
    busy: bool,
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

impl App {
    pub fn new(view: ViewState) -> Self {
        App {
            view,
            pending: None,
            busy: false,
        }
    }

    #[cfg(test)]
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    fn say(&mut self, kind: StatusKind, text: impl Into<String>) {
        self.view.status = Some(Status::new(kind, text));
    }

    fn start(&mut self, job: Job) -> Effect {
        self.busy = true;
        self.say(StatusKind::Info, job.label());
        Effect::Start(job)
    }

    fn ask(&mut self, question: String, job: Job) {
        self.view.prompt = Some(format!("{question} [y/N]"));
        self.pending = Some(job);
    }

    /// Handles one key.
    pub fn on_key(&mut self, key: Key) -> Effect {
        if matches!(key, Key::Char('q') | Key::CtrlC) {
            return Effect::Quit;
        }
        if self.view.prompt.is_some() {
            let job = self.pending.take();
            self.view.prompt = None;
            return match (key, job) {
                (Key::Char('y'), Some(job)) => self.start(job),
                _ => {
                    self.say(StatusKind::Info, "cancelled");
                    Effect::None
                }
            };
        }
        if self.view.help {
            if matches!(key, Key::Char('?') | Key::Escape | Key::Enter) {
                self.view.help = false;
            }
            return Effect::None;
        }
        // Moving around stays possible while a job runs; only starting another one waits.
        if self.busy
            && matches!(
                key,
                Key::Enter | Key::Char('g' | 'r' | 'v' | 'x' | 'p' | 'a')
            )
        {
            return Effect::None;
        }
        let last = self.view.rows.len().saturating_sub(1);
        match key {
            Key::Up | Key::Char('k') => self.view.selected = self.view.selected.saturating_sub(1),
            Key::Down | Key::Char('j') => self.view.selected = (self.view.selected + 1).min(last),
            Key::PageUp => self.view.selected = self.view.selected.saturating_sub(5),
            Key::PageDown => self.view.selected = (self.view.selected + 5).min(last),
            Key::Char('?') => self.view.help = true,
            Key::Enter | Key::Char('g') => {
                let Some(row) = self.view.selected_row() else {
                    return Effect::None;
                };
                let (pane_id, session_name) = (row.pane_id.clone(), row.session_name.clone());
                if row.pane_exists == Some(false) {
                    self.say(
                        StatusKind::Error,
                        format!("pane {pane_id} of {session_name} is gone; nothing to jump to"),
                    );
                } else {
                    return self.start(Job::Jump { pane_id });
                }
            }
            Key::Char('a') => {
                if self.view.rows.is_empty() {
                    self.say(StatusKind::Info, "nothing to clean up: no mappings");
                    return Effect::None;
                }
                let running: Vec<(String, String)> = self
                    .view
                    .rows
                    .iter()
                    .filter(|row| row.running == Some(true) || row.server_running == Some(true))
                    .map(|row| (row.pane_id.clone(), row.session_name.clone()))
                    .collect();
                let total = self.view.rows.len();
                self.ask(
                    format!(
                        "Stop {} running agent{} and forget all {total} mapping{}?",
                        running.len(),
                        plural(running.len()),
                        plural(total)
                    ),
                    Job::CleanAll { running },
                );
            }
            Key::Char('i') => {
                self.view.mode = if self.view.mode == Mode::Profiles {
                    Mode::Sandboxes
                } else {
                    Mode::Profiles
                };
                self.view.status = None;
            }
            Key::Char('r') => return self.start(Job::Refresh),
            Key::Char('v') => {
                let Some(row) = self.view.selected_row() else {
                    return Effect::None;
                };
                let (pane_id, session_name, running) = (
                    row.pane_id.clone(),
                    row.session_name.clone(),
                    row.running == Some(true),
                );
                if !running {
                    self.say(
                        StatusKind::Error,
                        format!("{session_name} is not running; nothing to verify"),
                    );
                } else {
                    return self.start(Job::Verify {
                        pane_id,
                        session_name,
                    });
                }
            }
            Key::Char('x') => {
                let Some(row) = self.view.selected_row() else {
                    return Effect::None;
                };
                let (pane_id, session_name) = (row.pane_id.clone(), row.session_name.clone());
                if row.running != Some(true) && row.server_running != Some(true) {
                    self.say(StatusKind::Error, format!("{session_name} is not running"));
                } else {
                    self.ask(
                        format!("Stop {session_name} (pane {pane_id})?"),
                        Job::Stop {
                            pane_id,
                            session_name,
                        },
                    );
                }
            }
            Key::Char('p') => {
                let stale: Vec<String> = self
                    .view
                    .rows
                    .iter()
                    .filter(|row| is_stale(row))
                    .map(|row| row.pane_id.clone())
                    .collect();
                let Some(pane_ids) = self.view.pane_ids.clone() else {
                    self.say(
                        StatusKind::Error,
                        "Herdr's pane list is unavailable, so nothing can be pruned",
                    );
                    return Effect::None;
                };
                if stale.is_empty() {
                    self.say(
                        StatusKind::Info,
                        "nothing to prune: every mapping has a pane or something running",
                    );
                } else {
                    self.ask(
                        format!(
                            "Forget {} mapping{} whose pane is gone ({})?",
                            stale.len(),
                            plural(stale.len()),
                            stale.join(", ")
                        ),
                        Job::Prune { pane_ids },
                    );
                }
            }
            _ => {}
        }
        Effect::None
    }

    /// A job finished: its result goes on the status line and the next frame is
    /// fetched by the worker.
    pub fn on_job_done(&mut self, status: Status) {
        self.busy = false;
        self.view.status = Some(status);
    }

    /// A fresh frame arrived.
    pub fn on_snapshot(&mut self, snapshot: Result<Collected, String>) {
        match snapshot {
            Ok(data) => self.view.apply(data),
            Err(message) => self.say(
                StatusKind::Error,
                format!("could not read the sandboxes: {message}"),
            ),
        }
    }
}

/// The status line for a finished verification.
pub fn verify_status(session_name: &str, report: &crate::verify::VerificationReport) -> Status {
    let text = format!(
        "{session_name}: {}",
        summarize_verification(serde_json::to_value(report).ok().as_ref())
    );
    Status::new(
        if report.ok == Some(false) {
            StatusKind::Error
        } else {
            StatusKind::Ok
        },
        text,
    )
}

/// The status line for a finished clean-up.
pub fn clean_status(stopped: usize, forgotten: usize, failures: &[String]) -> Status {
    let text = format!(
        "stopped {stopped} agent{}, forgot {forgotten} mapping{}",
        plural(stopped),
        plural(forgotten)
    );
    if failures.is_empty() {
        Status::new(StatusKind::Ok, text)
    } else {
        Status::new(
            StatusKind::Error,
            format!("{text}; {}", failures.join("; ")),
        )
    }
}

/// The status line for a finished prune.
pub fn prune_status(pruned: usize, kept_reason: Option<(usize, &str)>) -> Status {
    let kept = kept_reason.map_or_else(String::new, |(count, reason)| {
        format!(", kept {count} ({reason})")
    });
    Status::new(
        StatusKind::Ok,
        format!("pruned {pruned} mapping{}{kept}", plural(pruned)),
    )
}

#[cfg(test)]
mod tests {
    use super::super::model::{Networks, Row, Trees};
    use super::super::ui::ProfilesInfo;
    use super::*;
    use crate::state::Entry;

    fn row(
        pane: &str,
        session: &str,
        running: Option<bool>,
        server: Option<bool>,
        pane_exists: Option<bool>,
    ) -> Row {
        Row {
            pane_id: pane.into(),
            pane_exists,
            session_name: session.into(),
            agent_kind: "opencode".into(),
            lifecycle_state: "exited".into(),
            running,
            server_running: server,
            shells: Some(0),
            verified: None,
            verification: None,
            local_path: "/w".into(),
            trees: Trees::default(),
            network: Networks::default(),
            entry: Entry::new(),
        }
    }

    fn app_with(rows: Vec<Row>, pane_ids: Option<Vec<&str>>) -> App {
        let mut app = App::new(ViewState::new(ProfilesInfo::default()));
        app.on_snapshot(Ok(Collected {
            rows,
            session_error: None,
            pane_ids: pane_ids.map(|ids| ids.into_iter().map(String::from).collect()),
            configured: None,
        }));
        app
    }

    fn two_idle() -> Vec<Row> {
        vec![
            row("w1:p1", "s-1", Some(false), Some(false), Some(true)),
            row("w1:p2", "s-2", Some(false), Some(false), Some(false)),
        ]
    }

    fn status(app: &App) -> String {
        app.view
            .status
            .as_ref()
            .map(|status| status.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn q_and_ctrl_c_quit_even_over_a_prompt() {
        let mut app = app_with(two_idle(), Some(vec!["w1:p1"]));
        assert_eq!(app.on_key(Key::Char('q')), Effect::Quit);
        assert_eq!(app.on_key(Key::CtrlC), Effect::Quit);
        app.on_key(Key::Char('p'));
        assert!(app.view.prompt.is_some());
        assert_eq!(app.on_key(Key::Char('q')), Effect::Quit);
    }

    #[test]
    fn the_selection_moves_and_stops_at_the_ends() {
        let mut app = app_with(two_idle(), Some(vec![]));
        app.on_key(Key::Down);
        assert_eq!(app.view.selected, 1);
        app.on_key(Key::Char('j'));
        assert_eq!(app.view.selected, 1, "stops at the last row");
        app.on_key(Key::Char('k'));
        assert_eq!(app.view.selected, 0);
        app.on_key(Key::Up);
        assert_eq!(app.view.selected, 0);
        app.on_key(Key::PageDown);
        assert_eq!(app.view.selected, 1);
        app.on_key(Key::PageUp);
        assert_eq!(app.view.selected, 0);
        let mut empty = app_with(Vec::new(), None);
        empty.on_key(Key::Down);
        empty.on_key(Key::Char('v'));
        empty.on_key(Key::Char('x'));
        assert_eq!(empty.view.selected, 0);
        assert_eq!(empty.on_key(Key::Char('x')), Effect::None);
    }

    #[test]
    fn pruning_asks_first_and_cancels_without_a_yes() {
        let mut app = app_with(two_idle(), Some(vec!["w1:p1"]));
        assert_eq!(app.on_key(Key::Char('p')), Effect::None);
        assert_eq!(
            app.view.prompt.as_deref(),
            Some("Forget 1 mapping whose pane is gone (w1:p2)? [y/N]")
        );
        assert_eq!(app.on_key(Key::Char('n')), Effect::None);
        assert_eq!(
            (app.view.prompt.clone(), status(&app).as_str()),
            (None, "cancelled")
        );
        app.on_key(Key::Char('p'));
        assert_eq!(app.on_key(Key::Escape), Effect::None);
        assert_eq!(status(&app), "cancelled");
        app.on_key(Key::Char('p'));
        let effect = app.on_key(Key::Char('y'));
        assert_eq!(
            effect,
            Effect::Start(Job::Prune {
                pane_ids: vec!["w1:p1".into()]
            })
        );
        assert!(app.is_busy());
        assert_eq!(status(&app), "pruning…");
        app.on_job_done(prune_status(1, None));
        assert!(!app.is_busy());
        assert_eq!(status(&app), "pruned 1 mapping");
    }

    #[test]
    fn pruning_says_why_it_cannot() {
        let mut no_panes = app_with(two_idle(), None);
        no_panes.on_key(Key::Char('p'));
        assert_eq!(
            status(&no_panes),
            "Herdr's pane list is unavailable, so nothing can be pruned"
        );
        assert!(no_panes.view.prompt.is_none());
        let mut nothing = app_with(
            vec![row("w1:p1", "s-1", Some(false), Some(false), Some(true))],
            Some(vec!["w1:p1"]),
        );
        nothing.on_key(Key::Char('p'));
        assert_eq!(
            status(&nothing),
            "nothing to prune: every mapping has a pane or something running"
        );
        assert_eq!(
            prune_status(2, Some((1, "its agent or shell still runs; stop it first"))).text,
            "pruned 2 mappings, kept 1 (its agent or shell still runs; stop it first)"
        );
    }

    #[test]
    fn verify_and_stop_refuse_an_agent_that_is_not_running() {
        let mut app = app_with(two_idle(), Some(vec![]));
        assert_eq!(app.on_key(Key::Char('v')), Effect::None);
        assert_eq!(status(&app), "s-1 is not running; nothing to verify");
        assert_eq!(app.view.status.as_ref().unwrap().kind, StatusKind::Error);
        app.on_key(Key::Char('x'));
        assert_eq!(status(&app), "s-1 is not running");
        assert!(app.view.prompt.is_none());
    }

    #[test]
    fn a_running_agent_is_verified_at_once_and_stopped_after_a_yes() {
        let mut app = app_with(
            vec![row("w1:p1", "s-1", Some(true), Some(false), Some(true))],
            Some(vec!["w1:p1"]),
        );
        let effect = app.on_key(Key::Char('v'));
        assert_eq!(
            effect,
            Effect::Start(Job::Verify {
                pane_id: "w1:p1".into(),
                session_name: "s-1".into()
            })
        );
        assert_eq!(status(&app), "verifying s-1…");
        assert_eq!(
            app.on_key(Key::Char('x')),
            Effect::None,
            "job keys are ignored while a job runs"
        );
        app.on_key(Key::Char('?'));
        assert!(app.view.help, "but the help and the selection still work");
        app.on_key(Key::Escape);
        app.on_job_done(Status::new(StatusKind::Ok, "s-1: confined"));
        assert_eq!(app.on_key(Key::Char('x')), Effect::None);
        assert_eq!(
            app.view.prompt.as_deref(),
            Some("Stop s-1 (pane w1:p1)? [y/N]")
        );
        assert_eq!(
            app.on_key(Key::Char('y')),
            Effect::Start(Job::Stop {
                pane_id: "w1:p1".into(),
                session_name: "s-1".into()
            })
        );
        assert_eq!(status(&app), "stopping s-1…");
        let only_server = app_with(
            vec![row("w1:p1", "s-1", Some(false), Some(true), Some(true))],
            Some(vec![]),
        );
        let mut only_server = only_server;
        only_server.on_key(Key::Char('x'));
        assert!(
            only_server.view.prompt.is_some(),
            "a running server alone can be stopped"
        );
    }

    #[test]
    fn quitting_works_while_a_job_runs() {
        let mut app = app_with(
            vec![row("w1:p1", "s-1", Some(true), Some(false), Some(true))],
            Some(vec![]),
        );
        app.on_key(Key::Char('v'));
        assert!(app.is_busy());
        assert_eq!(app.on_key(Key::Char('q')), Effect::Quit);
    }

    #[test]
    fn i_toggles_the_profiles_view_and_r_refreshes() {
        let mut app = app_with(two_idle(), Some(vec![]));
        app.view.status = Some(Status::new(StatusKind::Info, "x"));
        app.on_key(Key::Char('i'));
        assert_eq!(app.view.mode, Mode::Profiles);
        assert!(app.view.status.is_none());
        app.on_key(Key::Char('i'));
        assert_eq!(app.view.mode, Mode::Sandboxes);
        assert_eq!(app.on_key(Key::Char('r')), Effect::Start(Job::Refresh));
        assert_eq!(status(&app), "refreshing…");
    }

    #[test]
    fn a_snapshot_keeps_the_selection_on_the_same_pane() {
        let mut app = app_with(two_idle(), Some(vec![]));
        app.on_key(Key::Down);
        assert_eq!(app.view.selected_row().unwrap().pane_id, "w1:p2");
        let mut shifted = two_idle();
        shifted.insert(0, row("w1:p0", "s-0", Some(false), Some(false), Some(true)));
        app.on_snapshot(Ok(Collected {
            rows: shifted,
            ..Default::default()
        }));
        assert_eq!(
            app.view.selected_row().unwrap().pane_id,
            "w1:p2",
            "the pane moved down one row, the selection followed"
        );
        app.on_snapshot(Ok(Collected {
            rows: vec![row("w9:p9", "s-9", None, None, None)],
            ..Default::default()
        }));
        assert_eq!(app.view.selected, 0, "a vanished pane clamps the index");
        app.on_snapshot(Err("boom".into()));
        assert_eq!(status(&app), "could not read the sandboxes: boom");
        assert_eq!(app.view.rows.len(), 1, "an error keeps the last data");
    }

    #[test]
    fn enter_jumps_to_the_selected_pane_unless_it_is_gone() {
        let mut app = app_with(two_idle(), Some(vec!["w1:p1"]));
        assert_eq!(
            app.on_key(Key::Enter),
            Effect::Start(Job::Jump {
                pane_id: "w1:p1".into()
            })
        );
        assert_eq!(status(&app), "jumping to w1:p1…");
        app.on_job_done(Status::new(StatusKind::Error, "x"));
        app.on_key(Key::Down);
        assert_eq!(app.on_key(Key::Char('g')), Effect::None);
        assert_eq!(
            status(&app),
            "pane w1:p2 of s-2 is gone; nothing to jump to"
        );
        assert_eq!(app_with(Vec::new(), None).on_key(Key::Enter), Effect::None);
    }

    #[test]
    fn a_asks_then_stops_the_running_agents_and_forgets_everything() {
        let mut app = app_with(
            vec![
                row("w1:p1", "s-1", Some(true), Some(false), Some(true)),
                row("w1:p2", "s-2", Some(false), Some(false), Some(false)),
            ],
            Some(vec!["w1:p1"]),
        );
        assert_eq!(app.on_key(Key::Char('a')), Effect::None);
        assert_eq!(
            app.view.prompt.as_deref(),
            Some("Stop 1 running agent and forget all 2 mappings? [y/N]")
        );
        assert_eq!(
            app.on_key(Key::Char('y')),
            Effect::Start(Job::CleanAll {
                running: vec![("w1:p1".into(), "s-1".into())]
            })
        );
        assert_eq!(status(&app), "cleaning up…");
        assert_eq!(
            clean_status(1, 2, &[]).text,
            "stopped 1 agent, forgot 2 mappings"
        );
        assert_eq!(
            clean_status(0, 1, &["s-1: boom".into()]).kind,
            StatusKind::Error
        );
        let mut empty = app_with(Vec::new(), None);
        empty.on_key(Key::Char('a'));
        assert!(empty.view.prompt.is_none());
    }

    #[test]
    fn the_help_screen_opens_with_a_question_mark_and_swallows_other_keys() {
        let mut app = app_with(two_idle(), Some(vec![]));
        app.on_key(Key::Char('?'));
        assert!(app.view.help);
        assert_eq!(app.on_key(Key::Char('r')), Effect::None);
        assert!(app.view.help, "other keys do nothing while help shows");
        app.on_key(Key::Escape);
        assert!(!app.view.help);
        app.on_key(Key::Char('?'));
        assert_eq!(app.on_key(Key::Char('q')), Effect::Quit);
    }
}

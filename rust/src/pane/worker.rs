//! The overlay's worker: one thread that fetches a frame every interval and runs
//! the jobs a keypress starts (verify, stop, prune). Its commands carry a
//! [`CancelToken`], so quitting kills an in-flight `nono` or `herdr` call at once
//! instead of leaving the terminal waiting for it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::app::{prune_status, verify_status, Job};
use super::model::{collect_sandboxes, CollectInput, Collected};
use super::ui::{Status, StatusKind};
use crate::config::PluginConfig;
use crate::context::Env;
use crate::exec::CancelToken;
use crate::herdr::HerdrClient;
use crate::lifecycle::{Lifecycle, LifecycleOptions};
use crate::nono::{NonoClient, ProfileSummary};

/// How long a nono call may take in the overlay, shorter than in an action.
pub const OVERLAY_NONO_TIMEOUT: Duration = Duration::from_millis(15_000);

/// What the worker needs.
pub struct WorkerConfig {
    pub state_dir: PathBuf,
    pub config: PluginConfig,
    pub nono_bin: String,
    pub herdr_bin: String,
    pub plugin_root: String,
    pub env: Env,
    pub interval: Duration,
    pub proc_root: PathBuf,
    /// The client and server profile references the next launch uses.
    pub configured: Option<(Option<String>, Option<String>)>,
}

/// What the worker sends to the UI thread.
#[derive(Debug)]
pub enum Msg {
    Snapshot(Box<Result<Collected, String>>),
    JobDone(Status),
}

pub struct Worker {
    jobs: Option<Sender<Job>>,
    pub results: Receiver<Msg>,
    cancel: CancelToken,
    handle: Option<JoinHandle<()>>,
}

fn run_job(job: &Job, lifecycle: &Lifecycle) -> Status {
    let failed =
        |error: crate::errors::PluginError| Status::new(StatusKind::Error, error.message.clone());
    match job {
        Job::Refresh => Status::new(StatusKind::Ok, "refreshed"),
        Job::Verify {
            pane_id,
            session_name,
        } => match lifecycle.verify(pane_id) {
            Ok(outcome) => verify_status(session_name, &outcome.report),
            Err(error) => failed(error),
        },
        Job::Stop {
            pane_id,
            session_name,
        } => match lifecycle.stop(pane_id, false) {
            Ok(_) => Status::new(StatusKind::Ok, format!("stopped {session_name}")),
            Err(error) => failed(error),
        },
        Job::Prune { pane_ids } => match lifecycle.prune(pane_ids) {
            Ok(outcome) => prune_status(
                outcome.pruned.len(),
                outcome
                    .kept
                    .first()
                    .map(|kept| (outcome.kept.len(), kept.reason.as_str())),
            ),
            Err(error) => failed(error),
        },
    }
}

impl Worker {
    pub fn spawn(config: WorkerConfig) -> Worker {
        let (job_tx, job_rx) = channel::<Job>();
        let (msg_tx, msg_rx) = channel::<Msg>();
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let handle = std::thread::spawn(move || {
            let nono = NonoClient::new(config.nono_bin.clone(), config.env.clone())
                .with_cancel(token.clone())
                .with_timeout(OVERLAY_NONO_TIMEOUT);
            let herdr = HerdrClient::new(config.herdr_bin.clone(), config.env.clone())
                .with_cancel(token.clone());
            // The actions run in this process: the overlay is a Herdr plugin pane with the plugin's environment.
            let mut options = LifecycleOptions::new(
                config.state_dir.clone(),
                config.config.clone(),
                config.plugin_root.clone(),
                NonoClient::new(config.nono_bin.clone(), config.env.clone())
                    .with_cancel(token.clone()),
                config.env.clone(),
            );
            options.log = Box::new(|_| {});
            options.herdr = Some(herdr.clone());
            let lifecycle = Lifecycle::new(options);
            let mut cache: HashMap<String, Option<ProfileSummary>> = HashMap::new();
            let mut collect = || {
                let snapshot = collect_sandboxes(CollectInput {
                    state_dir: &config.state_dir,
                    nono: &nono,
                    herdr: &herdr,
                    proc_root: &config.proc_root,
                    profile_cache: &mut cache,
                    configured: config.configured.clone(),
                })
                .map_err(|error| error.message.clone());
                msg_tx.send(Msg::Snapshot(Box::new(snapshot))).is_ok()
            };
            let mut next_collect = Instant::now();
            loop {
                if token.is_cancelled() {
                    break;
                }
                match job_rx.recv_timeout(next_collect.saturating_duration_since(Instant::now())) {
                    Ok(job) => {
                        let status = run_job(&job, &lifecycle);
                        if token.is_cancelled() || msg_tx.send(Msg::JobDone(status)).is_err() {
                            break;
                        }
                        if !collect() {
                            break;
                        }
                        next_collect = Instant::now() + config.interval;
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if !collect() {
                            break;
                        }
                        next_collect = Instant::now() + config.interval;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        Worker {
            jobs: Some(job_tx),
            results: msg_rx,
            cancel,
            handle: Some(handle),
        }
    }

    /// Queues a job for the worker thread.
    pub fn submit(&self, job: Job) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(job);
        }
    }

    /// Cancels whatever is running and waits up to `wait` for the thread to
    /// finish; a thread still busy after that is left to die with the process.
    pub fn shutdown(&mut self, wait: Duration) {
        self.cancel.cancel();
        self.jobs = None;
        let deadline = Instant::now() + wait;
        if let Some(handle) = self.handle.take() {
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::save_pane_entry;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    /// A fake nono: `ps --json` prints sessions.json, or sleeps for 30s (recording its pid) while the file `slow` exists;
    /// `stop <id>` records the id.
    fn fake_nono(dir: &Path) -> PathBuf {
        let script = dir.join("nono");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
case "$1" in
  ps) if [ -e '{dir}/slow' ]; then echo $$ > '{dir}/slow-pid'; sleep 30; fi; cat '{dir}/sessions.json' ;;
  stop) echo "$2" >> '{dir}/stopped' ;;
  profile) echo '{{"name":"p","network":{{"block":true}}}}' ;;
  *) exit 2 ;;
esac
"#,
                dir = dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let herdr = dir.join("herdr");
        std::fs::write(
            &herdr,
            "#!/bin/sh\necho '{\"result\":{\"panes\":[{\"pane_id\":\"w1:p1\"}]}}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&herdr, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("sessions.json"), "[]").unwrap();
        script
    }

    struct Fixture {
        dir: tempfile::TempDir,
        worker: Worker,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let nono = fake_nono(dir.path());
        let state_dir = dir.path().join("state");
        let entry = json!({"sessionName": "herdr-opencode-abc123def456", "agentKind": "opencode", "localPath": "/w", "workdir": "/w", "lifecycleState": "exited", "launchCount": 1});
        save_pane_entry(&state_dir, "w1:p1", entry.as_object().unwrap()).unwrap();
        save_pane_entry(&state_dir, "w1:p2", entry.as_object().unwrap()).unwrap();
        let env = Env::from([("PATH".to_string(), std::env::var("PATH").unwrap())]);
        let worker = Worker::spawn(WorkerConfig {
            state_dir,
            config: PluginConfig::default(),
            nono_bin: nono.to_string_lossy().into_owned(),
            herdr_bin: dir.path().join("herdr").to_string_lossy().into_owned(),
            plugin_root: "/plugin".into(),
            env,
            interval: Duration::from_secs(60),
            proc_root: dir.path().join("no-proc"),
            configured: None,
        });
        Fixture { dir, worker }
    }

    fn next(worker: &Worker) -> Msg {
        worker
            .results
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker answers")
    }

    fn snapshot(message: Msg) -> Collected {
        match message {
            Msg::Snapshot(snapshot) => (*snapshot).expect("a snapshot"),
            other => panic!("expected a snapshot, got {other:?}"),
        }
    }

    #[test]
    fn the_first_frame_arrives_without_asking() {
        let f = fixture();
        let data = snapshot(next(&f.worker));
        assert_eq!(data.rows.len(), 2);
        assert_eq!(data.pane_ids, Some(vec!["w1:p1".to_string()]));
    }

    #[test]
    fn a_prune_job_reports_its_result_and_is_followed_by_a_fresh_frame() {
        let f = fixture();
        snapshot(next(&f.worker));
        f.worker.submit(Job::Prune {
            pane_ids: vec!["w1:p1".into()],
        });
        match next(&f.worker) {
            Msg::JobDone(status) => assert_eq!(
                (status.kind, status.text.as_str()),
                (StatusKind::Ok, "pruned 1 mapping")
            ),
            other => panic!("expected the job result, got {other:?}"),
        }
        let data = snapshot(next(&f.worker));
        assert_eq!(
            data.rows
                .iter()
                .map(|row| row.pane_id.as_str())
                .collect::<Vec<_>>(),
            ["w1:p1"],
            "the gone pane's mapping is dropped"
        );
    }

    #[test]
    fn a_job_that_fails_puts_the_reason_on_the_status_line() {
        let f = fixture();
        snapshot(next(&f.worker));
        f.worker.submit(Job::Verify {
            pane_id: "w1:p1".into(),
            session_name: "herdr-opencode-abc123def456".into(),
        });
        match next(&f.worker) {
            Msg::JobDone(status) => {
                assert_eq!(status.kind, StatusKind::Error);
                assert!(
                    status.text.contains("No nono session is running"),
                    "{}",
                    status.text
                );
            }
            other => panic!("expected the job result, got {other:?}"),
        }
        // The frame that follows every job still arrives.
        assert!(matches!(next(&f.worker), Msg::Snapshot(snapshot) if snapshot.is_ok()));
    }

    #[test]
    fn stopping_a_session_goes_through_nono() {
        let f = fixture();
        std::fs::write(
            f.dir.path().join("sessions.json"),
            json!([{"session_id": "s1", "name": "herdr-opencode-abc123def456", "supervisor_pid": 1, "status": "running"}]).to_string(),
        )
        .unwrap();
        snapshot(next(&f.worker));
        f.worker.submit(Job::Stop {
            pane_id: "w1:p1".into(),
            session_name: "herdr-opencode-abc123def456".into(),
        });
        match next(&f.worker) {
            Msg::JobDone(status) => assert_eq!(
                (status.kind, status.text.as_str()),
                (StatusKind::Ok, "stopped herdr-opencode-abc123def456")
            ),
            other => panic!("expected the job result, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(f.dir.path().join("stopped"))
                .unwrap()
                .trim(),
            "s1"
        );
    }

    #[test]
    fn quitting_during_a_slow_nono_call_kills_it_within_a_fraction_of_a_second() {
        let mut f = fixture();
        snapshot(next(&f.worker));
        std::fs::write(f.dir.path().join("slow"), "").unwrap();
        f.worker.submit(Job::Refresh);
        assert!(matches!(next(&f.worker), Msg::JobDone(_)));
        // The refresh that follows is now stuck in a 30 s `nono ps`.
        let pid_file = f.dir.path().join("slow-pid");
        for _ in 0..200 {
            if pid_file.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .expect("the slow call started")
            .trim()
            .parse()
            .unwrap();
        assert!(crate::procfs::process_alive(pid));
        let started = Instant::now();
        f.worker.shutdown(Duration::from_millis(150));
        let took = started.elapsed();
        assert!(took < Duration::from_millis(250), "shutdown took {took:?}");
        // The shell is reaped by its parent thread; give the kernel a moment, then it must be gone.
        for _ in 0..50 {
            if !crate::procfs::process_alive(pid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !crate::procfs::process_alive(pid),
            "the slow nono call was killed, not left running"
        );
    }

    #[test]
    fn periodic_refresh_keeps_sending_frames() {
        let dir = tempfile::tempdir().unwrap();
        let nono = fake_nono(dir.path());
        let env = Env::from([("PATH".to_string(), std::env::var("PATH").unwrap())]);
        let worker = Worker::spawn(WorkerConfig {
            state_dir: dir.path().join("state"),
            config: PluginConfig::default(),
            nono_bin: nono.to_string_lossy().into_owned(),
            herdr_bin: dir.path().join("herdr").to_string_lossy().into_owned(),
            plugin_root: "/plugin".into(),
            env,
            interval: Duration::from_millis(100),
            proc_root: dir.path().join("no-proc"),
            configured: None,
        });
        for _ in 0..3 {
            snapshot(next(&worker));
        }
    }
}

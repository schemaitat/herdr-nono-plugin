//! The overlay in a real pseudo-terminal: it starts, draws, switches views on a
//! key, and quits on `q` even while a slow `nono` call is in flight, killing that
//! call and handing the terminal back. Runs the built binary against shell fakes
//! of `nono` and `herdr`.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SESSION: &str = "herdr-opencode-abc123def456";

struct Terminal {
    master: OwnedFd,
    output: Arc<Mutex<Vec<u8>>>,
}

impl Terminal {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    fn press(&self, keys: &str) {
        let mut writer = std::fs::File::from(self.master.try_clone().unwrap());
        writer.write_all(keys.as_bytes()).unwrap();
    }

    fn wait_for(&self, needle: &str, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.text().contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "timed out waiting for {what} ({needle:?}); output so far:\n{:?}",
            self.text()
        );
    }
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Setup {
    dir: tempfile::TempDir,
    env: Vec<(String, String)>,
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let state_panes = root.join("state").join("panes");
    std::fs::create_dir_all(&state_panes).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("rust/tests/fixtures/state-v1/panes/pane-1-370bd5d12a.json");
    std::fs::copy(&fixture, state_panes.join("pane-1-370bd5d12a.json")).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("sessions.json"),
        format!(
            r#"[{{"session_id":"s1","name":"{SESSION}","supervisor_pid":4242,"status":"running"}}]"#
        ),
    )
    .unwrap();
    script(
        &root.join("nono"),
        &format!(
            r#"case "$1" in
  ps) if [ -e '{r}/slow' ]; then echo $$ > '{r}/slow-pid'; sleep 30; fi; cat '{r}/sessions.json' ;;
  profile) echo '{{"name":"p","network":{{"block":true}}}}' ;;
  *) exit 2 ;;
esac
"#,
            r = root.display()
        ),
    );
    script(
        &root.join("herdr"),
        "echo '{\"result\":{\"panes\":[{\"pane_id\":\"pane-1\"}]}}'\n",
    );
    let path = std::env::var("PATH").unwrap();
    let env = vec![
        ("PATH".to_string(), path),
        (
            "HOME".to_string(),
            root.join("home").to_string_lossy().into_owned(),
        ),
        ("TERM".to_string(), "xterm-256color".to_string()),
        (
            "HERDR_PLUGIN_STATE_DIR".to_string(),
            root.join("state").to_string_lossy().into_owned(),
        ),
        (
            "HERDR_PLUGIN_CONFIG_DIR".to_string(),
            root.join("config").to_string_lossy().into_owned(),
        ),
        (
            "HERDR_PLUGIN_ROOT".to_string(),
            env!("CARGO_MANIFEST_DIR").to_string(),
        ),
        (
            "HERDR_BIN_PATH".to_string(),
            root.join("herdr").to_string_lossy().into_owned(),
        ),
        (
            "HERDR_NONO_BIN".to_string(),
            root.join("nono").to_string_lossy().into_owned(),
        ),
    ];
    Setup { dir, env }
}

fn spawn_in_pty(setup: &Setup, args: &[&str]) -> (Terminal, Child) {
    let (mut master, mut slave) = (0, 0);
    let size = libc::winsize {
        ws_row: 32,
        ws_col: 110,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty fills the two descriptors we pass; the window size is a valid struct.
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &size,
        )
    };
    assert_eq!(opened, 0, "openpty failed");
    // SAFETY: both descriptors were just returned by openpty and are owned by us.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let mut command = Command::new(env!("CARGO_BIN_EXE_herdr-nono"));
    command
        .arg("pane")
        .args(args)
        .env_clear()
        .envs(setup.env.clone())
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    // SAFETY: setsid and ioctl are async-signal-safe; the slave is on fd 0 here, so it becomes the controlling terminal.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            libc::ioctl(0, libc::TIOCSCTTY, 0);
            Ok(())
        });
    }
    let child = command.spawn().unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&output);
    let mut reader = std::fs::File::from(master.try_clone().unwrap());
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        // EIO on the master means the child side closed.
        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buffer[..read]);
        }
    });
    let _ = master.as_raw_fd();
    (Terminal { master, output }, child)
}

fn wait_exit(child: &mut Child, within: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

fn process_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[test]
fn it_draws_switches_views_and_quits_restoring_the_terminal() {
    let setup = setup();
    let (term, mut child) = spawn_in_pty(&setup, &[]);
    term.wait_for("nono sandboxes", "the first frame");
    term.wait_for("abc123def456", "the mapping row");
    assert!(
        term.text().contains("\u{1b}[?1049h"),
        "the overlay enters the alternate screen"
    );
    term.press("i");
    term.wait_for("Profiles · agent pane-1", "the profiles view");
    term.press("i");
    term.wait_for("select", "the list again");
    term.press("\r");
    term.wait_for("Details", "enter opens the details popup");
    term.press("\u{1b}");
    term.wait_for("refreshes every", "escape closes the popup");
    term.press("q");
    let status = wait_exit(&mut child, Duration::from_secs(5)).expect("q quits");
    assert!(status.success(), "{status:?}");
    let text = term.text();
    assert!(
        text.contains("\u{1b}[?1049l"),
        "the main screen is restored"
    );
    assert!(text.contains("\u{1b}[?25h"), "the cursor is shown again");
}

#[test]
fn ctrl_c_also_quits() {
    let setup = setup();
    let (term, mut child) = spawn_in_pty(&setup, &[]);
    term.wait_for("abc123def456", "the mapping row");
    term.press("\u{3}");
    assert!(wait_exit(&mut child, Duration::from_secs(5))
        .expect("ctrl-c quits")
        .success());
}

#[test]
fn quitting_during_a_slow_verify_exits_at_once_and_kills_the_nono_call() {
    let setup = setup();
    let root: PathBuf = setup.dir.path().to_path_buf();
    let (term, mut child) = spawn_in_pty(&setup, &[]);
    term.wait_for("abc123def456", "the mapping row");
    // From now on every `nono ps` hangs for 30 s.
    std::fs::write(root.join("slow"), "").unwrap();
    term.press("v");
    term.wait_for(&format!("verifying {SESSION}"), "the verify to start");
    let pid_file = root.join("slow-pid");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pid_file.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("the slow nono call started")
        .trim()
        .parse()
        .unwrap();
    assert!(process_alive(pid));
    let started = Instant::now();
    term.press("q");
    let status =
        wait_exit(&mut child, Duration::from_secs(5)).expect("q quits even while verifying");
    let took = started.elapsed();
    println!("quit took {took:?}");
    assert!(status.success(), "{status:?}");
    assert!(took < Duration::from_millis(500), "quitting took {took:?}");
    assert!(
        term.text().contains("\u{1b}[?1049l"),
        "the terminal is restored"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while process_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !process_alive(pid),
        "the slow nono call was killed, not left running"
    );
}

#[test]
fn a_startup_error_stays_on_screen_until_a_key_is_pressed() {
    let mut setup = setup();
    setup
        .env
        .retain(|(name, _)| name != "HERDR_PLUGIN_STATE_DIR");
    let (term, mut child) = spawn_in_pty(&setup, &[]);
    term.wait_for("HERDR_PLUGIN_STATE_DIR is not set", "the error");
    term.wait_for("press any key to close", "the hold notice");
    assert!(
        wait_exit(&mut child, Duration::from_millis(500)).is_none(),
        "it waits for a key"
    );
    term.press("x");
    let status = wait_exit(&mut child, Duration::from_secs(5)).expect("a key closes it");
    assert_eq!(status.code(), Some(1));
}

#[test]
fn once_prints_one_plain_frame_without_a_terminal() {
    let setup = setup();
    let output = Command::new(env!("CARGO_BIN_EXE_herdr-nono"))
        .args(["pane", "--once"])
        .env_clear()
        .envs(setup.env.clone())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("nono sandboxes") && text.contains("1 agent"),
        "{text}"
    );
    assert!(
        text.contains("┏ ▶ repo") && text.contains("…abc123def456"),
        "{text}"
    );
    assert!(!text.contains('\u{1b}'), "--once prints no escape codes");
}

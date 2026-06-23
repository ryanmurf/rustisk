#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
static CLI_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn cli_test_lock() -> &'static Mutex<()> {
    CLI_TEST_LOCK.get_or_init(|| Mutex::new(()))
}

fn lock_cli_tests() -> MutexGuard<'static, ()> {
    cli_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Fixture {
    root: PathBuf,
    config_file: PathBuf,
    run_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "rustisk-cli-startup-{}-{}-{}",
            std::process::id(),
            now,
            id
        ));
        let config_dir = root.join("etc");
        let run_dir = root.join("run");
        let mod_dir = root.join("modules");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&run_dir).unwrap();
        fs::create_dir_all(&mod_dir).unwrap();

        let config_file = config_dir.join("asterisk.conf");
        fs::write(
            &config_file,
            format!(
                "[directories]\nastetcdir => {}\nastrundir => {}\nastmoddir => {}\n",
                config_dir.display(),
                run_dir.display(),
                mod_dir.display()
            ),
        )
        .unwrap();
        fs::write(
            config_dir.join("extensions.conf"),
            "[default]\nexten => s,1,Answer()\nexten => s,n,Hangup()\n",
        )
        .unwrap();
        fs::write(
            config_dir.join("pjsip.conf"),
            "[transport-udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n",
        )
        .unwrap();
        fs::write(
            config_dir.join("manager.conf"),
            "[general]\nenabled = yes\nbindaddr = 127.0.0.1\nport = 0\n\n[admin]\nsecret = admin\n",
        )
        .unwrap();

        Self {
            root,
            config_file,
            run_dir,
        }
    }

    fn socket_path(&self) -> PathBuf {
        self.run_dir.join("asterisk.ctl")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct CapturedOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

impl CapturedOutput {
    fn combined(&self) -> String {
        format!("stdout:\n{}\nstderr:\n{}", self.stdout, self.stderr)
    }
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_rustisk")
}

fn command_for(fixture: &Fixture) -> Command {
    let mut command = Command::new(binary());
    command.arg("-C").arg(&fixture.config_file);
    command.env_remove("OTEL_EXPORTER_OTLP_ENDPOINT");
    command
}

fn run_asterisk(
    fixture: &Fixture,
    args: &[&str],
    stdin_data: Option<&str>,
    timeout: Duration,
) -> CapturedOutput {
    let mut command = command_for(fixture);
    command.args(args);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    if stdin_data.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }

    let mut child = command.spawn().unwrap();
    if let Some(input) = stdin_data {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.as_bytes()).unwrap();
    }

    wait_for_exit(child, timeout)
}

fn spawn_daemon(fixture: &Fixture, args: &[&str]) -> Child {
    let mut command = command_for(fixture);
    command.args(args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.spawn().unwrap()
}

fn wait_for_exit(mut child: Child, timeout: Duration) -> CapturedOutput {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return collect_output(child, status, false);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child.wait().unwrap();
            return collect_output(child, status, true);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn collect_output(mut child: Child, status: ExitStatus, timed_out: bool) -> CapturedOutput {
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let _ = child.wait();
    CapturedOutput {
        status,
        stdout,
        stderr,
        timed_out,
    }
}

fn wait_for_control_socket(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("control socket was not ready at {}", path.display());
}

fn terminate(mut child: Child) -> CapturedOutput {
    if let Some(status) = child.try_wait().unwrap() {
        return collect_output(child, status, false);
    }

    let status = Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()
        .unwrap();
    assert!(status.success(), "failed to send SIGTERM to child");

    wait_for_exit(child, Duration::from_secs(10))
}

#[test]
fn console_mode_with_foreground_and_dump_core_reaches_prompt() {
    let _guard = lock_cli_tests();
    let fixture = Fixture::new();

    let output = run_asterisk(&fixture, &["-f", "-c", "-g"], None, Duration::from_secs(20));

    assert!(!output.timed_out, "{}", output.combined());
    assert!(output.status.success(), "{}", output.combined());
    assert!(
        output.stdout.contains("Rustisk console ready"),
        "{}",
        output.combined()
    );
    assert!(
        !output
            .combined()
            .contains("Cannot block the current thread"),
        "{}",
        output.combined()
    );
}

#[test]
fn foreground_mode_without_console_waits_for_shutdown_signal() {
    let _guard = lock_cli_tests();
    let fixture = Fixture::new();
    let mut child = spawn_daemon(&fixture, &["-f", "-g"]);

    wait_for_control_socket(&fixture.socket_path(), Duration::from_secs(10));
    assert!(
        child.try_wait().unwrap().is_none(),
        "daemon exited before SIGTERM"
    );

    let output = terminate(child);
    assert!(!output.timed_out, "{}", output.combined());
    assert!(output.status.success(), "{}", output.combined());
    assert!(
        !output.stdout.contains("Rustisk console ready"),
        "{}",
        output.combined()
    );
}

#[test]
fn always_fork_flag_does_not_enter_console_mode() {
    let _guard = lock_cli_tests();
    let fixture = Fixture::new();
    let mut child = spawn_daemon(&fixture, &["-F"]);

    wait_for_control_socket(&fixture.socket_path(), Duration::from_secs(10));
    assert!(
        child.try_wait().unwrap().is_none(),
        "-F unexpectedly exited"
    );

    let output = terminate(child);
    assert!(!output.timed_out, "{}", output.combined());
    assert!(output.status.success(), "{}", output.combined());
    assert!(
        !output.stdout.contains("Rustisk console ready"),
        "{}",
        output.combined()
    );
}

#[test]
fn remote_execution_accepts_upstream_x_forms_and_socket_override() {
    let _guard = lock_cli_tests();
    let fixture = Fixture::new();
    let child = spawn_daemon(&fixture, &["-f"]);
    wait_for_control_socket(&fixture.socket_path(), Duration::from_secs(10));

    let remote_rx = run_asterisk(
        &fixture,
        &["-rx", "core show version"],
        None,
        Duration::from_secs(10),
    );
    assert!(!remote_rx.timed_out, "{}", remote_rx.combined());
    assert!(remote_rx.status.success(), "{}", remote_rx.combined());
    assert!(
        remote_rx.stdout.contains("Rustisk 0.1.0"),
        "{}",
        remote_rx.combined()
    );

    let bare_x = run_asterisk(
        &fixture,
        &["-x", "core show version"],
        None,
        Duration::from_secs(10),
    );
    assert!(!bare_x.timed_out, "{}", bare_x.combined());
    assert!(bare_x.status.success(), "{}", bare_x.combined());
    assert!(
        bare_x.stdout.contains("Rustisk 0.1.0"),
        "{}",
        bare_x.combined()
    );

    let socket_path = fixture.socket_path();
    let socket_override = run_asterisk(
        &fixture,
        &[
            "-s",
            socket_path.to_str().unwrap(),
            "-x",
            "core show version",
        ],
        None,
        Duration::from_secs(10),
    );
    assert!(!socket_override.timed_out, "{}", socket_override.combined());
    assert!(
        socket_override.status.success(),
        "{}",
        socket_override.combined()
    );
    assert!(
        socket_override.stdout.contains("Rustisk 0.1.0"),
        "{}",
        socket_override.combined()
    );

    let remote_console = run_asterisk(
        &fixture,
        &["-r"],
        Some("core show version\nquit\n"),
        Duration::from_secs(10),
    );
    assert!(!remote_console.timed_out, "{}", remote_console.combined());
    assert!(
        remote_console.status.success(),
        "{}",
        remote_console.combined()
    );
    assert!(
        remote_console.stdout.contains("Rustisk 0.1.0"),
        "{}",
        remote_console.combined()
    );

    let output = terminate(child);
    assert!(!output.timed_out, "{}", output.combined());
    assert!(output.status.success(), "{}", output.combined());
}

#[test]
fn bare_x_without_running_daemon_fails_as_remote_execution() {
    let _guard = lock_cli_tests();
    let fixture = Fixture::new();

    let output = run_asterisk(
        &fixture,
        &["-x", "core show version"],
        None,
        Duration::from_secs(8),
    );

    assert!(!output.timed_out, "{}", output.combined());
    assert!(!output.status.success(), "{}", output.combined());
    assert!(
        output
            .stderr
            .contains("Unable to connect to remote asterisk"),
        "{}",
        output.combined()
    );
    assert!(
        !output.stdout.contains("Rustisk console ready"),
        "{}",
        output.combined()
    );
}

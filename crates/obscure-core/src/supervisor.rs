//! Runs the Xray process: validates the configuration with `-test`, spawns
//! `xray run`, streams its output and detects exit.

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use crate::error::{Error, Result};

/// Events emitted by a running core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreEvent {
    /// A line from stdout/stderr. Exit is detected by polling
    /// [`RunningCore::check_exit`].
    Log(String),
}

/// Handle to a running Xray process. Dropping it kills the process.
pub struct RunningCore {
    child: Child,
    pid_file: Option<PathBuf>,
    pub local_port: u16,
    pub events: mpsc::UnboundedReceiver<CoreEvent>,
}

impl std::fmt::Debug for RunningCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningCore")
            .field("pid", &self.child.id())
            .field("local_port", &self.local_port)
            .finish()
    }
}

impl RunningCore {
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Stops the process. Sends SIGTERM first and falls back to SIGKILL
    /// after a short grace period.
    pub async fn stop(mut self) {
        if let Some(pid) = self.child.id() {
            // SAFETY: plain libc call with a pid we own.
            unsafe {
                libc_kill(pid as i32, 15);
            }
            match tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await {
                Ok(_) => {
                    self.remove_pid_file();
                    return;
                }
                Err(_) => tracing::warn!("xray did not exit after SIGTERM, killing"),
            }
        }
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        self.remove_pid_file();
    }

    fn remove_pid_file(&self) {
        if let Some(f) = &self.pid_file {
            let _ = std::fs::remove_file(f);
        }
    }
}

unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

/// Spawns and supervises Xray.
#[derive(Debug, Clone)]
pub struct Supervisor {
    xray: PathBuf,
    asset_dir: PathBuf,
    pid_file: Option<PathBuf>,
}

impl Supervisor {
    pub fn new(xray: PathBuf, asset_dir: PathBuf) -> Self {
        Self {
            xray,
            asset_dir,
            pid_file: None,
        }
    }

    /// Records the running core's pid in `pid_file`, so a core left behind
    /// by a crashed or killed app (`kill -9` skips `kill_on_drop`) is
    /// stopped before the next start instead of holding the port.
    pub fn with_pid_file(mut self, pid_file: PathBuf) -> Self {
        self.pid_file = Some(pid_file);
        self
    }

    /// Stops the core recorded in the pid file, if it is still running and
    /// really is our Xray (checked through its command line, so a reused
    /// pid is never touched).
    pub async fn stop_orphan(&self) {
        let Some(pid_file) = &self.pid_file else {
            return;
        };
        let Some((pid, owner)) = std::fs::read_to_string(pid_file)
            .ok()
            .and_then(|s| parse_pid_file(&s))
        else {
            return;
        };
        // Another live instance (e.g. the installed app next to a
        // development build, sharing the data dir) still owns it.
        if let Some(owner) = owner
            && owner != std::process::id() as i32
            && process_alive(owner)
        {
            return;
        }
        let _ = std::fs::remove_file(pid_file);
        if !is_our_core(pid, &self.xray) {
            return;
        }
        tracing::warn!("stopping a leftover Xray (pid {pid}) from a previous session");
        // SAFETY: plain libc calls; the pid was verified above.
        unsafe {
            libc_kill(pid, 15);
        }
        for _ in 0..30 {
            if !is_our_core(pid, &self.xray) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        unsafe {
            libc_kill(pid, 9);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.xray);
        cmd.env("XRAY_LOCATION_ASSET", &self.asset_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    /// Runs `xray run -test -c <config>` and returns Xray's output on failure.
    pub async fn test_config(&self, config: &Path) -> Result<()> {
        if !self.xray.is_file() {
            return Err(Error::CoreNotInstalled);
        }
        let out = self
            .command()
            .args(["run", "-test", "-c"])
            .arg(config)
            .output()
            .await
            .map_err(|e| Error::io(&self.xray, e))?;
        if out.status.success() {
            Ok(())
        } else {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            Err(Error::ConfigRejected(text.trim().to_owned()))
        }
    }

    /// Validates, spawns and waits until the local port accepts connections.
    pub async fn start(&self, config: &Path, local_port: u16) -> Result<RunningCore> {
        self.test_config(config).await?;
        self.stop_orphan().await;

        let mut child = self
            .command()
            .args(["run", "-c"])
            .arg(config)
            .spawn()
            .map_err(|e| Error::io(&self.xray, e))?;

        let (tx, rx) = mpsc::unbounded_channel();
        if let Some(stdout) = child.stdout.take() {
            spawn_line_reader(BufReader::new(stdout), tx.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_line_reader(BufReader::new(stderr), tx);
        }

        if let (Some(f), Some(pid)) = (&self.pid_file, child.id())
            && let Err(e) =
                crate::paths::write_private(f, format!("{pid} {}", std::process::id()).as_bytes())
        {
            tracing::warn!("cannot record the core pid: {e}");
        }
        let mut core = RunningCore {
            child,
            pid_file: self.pid_file.clone(),
            local_port,
            events: rx,
        };

        // Wait for readiness or early death.
        let addr = format!("127.0.0.1:{local_port}");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        let mut early_output = Vec::new();
        loop {
            if let Some(status) = core
                .child
                .try_wait()
                .map_err(|e| Error::io(&self.xray, e))?
            {
                while let Ok(CoreEvent::Log(l)) = core.events.try_recv() {
                    early_output.push(l);
                }
                return Err(Error::CoreExited {
                    status: describe_status(status),
                    output: early_output.join("\n"),
                });
            }
            if tokio::net::TcpStream::connect(&addr).await.is_ok() {
                // Something answered; make sure it is our child and not
                // another process holding the port (ours would then die
                // with "address already in use").
                tokio::time::sleep(Duration::from_millis(150)).await;
                if core
                    .child
                    .try_wait()
                    .map_err(|e| Error::io(&self.xray, e))?
                    .is_none()
                {
                    break;
                }
                continue;
            }
            if tokio::time::Instant::now() >= deadline {
                core.stop().await;
                return Err(Error::StartTimeout(addr));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // The caller owns the child, so exit is detected by polling
        // `RunningCore::check_exit`.
        Ok(core)
    }
}

fn spawn_line_reader<R>(reader: BufReader<R>, tx: mpsc::UnboundedSender<CoreEvent>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(CoreEvent::Log(line)).is_err() {
                break;
            }
        }
    });
}

impl RunningCore {
    /// Returns `Some(status)` if the process has exited.
    pub fn check_exit(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(describe_status(status)),
            Ok(None) => None,
            Err(e) => Some(format!("wait error: {e}")),
        }
    }
}

/// Pid file contents: `<core pid> <app pid>` (older files: just the core).
fn parse_pid_file(text: &str) -> Option<(i32, Option<i32>)> {
    let mut it = text.split_whitespace();
    let pid = it.next()?.parse::<i32>().ok().filter(|p| *p > 1)?;
    let owner = it.next().and_then(|o| o.parse::<i32>().ok());
    Some((pid, owner))
}

fn process_alive(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
        s.rsplit(')')
            .next()
            .and_then(|r| r.split_whitespace().next())
            != Some("Z")
    })
}

/// `true` if `pid` is alive and was started from the `xray` binary.
fn is_our_core(pid: i32, xray: &Path) -> bool {
    let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let argv0 = cmdline.split(|b| *b == 0).next().unwrap_or_default();
    // A zombie has an empty command line.
    !argv0.is_empty() && Path::new(std::ffi::OsStr::from_bytes(argv0)) == xray
}

fn describe_status(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        format!("exit code {code}")
    } else if let Some(sig) = status.signal() {
        format!("signal {sig}")
    } else {
        "unknown".into()
    }
}

/// Writes `config` to `path` (0600) atomically.
pub fn write_config(path: &Path, config: &serde_json::Value) -> Result<()> {
    crate::paths::write_private(path, &serde_json::to_vec_pretty(config)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes an executable script and waits until it can actually be
    /// executed: a concurrent test may fork while our write fd is still
    /// open, which makes exec fail with ETXTBSY for a moment.
    fn make_script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..50 {
            match std::process::Command::new(path).arg("--probe").output() {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => return,
            }
        }
    }

    #[tokio::test]
    async fn missing_binary_is_reported() {
        let s = Supervisor::new(PathBuf::from("/nonexistent/xray"), PathBuf::from("/tmp"));
        let err = s.test_config(Path::new("/dev/null")).await.unwrap_err();
        assert!(matches!(err, Error::CoreNotInstalled));
    }

    #[tokio::test]
    async fn fake_core_rejecting_config() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("xray");
        make_script(&fake, "#!/bin/sh\necho 'bad config' >&2\nexit 23\n");
        let s = Supervisor::new(fake, dir.path().to_path_buf());
        let err = s.test_config(Path::new("/dev/null")).await.unwrap_err();
        assert!(matches!(err, Error::ConfigRejected(ref t) if t.contains("bad config")));
    }

    #[tokio::test]
    async fn fake_core_that_listens_and_stops() {
        // A fake "xray" that passes -test and then listens on the port with
        // python, so we exercise readiness detection and SIGTERM handling.
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("xray");
        make_script(
            &fake,
            r#"#!/bin/sh
if [ "$1" = "--probe" ]; then exit 0; fi
if [ "$2" = "-test" ]; then exit 0; fi
echo "fake xray started"
exec python3 -c 'import socket,time,signal
signal.signal(signal.SIGTERM, lambda *a: exit(0))
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR,1); s.bind(("127.0.0.1", 28765)); s.listen(1)
while True: time.sleep(1)'
"#,
        );
        let s = Supervisor::new(fake, dir.path().to_path_buf());
        let cfg = dir.path().join("c.json");
        write_config(&cfg, &serde_json::json!({})).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&cfg).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut core = s.start(&cfg, 28765).await.unwrap();
        assert!(core.pid().is_some());
        assert!(core.check_exit().is_none());
        let first = tokio::time::timeout(Duration::from_secs(2), core.events.recv())
            .await
            .unwrap();
        assert_eq!(first, Some(CoreEvent::Log("fake xray started".into())));
        core.stop().await;
    }

    /// Fake core that listens on the port written in its config (argv0 is
    /// kept as the script path, like a real `xray`).
    const LISTENING_CORE: &str = r#"#!/bin/bash
if [ "$1" = "--probe" ]; then exit 0; fi
if [ "$2" = "-test" ]; then exit 0; fi
exec -a "$0" python3 -c 'import socket,time,signal,sys
signal.signal(signal.SIGTERM, lambda *a: sys.exit(0))
port=int(open(sys.argv[1]).read())
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR,1)
try:
    s.bind(("127.0.0.1", port))
except OSError as e:
    print("listen failed:", e); sys.exit(1)
s.listen(1)
while True: time.sleep(1)' "$3"
"#;

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn alive(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|s| !s.split(' ').nth(2).is_some_and(|st| st == "Z"))
    }

    #[tokio::test]
    async fn leftover_core_is_stopped_before_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("xray");
        make_script(&fake, LISTENING_CORE);
        let port = free_port();
        let cfg = dir.path().join("c.json");
        std::fs::write(&cfg, port.to_string()).unwrap();
        let pid_file = dir.path().join("xray.pid");
        let s =
            Supervisor::new(fake.clone(), dir.path().to_path_buf()).with_pid_file(pid_file.clone());

        // Simulate an app killed with SIGKILL: the core outlives its handle.
        let first = s.start(&cfg, port).await.unwrap();
        let old_pid = first.pid().unwrap();
        assert_eq!(
            std::fs::read_to_string(&pid_file).unwrap(),
            format!("{old_pid} {}", std::process::id())
        );
        std::mem::forget(first);
        // Pretend the owning app is gone (its pid is now a dead one).
        std::fs::write(&pid_file, format!("{old_pid} {}", dead_pid())).unwrap();

        let second = s.start(&cfg, port).await.expect("starts after the orphan");
        let new_pid = second.pid().unwrap();
        assert_ne!(new_pid, old_pid);
        assert!(!alive(old_pid), "orphan stopped");
        assert!(second_is_listener(port));
        second.stop().await;
        assert!(!pid_file.exists(), "pid file removed on stop");
    }

    fn dead_pid() -> u32 {
        let mut c = std::process::Command::new("true").spawn().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        pid
    }

    #[tokio::test]
    async fn core_of_another_live_instance_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("xray");
        make_script(&fake, LISTENING_CORE);
        let port = free_port();
        let cfg = dir.path().join("c.json");
        std::fs::write(&cfg, port.to_string()).unwrap();
        let pid_file = dir.path().join("xray.pid");
        let s = Supervisor::new(fake, dir.path().to_path_buf()).with_pid_file(pid_file.clone());
        let core = s.start(&cfg, port).await.unwrap();
        let pid = core.pid().unwrap();
        // Owned by another live process (pid 1 stands in for it).
        std::fs::write(&pid_file, format!("{pid} 1")).unwrap();
        s.stop_orphan().await;
        assert!(alive(pid), "a live instance's core is not touched");
        core.stop().await;
    }

    #[test]
    fn pid_file_formats() {
        assert_eq!(parse_pid_file("123 456\n"), Some((123, Some(456))));
        assert_eq!(parse_pid_file("123"), Some((123, None)));
        assert_eq!(parse_pid_file("1"), None);
        assert_eq!(parse_pid_file("x"), None);
    }

    fn second_is_listener(port: u16) -> bool {
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    }

    #[tokio::test]
    async fn port_held_by_someone_else_is_not_reported_ready() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("xray");
        make_script(&fake, LISTENING_CORE);
        let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = squatter.local_addr().unwrap().port();
        let cfg = dir.path().join("c.json");
        std::fs::write(&cfg, port.to_string()).unwrap();
        let s = Supervisor::new(fake, dir.path().to_path_buf());
        let err = s.start(&cfg, port).await.unwrap_err();
        assert!(
            matches!(err, Error::CoreExited { ref output, .. } if output.contains("listen failed")),
            "{err:?}"
        );
    }

    #[test]
    fn pid_check_ignores_other_programs() {
        let me = std::process::id() as i32;
        assert!(!is_our_core(me, Path::new("/nonexistent/xray")));
        assert!(!is_our_core(i32::MAX, Path::new("/x")));
    }
}

/// Restart policy after an unexpected exit: exponential backoff with a cap
/// on the number of attempts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartPolicy {
    pub max_restarts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    attempts: u32,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            max_restarts: 3,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(8),
            attempts: 0,
        }
    }
}

impl RestartPolicy {
    /// Returns how long to wait before the next restart, or `None` when the
    /// budget is exhausted.
    pub fn next_delay(&mut self) -> Option<Duration> {
        if self.attempts >= self.max_restarts {
            return None;
        }
        let delay = self
            .base_delay
            .checked_mul(2u32.saturating_pow(self.attempts))
            .unwrap_or(self.max_delay)
            .min(self.max_delay);
        self.attempts += 1;
        Some(delay)
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Call after the core has been up long enough to be considered stable.
    pub fn reset(&mut self) {
        self.attempts = 0;
    }
}

#[cfg(test)]
mod backoff_tests {
    use super::*;

    #[test]
    fn exponential_with_cap() {
        let mut p = RestartPolicy::default();
        assert_eq!(p.next_delay(), Some(Duration::from_secs(1)));
        assert_eq!(p.next_delay(), Some(Duration::from_secs(2)));
        assert_eq!(p.next_delay(), Some(Duration::from_secs(4)));
        assert_eq!(p.next_delay(), None);
        assert_eq!(p.attempts(), 3);
        p.reset();
        assert_eq!(p.next_delay(), Some(Duration::from_secs(1)));
        let mut big = RestartPolicy {
            max_restarts: 10,
            base_delay: Duration::from_secs(5),
            max_delay: Duration::from_secs(8),
            attempts: 0,
        };
        assert_eq!(big.next_delay(), Some(Duration::from_secs(5)));
        assert_eq!(big.next_delay(), Some(Duration::from_secs(8)));
    }
}

//! Runs the Xray process: validates the configuration with `-test`, spawns
//! `xray run`, streams its output and detects exit.

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
    /// A line from stdout/stderr.
    Log(String),
    /// The process ended. `status` is a human string such as `exit code 1`.
    Exited { status: String },
}

/// Handle to a running Xray process. Dropping it kills the process.
pub struct RunningCore {
    child: Child,
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
                Ok(_) => return,
                Err(_) => tracing::warn!("xray did not exit after SIGTERM, killing"),
            }
        }
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
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
}

impl Supervisor {
    pub fn new(xray: PathBuf, asset_dir: PathBuf) -> Self {
        Self { xray, asset_dir }
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

        let mut core = RunningCore {
            child,
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
                while let Ok(ev) = core.events.try_recv() {
                    if let CoreEvent::Log(l) = ev {
                        early_output.push(l);
                    }
                }
                return Err(Error::CoreExited {
                    status: describe_status(status),
                    output: early_output.join("\n"),
                });
            }
            if tokio::net::TcpStream::connect(&addr).await.is_ok() {
                break;
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
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| Error::io(&tmp, e))?;
    serde_json::to_writer_pretty(&mut f, config)?;
    std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        std::fs::write(&fake, "#!/bin/sh\necho 'bad config' >&2\nexit 23\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        std::fs::write(
            &fake,
            r#"#!/bin/sh
if [ "$2" = "-test" ]; then exit 0; fi
echo "fake xray started"
exec python3 -c 'import socket,time,signal
signal.signal(signal.SIGTERM, lambda *a: exit(0))
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR,1); s.bind(("127.0.0.1", 28765)); s.listen(1)
while True: time.sleep(1)'
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let s = Supervisor::new(fake, dir.path().to_path_buf());
        let cfg = dir.path().join("c.json");
        write_config(&cfg, &serde_json::json!({})).unwrap();
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

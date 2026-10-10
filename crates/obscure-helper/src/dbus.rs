//! Prototype system D-Bus service. Every call is authorised through polkit
//! (`io.github.talesam.Obscure.grant-tun`) using the caller's bus name, so
//! an unprivileged client (including a Flatpak) can ask for the grant and
//! the desktop shows the usual authentication dialog.

use zbus::{interface, message::Header, zvariant::Value};

pub const BUS_NAME: &str = "io.github.talesam.Obscure.Helper";
pub const OBJECT_PATH: &str = "/io/github/talesam/Obscure/Helper";
const ACTION_ID: &str = "io.github.talesam.Obscure.grant-tun";

/// The service is D-Bus activated, so it leaves after this long without a
/// call instead of staying resident as root.
const IDLE_EXIT: std::time::Duration = std::time::Duration::from_secs(60);

struct Helper {
    activity: std::sync::Arc<tokio::sync::Notify>,
}

#[interface(name = "io.github.talesam.Obscure.Helper")]
impl Helper {
    /// Grants TUN capabilities to the given Xray binary. Returns the new
    /// `getcap` line on success.
    async fn grant_tun(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        path: String,
    ) -> zbus::fdo::Result<String> {
        let sender = header
            .sender()
            .ok_or_else(|| zbus::fdo::Error::Failed("no sender".into()))?
            .to_owned();
        self.activity.notify_one();
        let failed = |e: String| zbus::fdo::Error::Failed(e);
        // Only the caller's own Xray binary may receive capabilities.
        let uid = zbus::fdo::DBusProxy::new(connection)
            .await?
            .get_connection_unix_user(sender.clone().into())
            .await?;
        check_authorization(connection, sender.as_str(), &path).await?;
        tokio::task::spawn_blocking(move || {
            let p = std::path::Path::new(&path);
            crate::grant_tun(p, uid)?;
            crate::describe_caps(p)
        })
        .await
        .map_err(|e| failed(e.to_string()))?
        .map_err(failed)
    }

    /// Protocol version, for clients to detect the helper.
    #[zbus(property)]
    fn version(&self) -> u32 {
        1
    }
}

/// Asks polkit whether `sender` may perform the action, allowing
/// interactive authentication (the agent shows the password dialog).
async fn check_authorization(
    connection: &zbus::Connection,
    sender: &str,
    path: &str,
) -> zbus::fdo::Result<()> {
    let proxy = zbus::Proxy::new(
        connection,
        "org.freedesktop.PolicyKit1",
        "/org/freedesktop/PolicyKit1/Authority",
        "org.freedesktop.PolicyKit1.Authority",
    )
    .await
    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

    let mut details = std::collections::HashMap::new();
    details.insert("name", Value::from(sender));
    let subject = ("system-bus-name", details);
    // Same detail pkexec provides, so the dialog names the target file.
    let command_line = format!("obscure-helper grant-tun {path}");
    let action_details: std::collections::HashMap<&str, &str> =
        [("command_line", command_line.as_str())].into();
    let flags: u32 = 1; // AllowUserInteraction
    let cancellation_id = "";

    let reply: ((bool, bool, std::collections::HashMap<String, String>),) = proxy
        .call(
            "CheckAuthorization",
            &(subject, ACTION_ID, action_details, flags, cancellation_id),
        )
        .await
        .map_err(|e| zbus::fdo::Error::Failed(format!("polkit: {e}")))?;
    let (authorized, _challenge, _) = reply.0;
    if authorized {
        Ok(())
    } else {
        Err(zbus::fdo::Error::AccessDenied(
            "not authorised by polkit".into(),
        ))
    }
}

/// Runs the service on the system bus until SIGTERM/SIGINT.
pub fn serve() -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let activity = std::sync::Arc::new(tokio::sync::Notify::new());
    rt.block_on(async {
        let _conn = zbus::connection::Builder::system()
            .map_err(|e| e.to_string())?
            .name(BUS_NAME)
            .map_err(|e| e.to_string())?
            .serve_at(
                OBJECT_PATH,
                Helper {
                    activity: activity.clone(),
                },
            )
            .map_err(|e| e.to_string())?
            .build()
            .await
            .map_err(|e| format!("cannot register {BUS_NAME} on the system bus: {e}"))?;
        eprintln!("obscure-helper: serving {BUS_NAME}");
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|e| e.to_string())?;
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
            _ = wait_idle(&activity, IDLE_EXIT) => eprintln!("obscure-helper: idle, exiting"),
        }
        Ok(())
    })
}

/// Resolves once `idle` passes without a notification on `activity`.
async fn wait_idle(activity: &tokio::sync::Notify, idle: std::time::Duration) {
    while tokio::time::timeout(idle, activity.notified())
        .await
        .is_ok()
    {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn idle_timer_restarts_on_activity() {
        let activity = std::sync::Arc::new(tokio::sync::Notify::new());
        let started = tokio::time::Instant::now();
        let a = activity.clone();
        tokio::spawn(async move {
            for _ in 0..3 {
                tokio::time::sleep(Duration::from_secs(40)).await;
                a.notify_one();
            }
        });
        wait_idle(&activity, Duration::from_secs(60)).await;
        // Three calls 40 s apart, then 60 s of silence.
        assert_eq!(started.elapsed().as_secs(), 180);
    }
}

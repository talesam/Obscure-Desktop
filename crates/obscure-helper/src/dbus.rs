//! Prototype system D-Bus service. Every call is authorised through polkit
//! (`io.github.talesam.Obscure.grant-tun`) using the caller's bus name, so
//! an unprivileged client (including a Flatpak) can ask for the grant and
//! the desktop shows the usual authentication dialog.

use zbus::{interface, message::Header, zvariant::Value};

pub const BUS_NAME: &str = "io.github.talesam.Obscure.Helper";
pub const OBJECT_PATH: &str = "/io/github/talesam/Obscure/Helper";
const ACTION_ID: &str = "io.github.talesam.Obscure.grant-tun";

struct Helper;

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
            .to_string();
        check_authorization(connection, &sender).await?;
        crate::grant_tun(std::path::Path::new(&path)).map_err(zbus::fdo::Error::Failed)?;
        let out = std::process::Command::new("getcap")
            .arg(&path)
            .output()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// Protocol version, for clients to detect the helper.
    #[zbus(property)]
    fn version(&self) -> u32 {
        1
    }
}

/// Asks polkit whether `sender` may perform the action, allowing
/// interactive authentication (the agent shows the password dialog).
async fn check_authorization(connection: &zbus::Connection, sender: &str) -> zbus::fdo::Result<()> {
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
    let action_details: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
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
    rt.block_on(async {
        let _conn = zbus::connection::Builder::system()
            .map_err(|e| e.to_string())?
            .name(BUS_NAME)
            .map_err(|e| e.to_string())?
            .serve_at(OBJECT_PATH, Helper)
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
        }
        Ok(())
    })
}

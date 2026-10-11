//! System-wide shortcut to connect/disconnect, through the GlobalShortcuts
//! portal (the desktop owns the binding and shows the configuration UI).

use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use ashpd::desktop::{CreateSessionOptions, Session};
use futures_util::StreamExt;
use gettextrs::gettext;

pub const SHORTCUT_ID: &str = "toggle-connection";

/// Registers the shortcut and forwards each activation to `tx`. Returns
/// when the portal session ends or `stop` fires (the session is closed, so
/// turning the preference off and on never leaves two sessions behind).
pub async fn run(
    tx: async_channel::Sender<()>,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), String> {
    tokio::select! {
        r = serve(tx) => r,
        _ = &mut stop => Ok(()),
    }
}

/// Closes the portal session when dropped (stop, error or app exit).
struct CloseOnDrop(Option<Session<GlobalShortcuts>>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if let (Some(session), Ok(rt)) = (self.0.take(), tokio::runtime::Handle::try_current()) {
            rt.spawn(async move {
                let _ = session.close().await;
            });
        }
    }
}

async fn serve(tx: async_channel::Sender<()>) -> Result<(), String> {
    let portal = GlobalShortcuts::new().await.map_err(|e| e.to_string())?;
    let guard = CloseOnDrop(Some(
        portal
            .create_session(CreateSessionOptions::default())
            .await
            .map_err(|e| e.to_string())?,
    ));
    let session = guard.0.as_ref().expect("session set above");
    let shortcut = NewShortcut::new(SHORTCUT_ID, gettext("Connect or disconnect Obscure"))
        .preferred_trigger(Some("<Super><Alt>o"));
    portal
        .bind_shortcuts(session, &[shortcut], None, BindShortcutsOptions::default())
        .await
        .map_err(|e| e.to_string())?
        .response()
        .map_err(|e| e.to_string())?;
    let mut activated = portal
        .receive_activated()
        .await
        .map_err(|e| e.to_string())?;
    while let Some(ev) = activated.next().await {
        if ev.shortcut_id() == SHORTCUT_ID && tx.send(()).await.is_err() {
            break;
        }
    }
    Ok(())
}

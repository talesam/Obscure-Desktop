//! System-wide shortcut to connect/disconnect, through the GlobalShortcuts
//! portal (the desktop owns the binding and shows the configuration UI).

use ashpd::desktop::CreateSessionOptions;
use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use futures_util::StreamExt;
use gettextrs::gettext;

pub const SHORTCUT_ID: &str = "toggle-connection";

/// Registers the shortcut and forwards each activation to `tx`. Returns
/// when the portal session ends.
pub async fn run(tx: async_channel::Sender<()>) -> Result<(), String> {
    let portal = GlobalShortcuts::new().await.map_err(|e| e.to_string())?;
    let session = portal
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    let shortcut = NewShortcut::new(SHORTCUT_ID, gettext("Connect or disconnect Obscure"))
        .preferred_trigger(Some("<Super><Alt>o"));
    portal
        .bind_shortcuts(&session, &[shortcut], None, BindShortcutsOptions::default())
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

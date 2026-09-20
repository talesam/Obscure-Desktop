//! Autostart through the XDG Background portal (works natively and in
//! Flatpak). The portal writes the autostart entry itself.

use ashpd::desktop::background::Background;
use gettextrs::gettext;

/// Requests (or revokes) autostart. Returns whether autostart is granted.
pub async fn request(enable: bool) -> Result<bool, String> {
    let response = Background::request()
        .reason(&*gettext(
            "Obscure keeps your connection available from the tray.",
        ))
        .auto_start(enable)
        .command(["obscure", "--start-minimized"])
        .dbus_activatable(false)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .response()
        .map_err(|e| e.to_string())?;
    Ok(response.auto_start())
}

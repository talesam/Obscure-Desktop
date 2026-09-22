//! Status Notifier Item (tray) via `ksni`. Optional: only active when a
//! StatusNotifierWatcher exists (KDE, or GNOME with the AppIndicator
//! extension). Callbacks run on the tokio side and are forwarded to the
//! GLib main loop through a channel.

use gettextrs::gettext;
use ksni::TrayMethods;
use ksni::menu::{MenuItem, StandardItem};

use crate::config::{APP_ID, BUILD_DATADIR, PKGDATADIR, PROFILE};

/// Requests from the tray menu to the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCmd {
    ShowWindow,
    ToggleConnection,
    Quit,
}

pub struct ObscureTray {
    tx: async_channel::Sender<TrayCmd>,
    pub connected: bool,
    pub busy: bool,
    pub server_name: String,
    pub status_text: String,
}

impl ObscureTray {
    fn send(&self, cmd: TrayCmd) {
        let _ = self.tx.send_blocking(cmd);
    }
}

impl ksni::Tray for ObscureTray {
    fn id(&self) -> String {
        APP_ID.to_owned()
    }

    fn title(&self) -> String {
        "Obscure".to_owned()
    }

    fn icon_name(&self) -> String {
        if self.connected {
            format!("{APP_ID}-connected-symbolic")
        } else {
            format!("{APP_ID}-symbolic")
        }
    }

    /// When running from the build tree the icons are not installed in the
    /// theme; point the host at the build directory instead.
    fn icon_theme_path(&self) -> String {
        let installed = std::path::Path::new(PKGDATADIR).join("obscure.gresource");
        if PROFILE == "development" && !installed.exists() {
            format!("{BUILD_DATADIR}/icons")
        } else {
            String::new()
        }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let description = if self.server_name.is_empty() {
            self.status_text.clone()
        } else {
            format!("{} · {}", self.server_name, self.status_text)
        };
        ksni::ToolTip {
            title: "Obscure".to_owned(),
            description,
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayCmd::ShowWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let toggle_label = if self.connected || self.busy {
            gettext("Disconnect")
        } else {
            gettext("Connect")
        };
        vec![
            StandardItem {
                label: gettext("Show Obscure"),
                activate: Box::new(|t: &mut Self| t.send(TrayCmd::ShowWindow)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: toggle_label,
                enabled: !self.server_name.is_empty(),
                activate: Box::new(|t: &mut Self| t.send(TrayCmd::ToggleConnection)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: gettext("Quit"),
                activate: Box::new(|t: &mut Self| t.send(TrayCmd::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Registers the tray. Returns `None` when no host is available.
pub async fn spawn(tx: async_channel::Sender<TrayCmd>) -> Option<ksni::Handle<ObscureTray>> {
    // `OBSCURE_NO_TRAY=1` simulates a desktop without a tray host (tests).
    if std::env::var_os("OBSCURE_NO_TRAY").is_some() {
        tracing::info!("tray disabled by OBSCURE_NO_TRAY");
        return None;
    }
    let tray = ObscureTray {
        tx,
        connected: false,
        busy: false,
        server_name: String::new(),
        status_text: String::new(),
    };
    match tray.spawn().await {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::info!("tray not available: {e}");
            None
        }
    }
}

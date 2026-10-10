//! Fake desktop environment for system proxy tests.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

pub struct FakeDesktop {
    dir: tempfile::TempDir,
}

impl FakeDesktop {
    /// Sets up HOME/XDG dirs, a session bus that does not exist, an
    /// in-memory GSettings backend (or no GNOME schemas at all when `gnome`
    /// is false) and a `kwriteconfig6` that fails for any value pointing at
    /// 127.0.0.1. Returns `None` if `gnome` is requested but the schema is
    /// missing on this machine.
    pub fn new(gnome: bool) -> Option<Self> {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = dir.path().join("kwrite.log");
        let script = bin.join("kwriteconfig6");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$*\" in *127.0.0.1*) exit 1;; esac\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let has_schema = std::path::Path::new("/usr/share/glib-2.0/schemas/gschemas.compiled")
            .exists()
            && std::path::Path::new(
                "/usr/share/glib-2.0/schemas/org.gnome.system.proxy.gschema.xml",
            )
            .exists();
        if gnome && !has_schema {
            return None;
        }
        // SAFETY: single test per binary, set before any thread reads env.
        unsafe {
            std::env::set_var("HOME", dir.path());
            std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config"));
            std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
            std::env::set_var("PATH", format!("{}:/usr/bin:/bin", bin.display()));
            std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent");
            std::env::set_var("GSETTINGS_BACKEND", "memory");
            if !gnome {
                // No schema dirs at all: GNOME support is "absent".
                std::env::set_var("XDG_DATA_DIRS", dir.path().join("none"));
                std::env::set_var("GSETTINGS_SCHEMA_DIR", dir.path().join("none"));
            }
        }
        Some(Self { dir })
    }

    pub fn state_file(&self) -> PathBuf {
        self.dir.path().join("data/obscure/state.json")
    }

    #[allow(dead_code)]
    pub fn kwrite_calls(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("kwrite.log")).unwrap_or_default()
    }
}

#[allow(dead_code)]
pub fn gnome_mode() -> String {
    use gio::prelude::*;
    gio::Settings::new("org.gnome.system.proxy")
        .string("mode")
        .to_string()
}

//! XDG paths used by Obscure. Works natively and inside Flatpak (where the
//! XDG variables point into the sandbox).

use std::path::PathBuf;

const APP_DIR: &str = "obscure";

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `~/.config/obscure`
pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join(APP_DIR)
}

/// `~/.local/share/obscure`
pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join(APP_DIR)
}

/// `~/.cache/obscure`
pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache").join(APP_DIR)
}

/// Where the Xray binary and geo files live.
pub fn core_dir() -> PathBuf {
    data_dir().join("core")
}

pub fn profiles_file() -> PathBuf {
    config_dir().join("profiles.json")
}

pub fn state_file() -> PathBuf {
    data_dir().join("state.json")
}

/// Generated Xray configuration for the current session.
pub fn runtime_config_file() -> PathBuf {
    cache_dir().join("config.json")
}

pub fn log_dir() -> PathBuf {
    cache_dir().join("logs")
}

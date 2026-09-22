//! Privileged helper for Obscure.
//!
//! Two entry points, both narrow on purpose:
//!
//! * `obscure-helper grant-tun <xray>` — run as root through `pkexec`
//!   (polkit action `io.github.talesam.Obscure.grant-tun`). Sets the file
//!   capabilities Xray needs for its `tun` inbound. The path is validated
//!   so this cannot be turned into a generic `setcap`.
//! * `obscure-helper check-tun <xray>` — prints the current capabilities.
//! * `obscure-helper --dbus` — prototype system D-Bus service
//!   (`io.github.talesam.Obscure.Helper`) exposing `GrantTun(path)` guarded
//!   by a polkit `CheckAuthorization`. This is the path Flatpak will need,
//!   since `pkexec` cannot be used from inside the sandbox.

mod dbus;
mod validate;

use std::path::Path;
use std::process::{Command, ExitCode};

/// File capabilities Xray needs for the `tun` inbound.
pub const REQUIRED_CAPS: &str = "cap_net_admin,cap_net_raw,cap_net_bind_service+ep";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, path] if cmd == "grant-tun" => run(grant_tun(Path::new(path))),
        [cmd, path] if cmd == "check-tun" => run(check_tun(Path::new(path))),
        [flag] if flag == "--dbus" => run(dbus::serve()),
        _ => {
            eprintln!(
                "usage:\n  obscure-helper grant-tun <path-to-xray>\n  obscure-helper check-tun <path-to-xray>\n  obscure-helper --dbus"
            );
            ExitCode::from(2)
        }
    }
}

fn run(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("obscure-helper: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Grants the capabilities after validating the target.
pub fn grant_tun(path: &Path) -> Result<(), String> {
    let path = validate::xray_binary(path)?;
    let status = Command::new("setcap")
        .arg(REQUIRED_CAPS)
        .arg(&path)
        .status()
        .map_err(|e| format!("cannot run setcap: {e}"))?;
    if !status.success() {
        return Err(format!("setcap failed with {status}"));
    }
    Ok(())
}

fn check_tun(path: &Path) -> Result<(), String> {
    let path = validate::xray_binary(path)?;
    let out = Command::new("getcap")
        .arg(&path)
        .output()
        .map_err(|e| format!("cannot run getcap: {e}"))?;
    print!("{}", String::from_utf8_lossy(&out.stdout));
    Ok(())
}

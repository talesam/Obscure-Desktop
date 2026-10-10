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

mod caps;
mod dbus;
mod validate;

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, path] if cmd == "grant-tun" => {
            run(validate::pkexec_uid().and_then(|uid| grant_tun(Path::new(path), uid)))
        }
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

/// Grants the capabilities to the Xray binary of user `owner` after
/// validating it. Returns the resulting `getcap`-style line.
pub fn grant_tun(path: &Path, owner: u32) -> Result<(), String> {
    let file = validate::open_xray_binary(path, Some(owner))?;
    caps::set(&file, caps::TUN_CAPS)
}

/// Prints `<path> <caps>` like `getcap`, or nothing when there are none.
fn check_tun(path: &Path) -> Result<(), String> {
    println!("{}", describe_caps(path)?);
    Ok(())
}

pub fn describe_caps(path: &Path) -> Result<String, String> {
    let file = validate::open_xray_binary(path, None)?;
    let caps = caps::describe(&caps::get(&file)?);
    Ok(if caps.is_empty() {
        String::new()
    } else {
        format!("{} {caps}", path.display())
    })
}

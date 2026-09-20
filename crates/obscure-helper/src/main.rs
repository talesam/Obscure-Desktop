//! Privileged helper for Obscure (phase 4).
//!
//! Will become a polkit-activated D-Bus system service that opens
//! `/dev/net/tun`, sets routes and hands the fd to Xray. Until then it
//! only exists so the workspace layout is complete.

fn main() {
    eprintln!("obscure-helper: not implemented yet (see docs/PLANO.md, phase 4)");
    std::process::exit(1);
}

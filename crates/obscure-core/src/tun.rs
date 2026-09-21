//! Tunnel-mode plumbing: does the Xray binary have the file capabilities it
//! needs to open a TUN device, and can it have them at all (a `nosuid`
//! mount ignores file capabilities)?

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};

/// File capabilities Xray needs for the `tun` inbound.
pub const REQUIRED_CAPS: &str = "cap_net_admin,cap_net_raw,cap_net_bind_service+ep";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapStatus {
    /// All required capabilities are set.
    Granted,
    /// Missing or incomplete capabilities.
    Missing,
    /// Could not determine (no `getcap`, unreadable file…).
    Unknown,
}

/// Reads the file capabilities of `xray` with `getcap`.
pub fn capabilities(xray: &Path) -> CapStatus {
    let Ok(out) = Command::new("getcap").arg(xray).output() else {
        return CapStatus::Unknown;
    };
    if !out.status.success() {
        return CapStatus::Unknown;
    }
    parse_getcap(&String::from_utf8_lossy(&out.stdout))
}

/// Parses `getcap` output such as
/// `/path/xray cap_net_bind_service,cap_net_admin,cap_net_raw=ep`.
pub fn parse_getcap(output: &str) -> CapStatus {
    let line = output.trim();
    if line.is_empty() {
        return CapStatus::Missing;
    }
    // Everything after the path; formats: "path cap=ep" or "path = cap+ep".
    let caps = line
        .rsplit(' ')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let has = |c: &str| caps.contains(c);
    let effective = caps.contains("=ep") || caps.contains("+ep") || caps.contains("=eip");
    if has("cap_net_admin") && has("cap_net_raw") && effective {
        CapStatus::Granted
    } else {
        CapStatus::Missing
    }
}

/// `true` when the filesystem holding `path` is mounted `nosuid`; file
/// capabilities are ignored there, so `setcap` cannot help.
pub fn is_nosuid(path: &Path) -> bool {
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") else {
        return false;
    };
    is_nosuid_in(path, &mounts)
}

pub fn is_nosuid_in(path: &Path, mounts: &str) -> bool {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut best: Option<(PathBuf, bool)> = None;
    for line in mounts.lines() {
        let mut parts = line.split_whitespace();
        let (Some(_dev), Some(mount), Some(_fs), Some(opts)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let mount = PathBuf::from(mount.replace("\\040", " "));
        if path.starts_with(&mount)
            && best
                .as_ref()
                .is_none_or(|(m, _)| mount.as_os_str().len() > m.as_os_str().len())
        {
            let nosuid = opts.split(',').any(|o| o == "nosuid");
            best = Some((mount, nosuid));
        }
    }
    best.map(|(_, n)| n).unwrap_or(false)
}

/// Builds the privileged command that grants the capabilities through
/// polkit: `pkexec <helper> grant-tun <xray>`.
pub fn grant_command(helper: &Path, xray: &Path) -> Command {
    // `OBSCURE_PKEXEC` lets tests replace pkexec with a stub.
    let pkexec = std::env::var("OBSCURE_PKEXEC").unwrap_or_else(|_| "pkexec".into());
    let mut cmd = Command::new(pkexec);
    cmd.arg(helper).arg("grant-tun").arg(xray);
    cmd
}

/// Runs the grant command asynchronously and re-checks the capabilities.
pub async fn grant(helper: &Path, xray: &Path) -> Result<CapStatus> {
    if is_nosuid(xray) {
        return Err(Error::Tun(format!(
            "{} is on a nosuid filesystem; file capabilities are ignored there",
            xray.display()
        )));
    }
    let mut cmd = tokio::process::Command::from(grant_command(helper, xray));
    let out = cmd.output().await.map_err(|e| Error::io(helper, e))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        // pkexec: 126 = dismissed, 127 = not authorised.
        return Err(match out.status.code() {
            Some(126) => Error::TunDismissed,
            _ => Error::Tun(if stderr.is_empty() {
                format!("pkexec exited with {}", out.status)
            } else {
                stderr
            }),
        });
    }
    Ok(capabilities(xray))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn getcap_parsing() {
        assert_eq!(parse_getcap(""), CapStatus::Missing);
        assert_eq!(
            parse_getcap("/x/xray cap_net_bind_service,cap_net_admin,cap_net_raw=ep\n"),
            CapStatus::Granted
        );
        assert_eq!(
            parse_getcap("/x/xray = cap_net_admin,cap_net_raw+ep"),
            CapStatus::Granted
        );
        assert_eq!(parse_getcap("/x/xray cap_net_admin=ep"), CapStatus::Missing);
        assert_eq!(
            parse_getcap("/x/xray cap_net_admin,cap_net_raw=p"),
            CapStatus::Missing
        );
    }

    #[test]
    fn nosuid_detection_uses_longest_mount() {
        let mounts = "\
/dev/a / btrfs rw,relatime 0 0
/dev/b /home btrfs rw,noatime 0 0
tmpfs /home/u/.local/share/flatpak tmpfs rw,nosuid,nodev 0 0
/dev/c /mnt/with\\040space ext4 rw,nosuid 0 0
";
        assert!(!is_nosuid_in(
            Path::new("/home/u/.local/share/obscure/core/xray"),
            mounts
        ));
        assert!(is_nosuid_in(
            Path::new("/home/u/.local/share/flatpak/app/xray"),
            mounts
        ));
        assert!(is_nosuid_in(Path::new("/mnt/with space/xray"), mounts));
        assert!(!is_nosuid_in(Path::new("/usr/bin/xray"), mounts));
    }

    #[test]
    fn grant_command_shape() {
        let cmd = grant_command(
            Path::new("/usr/lib/obscure/obscure-helper"),
            Path::new("/x/xray"),
        );
        assert_eq!(cmd.get_program(), "pkexec");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            ["/usr/lib/obscure/obscure-helper", "grant-tun", "/x/xray"]
        );
    }

    #[test]
    fn real_binary_status_is_readable() {
        let xray = crate::paths::core_dir().join("xray");
        if xray.exists() {
            assert_ne!(capabilities(&xray), CapStatus::Unknown);
        }
    }
}

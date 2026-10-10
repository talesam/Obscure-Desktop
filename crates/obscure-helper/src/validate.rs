//! Only ever touch a file that really is the Xray binary managed by Obscure
//! for the requesting user: absolute canonical path (no symlink anywhere),
//! regular file named `xray` inside `<data dir>/obscure/core/`, owned by
//! that user and with a single hard link.
//!
//! The file is opened once and every later step works on that descriptor,
//! so swapping the path after validation cannot redirect the operation.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

/// Validates `path` and returns it opened read-only. `owner` is the uid of
/// the user who asked for the grant (`None` skips the ownership check, for
/// read-only queries).
pub fn open_xray_binary(path: &Path, owner: Option<u32>) -> Result<File, String> {
    check_layout(path)?;
    // Canonical means no `..`, no `.` and no symlink in any component.
    let canonical = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if canonical != path {
        return Err("path must not contain symlinks or relative components".into());
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    // The descriptor must still point at the validated path: closes the
    // window between `canonicalize` and `open` (a directory swapped in).
    let opened = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|e| format!("cannot verify the opened file: {e}"))?;
    if opened != path {
        return Err("file changed while it was being checked".into());
    }
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a regular file".into());
    }
    if meta.nlink() != 1 {
        return Err("refusing a file with several hard links".into());
    }
    if let Some(uid) = owner
        && meta.uid() != uid
    {
        return Err("the file does not belong to the requesting user".into());
    }
    Ok(file)
}

/// Lexical checks: `/…/obscure/core/xray` (or the Flatpak app-id folder).
fn check_layout(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("path must be absolute".into());
    }
    if path.file_name().and_then(|n| n.to_str()) != Some("xray") {
        return Err("file must be named `xray`".into());
    }
    let parent_ok = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        == Some("core");
    let grandparent_ok = path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "obscure" || n.starts_with("io.github.talesam.Obscure"));
    if !(parent_ok && grandparent_ok) {
        return Err("path must be <data dir>/obscure/core/xray".into());
    }
    Ok(())
}

/// The uid that asked for the grant through pkexec.
pub fn pkexec_uid() -> Result<u32, String> {
    std::env::var("PKEXEC_UID")
        .ok()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| "must be run through pkexec (PKEXEC_UID missing)".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn me() -> Option<u32> {
        Some(std::fs::metadata("/proc/self").unwrap().uid())
    }

    /// tempdir may live under a symlinked /tmp; work on its canonical path.
    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        (dir, root)
    }

    #[test]
    fn accepts_only_the_managed_layout() {
        let (_d, root) = tmp();
        let core = root.join("obscure").join("core");
        std::fs::create_dir_all(&core).unwrap();
        let xray = core.join("xray");
        std::fs::write(&xray, b"#!/bin/sh\n").unwrap();
        assert!(open_xray_binary(&xray, me()).is_ok());

        // Flatpak: XDG_DATA_HOME is ~/.var/app/<id>/data, so the grandparent
        // is still `obscure`; the app-id folder is accepted too.
        let flat = root.join("io.github.talesam.Obscure.Devel").join("core");
        std::fs::create_dir_all(&flat).unwrap();
        std::fs::write(flat.join("xray"), b"x").unwrap();
        assert!(open_xray_binary(&flat.join("xray"), me()).is_ok());

        assert!(open_xray_binary(Path::new("relative/xray"), me()).is_err());
        assert!(open_xray_binary(&core, me()).is_err(), "directory");
        let other = core.join("notxray");
        std::fs::write(&other, b"x").unwrap();
        assert!(open_xray_binary(&other, me()).is_err());
        let elsewhere = root.join("xray");
        std::fs::write(&elsewhere, b"x").unwrap();
        assert!(open_xray_binary(&elsewhere, me()).is_err(), "wrong parent");
    }

    #[test]
    fn rejects_symlinks_anywhere() {
        let (_d, root) = tmp();
        // A real "victim" binary outside the managed layout.
        let victim_dir = root.join("usr-bin");
        std::fs::create_dir_all(&victim_dir).unwrap();
        std::fs::write(victim_dir.join("xray"), b"victim").unwrap();

        // Last component is a symlink.
        let core = root.join("a").join("obscure").join("core");
        std::fs::create_dir_all(&core).unwrap();
        std::os::unix::fs::symlink(victim_dir.join("xray"), core.join("xray")).unwrap();
        assert!(open_xray_binary(&core.join("xray"), me()).is_err());

        // Intermediate directory is a symlink: …/obscure/core -> usr-bin.
        let obscure = root.join("b").join("obscure");
        std::fs::create_dir_all(&obscure).unwrap();
        std::os::unix::fs::symlink(&victim_dir, obscure.join("core")).unwrap();
        assert!(open_xray_binary(&obscure.join("core").join("xray"), me()).is_err());

        // `..` games.
        let sneaky = root.join("b/obscure/core/../core/xray");
        assert!(open_xray_binary(&sneaky, me()).is_err());
    }

    #[test]
    fn rejects_hard_links_and_foreign_owner() {
        let (_d, root) = tmp();
        let core = root.join("obscure").join("core");
        std::fs::create_dir_all(&core).unwrap();
        let xray = core.join("xray");
        std::fs::write(&xray, b"x").unwrap();
        // Wrong owner.
        let other_uid = me().unwrap().wrapping_add(1);
        assert!(open_xray_binary(&xray, Some(other_uid)).is_err());
        // Read-only queries skip the owner check.
        assert!(open_xray_binary(&xray, None).is_ok());
        // A second hard link (e.g. to a system binary) is refused.
        std::fs::hard_link(&xray, root.join("elsewhere")).unwrap();
        assert!(open_xray_binary(&xray, me()).is_err());
    }
}

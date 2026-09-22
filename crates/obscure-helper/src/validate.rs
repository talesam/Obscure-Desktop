//! Only ever touch a file that really looks like the Xray binary managed by
//! Obscure: absolute path, regular file, named `xray`, inside a directory
//! named `core` under an `obscure` data directory, and not a symlink.

use std::path::{Path, PathBuf};

pub fn xray_binary(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("path must be absolute".into());
    }
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.file_type().is_symlink() {
        return Err("refusing to operate on a symlink".into());
    }
    if !meta.is_file() {
        return Err("not a regular file".into());
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
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_the_managed_layout() {
        let dir = tempfile::tempdir().unwrap();
        let core = dir.path().join("obscure").join("core");
        std::fs::create_dir_all(&core).unwrap();
        let xray = core.join("xray");
        std::fs::write(&xray, b"#!/bin/sh\n").unwrap();
        assert!(xray_binary(&xray).is_ok());

        // Flatpak data dir layout: ~/.var/app/<id>/data/obscure/core? No:
        // XDG_DATA_HOME inside the sandbox is ~/.var/app/<id>/data, so the
        // grandparent is still `obscure`. Also accept the app-id folder.
        let flat = dir
            .path()
            .join("io.github.talesam.Obscure.Devel")
            .join("core");
        std::fs::create_dir_all(&flat).unwrap();
        std::fs::write(flat.join("xray"), b"x").unwrap();
        assert!(xray_binary(&flat.join("xray")).is_ok());

        assert!(xray_binary(Path::new("relative/xray")).is_err());
        assert!(xray_binary(&core).is_err(), "directory");
        let other = core.join("notxray");
        std::fs::write(&other, b"x").unwrap();
        assert!(xray_binary(&other).is_err());
        let elsewhere = dir.path().join("xray");
        std::fs::write(&elsewhere, b"x").unwrap();
        assert!(xray_binary(&elsewhere).is_err(), "wrong parent");
        let link = core.join("link");
        std::os::unix::fs::symlink(&xray, &link).unwrap();
        assert!(xray_binary(&link).is_err());
    }
}

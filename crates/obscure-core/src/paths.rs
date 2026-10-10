//! XDG paths used by Obscure. Works natively and inside Flatpak (where the
//! XDG variables point into the sandbox).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Error, Result};

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

/// Atomically replaces `path` with `contents`, readable only by the owner
/// (0600). The data goes to a uniquely named temporary file in the same
/// directory (so concurrent writers never share one), is flushed to disk,
/// renamed over `path`, and the directory entry is synced as well.
pub fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| Error::io(path, std::io::ErrorKind::InvalidInput.into()))?
        .to_string_lossy();

    // `create_new` never reuses an existing file, so the mode always applies.
    let (tmp, mut file) = loop {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".{file_name}.{}.{n}.tmp", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
        {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Error::io(&tmp, e)),
        }
    };

    let written = file
        .write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(|e| Error::io(&tmp, e))
        .and_then(|()| {
            drop(file);
            std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
        });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // Make the rename itself durable. Failure here is not worth an error:
    // the new contents are already in place.
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn write_private_is_0600_even_over_a_stale_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/p.json");
        // A world-readable leftover with the old fixed temp name must not
        // be reused (it would keep its 0644 mode).
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let stale = path.with_extension("json.tmp");
        std::fs::write(&stale, b"old").unwrap();
        std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_private(&path, b"{}").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{}");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        write_private(&path, b"[]").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"[]");
        assert_eq!(
            leftovers(path.parent().unwrap()),
            vec!["p.json.tmp".to_owned()]
        );
    }

    #[test]
    fn concurrent_writes_never_mix_or_fail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let payloads: Vec<Vec<u8>> = (0..8u8).map(|i| vec![b'a' + i; 64 * 1024]).collect();
        std::thread::scope(|s| {
            for p in &payloads {
                let path = &path;
                s.spawn(move || {
                    for _ in 0..20 {
                        write_private(path, p).unwrap();
                    }
                });
            }
        });
        let got = std::fs::read(&path).unwrap();
        assert!(payloads.contains(&got), "file holds a mix of writers");
        assert!(leftovers(dir.path()).is_empty());
    }

    #[test]
    fn write_private_cleans_up_on_error() {
        let dir = tempfile::tempdir().unwrap();
        // Renaming a file over a non-empty directory fails.
        let path = dir.path().join("target");
        std::fs::create_dir_all(path.join("inner")).unwrap();
        assert!(write_private(&path, b"x").is_err());
        assert!(leftovers(dir.path()).is_empty());
    }
}

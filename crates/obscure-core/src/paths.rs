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

/// The user's home directory. `$HOME` first; without it the password
/// database (`std::env::home_dir` uses `getpwuid_r`); and only if that fails
/// too, a private (0700, owned by us) fallback directory, never a shared
/// one such as plain `/tmp`, since profiles hold credentials.
fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return h;
    }
    if let Some(h) = std::env::home_dir().filter(|p| p.is_absolute()) {
        return h;
    }
    static FALLBACK: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    FALLBACK
        .get_or_init(|| {
            let runtime = std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute());
            let dir = private_fallback_home(runtime.as_deref(), Path::new("/tmp"));
            tracing::warn!("no home directory found; using {}", dir.display());
            dir
        })
        .clone()
}

/// UID of this process, read from `/proc/self` (no libc needed).
fn current_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

/// `true` if `dir` is a real directory (not a symlink) owned by `uid` and
/// inaccessible to group and others.
fn is_private_dir(dir: &Path, uid: Option<u32>) -> bool {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(dir) {
        Ok(m) => m.is_dir() && Some(m.uid()) == uid && m.mode() & 0o077 == 0,
        Err(_) => false,
    }
}

/// Creates (or reuses, after checking it is ours and 0700) a private home
/// replacement: `$XDG_RUNTIME_DIR/obscure-home` if the runtime dir exists,
/// else `<tmp>/obscure-<uid>`. If a squatter owns that name, a fresh
/// uniquely named directory is created instead.
fn private_fallback_home(runtime: Option<&Path>, tmp: &Path) -> PathBuf {
    use std::os::unix::fs::DirBuilderExt;
    let uid = current_uid();
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);

    if let Some(rt) = runtime.filter(|rt| is_private_dir(rt, uid)) {
        let dir = rt.join("obscure-home");
        let _ = builder.create(&dir);
        if is_private_dir(&dir, uid) {
            return dir;
        }
    }
    let uid_str = uid.map_or_else(|| "unknown".to_owned(), |u| u.to_string());
    let dir = tmp.join(format!("obscure-{uid_str}"));
    let _ = builder.create(&dir);
    if is_private_dir(&dir, uid) {
        return dir;
    }
    // Someone else holds the name (or it has loose permissions). `create`
    // fails if the path exists, so a directory it makes is always new, and
    // the mode passed at creation is never widened by a pre-existing entry.
    for n in 0u32.. {
        let dir = tmp.join(format!("obscure-{uid_str}-{}-{n}", std::process::id()));
        if builder.create(&dir).is_ok() && is_private_dir(&dir, uid) {
            return dir;
        }
        if n > 1000 {
            break;
        }
    }
    // Nothing writable at all: there is nowhere safe to keep data, and any
    // write below will fail with a clear I/O error rather than leak.
    tmp.join(format!("obscure-{uid_str}"))
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

    /// Runs in a child process (see `home_without_env_var_is_not_tmp`):
    /// env mutation in-process would race with other tests.
    #[test]
    fn child_home_unset() {
        if std::env::var_os("OBSCURE_PATHS_CHILD").is_none() {
            return;
        }
        assert!(std::env::var_os("HOME").is_none());
        let cfg = config_dir();
        println!("CONFIG_DIR={}", cfg.display());
        assert!(cfg.is_absolute());
        assert!(!cfg.starts_with("/tmp/.config"), "{}", cfg.display());
        if let Some(h) = std::env::home_dir() {
            assert_eq!(cfg, h.join(".config/obscure"));
        }
    }

    #[test]
    fn home_without_env_var_is_not_tmp() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "paths::tests::child_home_unset",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("OBSCURE_PATHS_CHILD", "1")
            .env_remove("HOME")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_CACHE_HOME")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "child failed: {stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.contains("CONFIG_DIR=/"),
            "child did not run: {stdout}"
        );
    }

    fn mode_of(p: &Path) -> u32 {
        std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn fallback_home_is_private_and_reused() {
        let tmp = tempfile::tempdir().unwrap();
        let a = private_fallback_home(None, tmp.path());
        assert!(a.starts_with(tmp.path()));
        assert!(a.is_dir());
        assert_eq!(mode_of(&a), 0o700);
        assert_eq!(private_fallback_home(None, tmp.path()), a, "reused");

        // A runtime dir that is private wins over the tmp dir.
        let rt = tmp.path().join("rt");
        std::fs::create_dir(&rt).unwrap();
        std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o700)).unwrap();
        let b = private_fallback_home(Some(&rt), tmp.path());
        assert_eq!(b, rt.join("obscure-home"));
        assert_eq!(mode_of(&b), 0o700);

        // A world-accessible runtime dir is not trusted.
        std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(private_fallback_home(Some(&rt), tmp.path()), a);
    }

    #[test]
    fn fallback_home_refuses_loose_or_symlinked_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let uid = current_uid().unwrap();
        let name = tmp.path().join(format!("obscure-{uid}"));

        // Pre-created with loose permissions (could be someone else's trap).
        std::fs::create_dir(&name).unwrap();
        std::fs::set_permissions(&name, std::fs::Permissions::from_mode(0o777)).unwrap();
        let got = private_fallback_home(None, tmp.path());
        assert_ne!(got, name);
        assert_eq!(mode_of(&got), 0o700);
        assert_eq!(mode_of(&name), 0o777, "foreign dir left untouched");
        std::fs::remove_dir(&name).unwrap();

        // A symlink to a private dir is still refused.
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&target, &name).unwrap();
        let got = private_fallback_home(None, tmp.path());
        assert_ne!(got, name);
        assert!(!std::fs::symlink_metadata(&got).unwrap().is_symlink());
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

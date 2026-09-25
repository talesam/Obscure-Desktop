//! Downloads, verifies and updates the Xray-core binary and geo files.
//! See docs/PLANO.md §3.3.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use futures_util::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

const RELEASES_API: &str = "https://api.github.com/repos/XTLS/Xray-core/releases";
const GEO_BASE: &str = "https://github.com/Loyalsoldier/v2ray-rules-dat/releases/latest/download";
const GEO_FILES: [&str; 2] = ["geoip.dat", "geosite.dat"];
const GEO_STAMP: &str = "geo-updated";
const CORE_STAMP: &str = "core-checked";
/// The core release list is consulted at most once per this interval.
pub const CORE_UPDATE_INTERVAL: Duration = Duration::from_secs(24 * 3600);
/// Geo files are refreshed at most once per this interval.
pub const GEO_UPDATE_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// Progress of a download/installation, for the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    /// Looking up the latest release.
    Resolving,
    /// Bytes downloaded so far and total when known.
    Downloading {
        received: u64,
        total: Option<u64>,
    },
    Verifying,
    Extracting,
    Done {
        version: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub zip_url: String,
    pub dgst_url: String,
}

#[derive(Debug, Deserialize)]
struct ApiRelease {
    tag_name: String,
    prerelease: bool,
    assets: Vec<ApiAsset>,
}

#[derive(Debug, Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

/// First Xray release whose `tun` inbound assigns addresses and installs
/// system routes on Linux by itself (`gateway`, `autoSystemRoutingTable`).
/// Older cores only create the interface, so tunnel mode silently carries
/// no traffic with them.
pub const MIN_TUN_ROUTING_VERSION: (u32, u32, u32) = (26, 7, 11);

/// Parses a tag such as `v26.7.11` into `(26, 7, 11)`.
pub fn parse_version(tag: &str) -> Option<(u32, u32, u32)> {
    let mut it = tag.trim().trim_start_matches('v').split('.');
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    let c = it.next()?.parse().ok()?;
    Some((a, b, c))
}

/// `true` when this core version can set up tunnel routing on its own.
pub fn supports_tun_routing(tag: &str) -> bool {
    parse_version(tag).is_some_and(|v| v >= MIN_TUN_ROUTING_VERSION)
}

/// Name of the Xray asset for the running machine.
pub fn asset_name() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("Xray-linux-64.zip"),
        "aarch64" => Ok("Xray-linux-arm64-v8a.zip"),
        other => Err(Error::NoAssetForArch(other.to_owned())),
    }
}

/// Manages the core installation directory (`~/.local/share/obscure/core`).
#[derive(Debug, Clone)]
pub struct CoreManager {
    dir: PathBuf,
    client: reqwest::Client,
}

impl CoreManager {
    pub fn new(dir: PathBuf, user_agent: &str) -> Self {
        crate::init_crypto();
        let client = reqwest::Client::builder()
            .user_agent(user_agent)
            .build()
            .expect("reqwest client");
        Self { dir, client }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn xray_path(&self) -> PathBuf {
        self.dir.join("xray")
    }

    pub fn is_installed(&self) -> bool {
        self.xray_path().is_file()
            && self.dir.join("geoip.dat").is_file()
            && self.dir.join("geosite.dat").is_file()
    }

    /// Installed version tag (e.g. `v26.3.27`), if any.
    pub fn installed_version(&self) -> Option<String> {
        std::fs::read_to_string(self.dir.join("VERSION"))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    /// Resolves the latest release (stable by default; `prerelease` allows
    /// pre-releases too).
    pub async fn latest_release(&self, prerelease: bool) -> Result<Release> {
        let asset = asset_name()?;
        let url = if prerelease {
            format!("{RELEASES_API}?per_page=10")
        } else {
            format!("{RELEASES_API}/latest")
        };
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let releases: Vec<ApiRelease> = if prerelease {
            resp.json().await?
        } else {
            vec![resp.json().await?]
        };
        let release = releases
            .into_iter()
            .find(|r| (prerelease || !r.prerelease) && r.assets.iter().any(|a| a.name == asset))
            .ok_or_else(|| Error::NoAssetForArch(asset.to_owned()))?;
        let find = |name: &str| {
            release
                .assets
                .iter()
                .find(|a| a.name == name)
                .map(|a| a.browser_download_url.clone())
                .ok_or_else(|| Error::NoAssetForArch(name.to_owned()))
        };
        Ok(Release {
            version: release.tag_name.clone(),
            zip_url: find(asset)?,
            dgst_url: find(&format!("{asset}.dgst"))?,
        })
    }

    /// Downloads, verifies and installs `release`, reporting progress.
    pub async fn install(
        &self,
        release: &Release,
        mut progress: impl FnMut(Progress),
    ) -> Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(|e| Error::io(&self.dir, e))?;
        let zip_path = self.dir.join("download.zip.part");

        // Digest first: small, and lets us fail early if malformed.
        let dgst = self
            .client
            .get(&release.dgst_url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let expected = parse_sha256(&dgst)
            .ok_or_else(|| Error::MalformedLink("no SHA2-256 line in .dgst".into()))?;

        // Stream the archive to disk while hashing.
        let resp = self
            .client
            .get(&release.zip_url)
            .send()
            .await?
            .error_for_status()?;
        let total = resp.content_length();
        let mut received = 0u64;
        progress(Progress::Downloading { received, total });
        let mut file = std::fs::File::create(&zip_path).map_err(|e| Error::io(&zip_path, e))?;
        let mut hasher = Sha256::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            hasher.update(&chunk);
            file.write_all(&chunk)
                .map_err(|e| Error::io(&zip_path, e))?;
            received += chunk.len() as u64;
            progress(Progress::Downloading { received, total });
        }
        file.flush().map_err(|e| Error::io(&zip_path, e))?;
        drop(file);

        progress(Progress::Verifying);
        let actual = hex::encode(hasher.finalize());
        if actual != expected {
            let _ = std::fs::remove_file(&zip_path);
            return Err(Error::Checksum {
                file: release.zip_url.clone(),
                expected,
                actual,
            });
        }

        progress(Progress::Extracting);
        let dir = self.dir.clone();
        let zip_for_extract = zip_path.clone();
        tokio::task::spawn_blocking(move || extract(&zip_for_extract, &dir))
            .await
            .expect("extract task panicked")?;
        let _ = std::fs::remove_file(&zip_path);

        let version_file = self.dir.join("VERSION");
        std::fs::write(&version_file, format!("{}\n", release.version))
            .map_err(|e| Error::io(&version_file, e))?;
        progress(Progress::Done {
            version: release.version.clone(),
        });
        Ok(())
    }

    /// `true` if the release list was not consulted in the last day.
    pub fn core_needs_check(&self) -> bool {
        stamp_older_than(&self.dir.join(CORE_STAMP), CORE_UPDATE_INTERVAL)
    }

    fn touch_core_stamp(&self) {
        let stamp = self.dir.join(CORE_STAMP);
        let _ = std::fs::write(&stamp, b"");
    }

    /// Installs the core if missing and otherwise, at most once a day,
    /// upgrades it to the newest release. Xray publishes its regular
    /// releases as pre-releases and only rarely promotes a stable tag, so
    /// "newest" includes pre-releases. Returns the version in use.
    pub async fn ensure_latest(&self, progress: impl FnMut(Progress)) -> Result<String> {
        let mut progress = progress;
        let installed = if self.is_installed() {
            self.installed_version()
        } else {
            None
        };
        if let Some(v) = &installed
            && !self.core_needs_check()
        {
            return Ok(v.clone());
        }
        progress(Progress::Resolving);
        let release = match self.latest_release(true).await {
            Ok(r) => r,
            // Offline with a working core: keep using it.
            Err(e) if installed.is_some() => {
                tracing::warn!(
                    "core update check failed, keeping {}: {e}",
                    installed.as_deref().unwrap_or("?")
                );
                return Ok(installed.unwrap());
            }
            Err(e) => return Err(e),
        };
        if installed.as_deref() != Some(release.version.as_str()) {
            self.install(&release, progress).await?;
        }
        self.touch_core_stamp();
        Ok(release.version)
    }

    /// Makes sure the installed core can do tunnel routing, installing the
    /// newest release (pre-releases included, since the feature is not in a
    /// stable release yet) when the installed one is too old. Returns the
    /// version in use and whether the binary was replaced.
    pub async fn ensure_tun_capable(
        &self,
        progress: impl FnMut(Progress),
    ) -> Result<(String, bool)> {
        if self.is_installed()
            && let Some(v) = self.installed_version()
            && supports_tun_routing(&v)
        {
            return Ok((v, false));
        }
        let mut progress = progress;
        progress(Progress::Resolving);
        let release = self.latest_release(true).await?;
        if !supports_tun_routing(&release.version) {
            return Err(Error::Tun(format!(
                "newest core {} does not support tunnel routing yet",
                release.version
            )));
        }
        self.install(&release, progress).await?;
        Ok((release.version, true))
    }

    /// Convenience: resolve + install the latest release if nothing is
    /// installed yet. Returns the installed version.
    pub async fn ensure_installed(
        &self,
        prerelease: bool,
        progress: impl FnMut(Progress),
    ) -> Result<String> {
        if self.is_installed()
            && let Some(v) = self.installed_version()
        {
            return Ok(v);
        }
        let mut progress = progress;
        progress(Progress::Resolving);
        let release = self.latest_release(prerelease).await?;
        self.install(&release, progress).await?;
        Ok(release.version)
    }
}

impl CoreManager {
    /// `true` if the geo files were never refreshed or are older than
    /// [`GEO_UPDATE_INTERVAL`].
    pub fn geo_needs_update(&self) -> bool {
        stamp_older_than(&self.dir.join(GEO_STAMP), GEO_UPDATE_INTERVAL)
    }

    /// Downloads `geoip.dat` and `geosite.dat` from Loyalsoldier's daily
    /// releases, verifying each against its `.sha256sum`. Files are swapped
    /// in atomically; on any failure the previous files stay in place.
    pub async fn update_geo(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(|e| Error::io(&self.dir, e))?;
        for name in GEO_FILES {
            let sum = self
                .client
                .get(format!("{GEO_BASE}/{name}.sha256sum"))
                .send()
                .await?
                .error_for_status()?
                .text()
                .await?;
            let expected = parse_sha256sum_line(&sum)
                .ok_or_else(|| Error::Subscription(format!("malformed {name}.sha256sum")))?;
            let bytes = self
                .client
                .get(format!("{GEO_BASE}/{name}"))
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await?;
            let actual = hex::encode(Sha256::digest(&bytes));
            if actual != expected {
                return Err(Error::Checksum {
                    file: name.to_owned(),
                    expected,
                    actual,
                });
            }
            let tmp = self.dir.join(format!("{name}.tmp"));
            let dest = self.dir.join(name);
            std::fs::write(&tmp, &bytes).map_err(|e| Error::io(&tmp, e))?;
            std::fs::rename(&tmp, &dest).map_err(|e| Error::io(&dest, e))?;
        }
        let stamp = self.dir.join(GEO_STAMP);
        std::fs::write(&stamp, b"").map_err(|e| Error::io(&stamp, e))?;
        Ok(())
    }
}

fn stamp_older_than(stamp: &Path, interval: Duration) -> bool {
    match std::fs::metadata(stamp).and_then(|m| m.modified()) {
        Ok(modified) => SystemTime::now()
            .duration_since(modified)
            .map(|age| age >= interval)
            .unwrap_or(true),
        Err(_) => true,
    }
}

/// Parses `<hex>  <filename>` (sha256sum format) and returns the hex digest.
pub fn parse_sha256sum_line(text: &str) -> Option<String> {
    let hex = text.split_whitespace().next()?.to_ascii_lowercase();
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(hex)
}

/// Extracts `xray`, `geoip.dat` and `geosite.dat` from the archive into
/// `dir`, writing to temp files and renaming so a crash never leaves a
/// half-written binary in place.
fn extract(zip_path: &Path, dir: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path).map_err(|e| Error::io(zip_path, e))?;
    let mut archive = zip::ZipArchive::new(file)?;
    for name in ["xray", "geoip.dat", "geosite.dat"] {
        let mut entry = archive.by_name(name)?;
        let tmp = dir.join(format!("{name}.tmp"));
        let dest = dir.join(name);
        {
            let mut out = std::fs::File::create(&tmp).map_err(|e| Error::io(&tmp, e))?;
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry
                .read_to_end(&mut buf)
                .map_err(|e| Error::io(&tmp, e))?;
            out.write_all(&buf).map_err(|e| Error::io(&tmp, e))?;
        }
        if name == "xray" {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| Error::io(&tmp, e))?;
        }
        std::fs::rename(&tmp, &dest).map_err(|e| Error::io(&dest, e))?;
    }
    Ok(())
}

/// Extracts the SHA-256 hex digest from a Xray `.dgst` file.
pub fn parse_sha256(dgst: &str) -> Option<String> {
    dgst.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        if key.trim().eq_ignore_ascii_case("SHA2-256") || key.trim().eq_ignore_ascii_case("SHA256")
        {
            let v = value.trim().to_ascii_lowercase();
            (v.len() == 64 && v.chars().all(|c| c.is_ascii_hexdigit())).then_some(v)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dgst() {
        let dgst = "MD5= ee4e\nSHA1= b55b\nSHA2-256= 23CD9AF937744D97776EE35ECAD4972CF4B2109D1E0FE6BE9930467608F7C8AE\nSHA2-512= e8bc\n";
        assert_eq!(
            parse_sha256(dgst).unwrap(),
            "23cd9af937744d97776ee35ecad4972cf4b2109d1e0fe6be9930467608f7c8ae"
        );
        assert_eq!(parse_sha256("SHA2-256= nothex"), None);
        assert_eq!(parse_sha256(""), None);
    }

    #[test]
    fn sha256sum_line() {
        assert_eq!(
            parse_sha256sum_line(
                "23CD9AF937744D97776EE35ECAD4972CF4B2109D1E0FE6BE9930467608F7C8AE  geoip.dat\n"
            )
            .unwrap(),
            "23cd9af937744d97776ee35ecad4972cf4b2109d1e0fe6be9930467608f7c8ae"
        );
        assert_eq!(parse_sha256sum_line("zz  x"), None);
    }

    #[test]
    fn core_stamp_controls_check() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = CoreManager::new(dir.path().to_path_buf(), "test");
        assert!(mgr.core_needs_check());
        mgr.touch_core_stamp();
        assert!(!mgr.core_needs_check());
    }

    #[test]
    fn geo_stamp_controls_update() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = CoreManager::new(dir.path().to_path_buf(), "test");
        assert!(mgr.geo_needs_update());
        std::fs::write(dir.path().join(GEO_STAMP), b"").unwrap();
        assert!(!mgr.geo_needs_update());
    }

    #[test]
    fn version_parsing_and_tun_support() {
        assert_eq!(parse_version("v26.7.11"), Some((26, 7, 11)));
        assert_eq!(parse_version("26.3.27\n"), Some((26, 3, 27)));
        assert_eq!(parse_version("garbage"), None);
        assert!(!supports_tun_routing("v26.3.27"));
        assert!(!supports_tun_routing("v26.6.27"));
        assert!(supports_tun_routing("v26.7.11"));
        assert!(supports_tun_routing("v26.9.9"));
        assert!(!supports_tun_routing(""));
    }

    #[test]
    fn asset_for_this_machine() {
        let name = asset_name().unwrap();
        assert!(name.starts_with("Xray-linux-"));
    }

    #[test]
    fn extract_and_version_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("a.zip");
        {
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut w = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            for (name, body) in [
                ("xray", "#!/bin/sh\necho xray\n"),
                ("geoip.dat", "ip"),
                ("geosite.dat", "site"),
                ("README.md", "x"),
            ] {
                w.start_file(name, opts).unwrap();
                w.write_all(body.as_bytes()).unwrap();
            }
            w.finish().unwrap();
        }
        let core = dir.path().join("core");
        std::fs::create_dir_all(&core).unwrap();
        extract(&zip_path, &core).unwrap();
        let mgr = CoreManager::new(core.clone(), "test");
        assert!(mgr.is_installed());
        assert_eq!(mgr.installed_version(), None);
        std::fs::write(core.join("VERSION"), "v1.2.3\n").unwrap();
        assert_eq!(mgr.installed_version().unwrap(), "v1.2.3");
        use std::os::unix::fs::PermissionsExt;
        assert!(
            std::fs::metadata(core.join("xray"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111
                != 0
        );
        assert!(!core.join("README.md").exists());
    }
}

//! Downloads, verifies and updates the Xray-core binary and geo files.
//! See docs/PLANO.md §3.3.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use futures_util::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::error::{Error, Result};

const RELEASES_API: &str = "https://api.github.com/repos/XTLS/Xray-core/releases";
const GEO_BASE: &str = "https://github.com/Loyalsoldier/v2ray-rules-dat/releases/latest/download";
const GEO_FILES: [&str; 2] = ["geoip.dat", "geosite.dat"];
const GEO_STAMP: &str = "geo-updated";
/// Touched when a geo update fails; postpones the next attempt.
const GEO_RETRY_STAMP: &str = "geo-retry";
/// After a failed geo update, wait this long before trying again.
pub const GEO_RETRY_INTERVAL: Duration = Duration::from_secs(3600);
/// Files a core installation consists of (besides `VERSION`).
const CORE_FILES: [&str; 3] = ["xray", "geoip.dat", "geosite.dat"];
/// Minimum time between two download progress reports.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Temporary files older than this are leftovers of a crashed install.
const STALE_TEMP_AGE: Duration = Duration::from_secs(3600);
const CORE_STAMP: &str = "core-checked";
/// The core release list is consulted at most once per this interval.
pub const CORE_UPDATE_INTERVAL: Duration = Duration::from_secs(24 * 3600);
/// Geo files are refreshed at most once per this interval.
/// Connection setup limit for every request: networks that silently drop
/// GitHub traffic must not leave "Connecting…" hanging forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum silence between two reads of a response body (large downloads
/// have no total limit, only this one).
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Total limit for small requests (release list, checksums).
const API_TIMEOUT: Duration = Duration::from_secs(20);

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

/// Parses a tag such as `v26.7.11` (or `v26.7.11-rc1`) into `(26, 7, 11)`.
/// Any pre-release/build suffix is ignored here; see [`compare_versions`]
/// for an ordering that takes it into account.
pub fn parse_version(tag: &str) -> Option<(u32, u32, u32)> {
    parse_full_version(tag).map(|(v, _)| v)
}

/// `(major, minor, patch)` and the pre-release suffix.
type FullVersion<'a> = ((u32, u32, u32), Option<&'a str>);

/// `(major, minor, patch)` plus the pre-release suffix, if any
/// (`v26.7.11-rc1` → `((26, 7, 11), Some("rc1"))`). Build metadata after
/// `+` is dropped.
fn parse_full_version(tag: &str) -> Option<FullVersion<'_>> {
    let s = tag.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    let s = s.split_once('+').map_or(s, |(v, _)| v);
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => (c, Some(p).filter(|p| !p.is_empty())),
        None => (s, None),
    };
    let mut it = core.split('.');
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    let c = it.next()?.parse().ok()?;
    Some(((a, b, c), pre))
}

/// Orders two pre-release suffixes: dot-separated identifiers, each split
/// into alternating letter/digit runs so that `rc10 > rc9` and `beta > alpha`.
fn compare_pre(a: &str, b: &str) -> std::cmp::Ordering {
    fn runs(s: &str) -> Vec<&str> {
        let mut out = Vec::new();
        for ident in s.split(['.', '-']).filter(|i| !i.is_empty()) {
            let mut start = 0;
            let mut prev_digit = None;
            for (i, ch) in ident.char_indices() {
                let digit = ch.is_ascii_digit();
                if prev_digit.is_some_and(|p| p != digit) {
                    out.push(&ident[start..i]);
                    start = i;
                }
                prev_digit = Some(digit);
            }
            out.push(&ident[start..]);
        }
        out
    }
    let (ra, rb) = (runs(a), runs(b));
    for (x, y) in ra.iter().zip(&rb) {
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(m), Ok(n)) => m.cmp(&n),
            // Numeric identifiers sort below alphanumeric ones (semver).
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            (Err(_), Err(_)) => x.cmp(y),
        };
        if ord.is_ne() {
            return ord;
        }
    }
    ra.len().cmp(&rb.len())
}

/// Compares two version tags; `None` if either cannot be read. A
/// pre-release sorts below the plain version (`v26.7.11-rc1 < v26.7.11`).
pub fn compare_versions(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let (va, pa) = parse_full_version(a)?;
    let (vb, pb) = parse_full_version(b)?;
    Some(va.cmp(&vb).then_with(|| match (pa, pb) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(x), Some(y)) => compare_pre(x, y),
    }))
}

/// `true` if `release` should replace `installed`: never downgrade, and
/// replace anything whose version cannot be read.
fn is_upgrade(release: &str, installed: Option<&str>) -> bool {
    let Some(old) = installed.filter(|o| parse_full_version(o).is_some()) else {
        return true;
    };
    compare_versions(release, old).is_some_and(|o| o.is_gt())
}

/// `true` when this core version can set up tunnel routing on its own. A
/// pre-release of the minimum version does not count.
pub fn supports_tun_routing(tag: &str) -> bool {
    parse_full_version(tag).is_some_and(|(v, pre)| {
        v > MIN_TUN_ROUTING_VERSION || (v == MIN_TUN_ROUTING_VERSION && pre.is_none())
    })
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
    releases_api: String,
    geo_base: String,
    api_timeout: Duration,
}

impl CoreManager {
    pub fn new(dir: PathBuf, user_agent: &str) -> Self {
        crate::init_crypto();
        let client = reqwest::Client::builder()
            .user_agent(user_agent)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .expect("reqwest client");
        Self {
            dir,
            client,
            releases_api: RELEASES_API.to_owned(),
            geo_base: GEO_BASE.to_owned(),
            api_timeout: API_TIMEOUT,
        }
    }

    /// Points release lookups at another API (tests use a local server).
    #[doc(hidden)]
    pub fn with_releases_api(mut self, url: &str, timeout: Duration) -> Self {
        self.releases_api = url.trim_end_matches('/').to_owned();
        self.api_timeout = timeout;
        self
    }

    /// Points geo downloads at another base URL (tests use a local server).
    #[doc(hidden)]
    pub fn with_geo_base(mut self, url: &str) -> Self {
        self.geo_base = url.trim_end_matches('/').to_owned();
        self
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
            format!("{}?per_page=10", self.releases_api)
        } else {
            format!("{}/latest", self.releases_api)
        };
        let resp = self
            .client
            .get(&url)
            .timeout(self.api_timeout)
            .send()
            .await?
            .error_for_status()?;
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
    ///
    /// `progress` is called from the async task that runs the download, so
    /// it must not block: post to an unbounded channel or similar (the app
    /// uses `async_channel::unbounded` + `send_blocking`, which never waits
    /// on an unbounded channel). Download progress is throttled to about
    /// ten calls per second.
    ///
    /// The archive is streamed to a uniquely named temporary file, verified,
    /// and then every file is extracted and synced to disk before any of
    /// them replaces the installed one; `VERSION` is written last,
    /// atomically. Temporary files are removed on every failure path.
    pub async fn install(
        &self,
        release: &Release,
        mut progress: impl FnMut(Progress),
    ) -> Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(|e| Error::io(&self.dir, e))?;
        remove_legacy_temps(&self.dir);

        // Digest first: small, and lets us fail early if malformed.
        let dgst = self
            .client
            .get(&release.dgst_url)
            .timeout(self.api_timeout)
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
        // Removed when `zip` is dropped: on error, cancellation or success.
        let (zip, file) = TempFile::create(&self.dir, "download.zip.part", 0o600)?;
        let mut out =
            tokio::io::BufWriter::with_capacity(256 * 1024, tokio::fs::File::from_std(file));
        let mut hasher = Sha256::new();
        let mut stream = resp.bytes_stream();
        let mut last_report = std::time::Instant::now();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            hasher.update(&chunk);
            out.write_all(&chunk)
                .await
                .map_err(|e| Error::io(zip.path(), e))?;
            received += chunk.len() as u64;
            if last_report.elapsed() >= PROGRESS_INTERVAL {
                last_report = std::time::Instant::now();
                progress(Progress::Downloading { received, total });
            }
        }
        progress(Progress::Downloading { received, total });
        out.flush().await.map_err(|e| Error::io(zip.path(), e))?;
        let file = out.into_inner();
        file.sync_all()
            .await
            .map_err(|e| Error::io(zip.path(), e))?;
        drop(file);

        progress(Progress::Verifying);
        let actual = hex::encode(hasher.finalize());
        if actual != expected {
            return Err(Error::Checksum {
                file: release.zip_url.clone(),
                expected,
                actual,
            });
        }

        progress(Progress::Extracting);
        let dir = self.dir.clone();
        let zip_for_extract = zip.path().to_path_buf();
        tokio::task::spawn_blocking(move || extract(&zip_for_extract, &dir))
            .await
            .map_err(|e| Error::io(zip.path(), std::io::Error::other(e)))??;
        drop(zip);

        // Last: only a complete installation gets a version.
        let version_file = self.dir.join("VERSION");
        crate::paths::write_private(&version_file, format!("{}\n", release.version).as_bytes())?;

        // Exec of a just-written binary can fail with ETXTBSY for a moment
        // if another thread forked while the write descriptor was open (the
        // child holds it until its own exec). Wait it out here so whoever
        // spawns the core next does not trip over it.
        wait_until_executable(&self.xray_path()).await;

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
        if !is_upgrade(&release.version, installed.as_deref()) {
            self.touch_core_stamp();
            return Ok(installed.unwrap_or(release.version));
        }
        match self.install(&release, progress).await {
            Ok(()) => {
                self.touch_core_stamp();
                Ok(release.version)
            }
            // A failed upgrade must not block connecting with a working core;
            // the stamp postpones the next attempt by a day.
            Err(e) if installed.is_some() => {
                tracing::warn!(
                    "core upgrade to {} failed, keeping {}: {e}",
                    release.version,
                    installed.as_deref().unwrap_or("?")
                );
                self.touch_core_stamp();
                Ok(installed.unwrap())
            }
            Err(e) => Err(e),
        }
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
    /// [`GEO_UPDATE_INTERVAL`], and no failed attempt happened in the last
    /// [`GEO_RETRY_INTERVAL`] (so an unreachable server is not hit on
    /// every connection).
    pub fn geo_needs_update(&self) -> bool {
        stamp_older_than(&self.dir.join(GEO_STAMP), GEO_UPDATE_INTERVAL)
            && stamp_older_than(&self.dir.join(GEO_RETRY_STAMP), GEO_RETRY_INTERVAL)
    }

    /// Downloads `geoip.dat` and `geosite.dat` from Loyalsoldier's daily
    /// releases, verifying each against its `.sha256sum`. Files are swapped
    /// in atomically; on any failure the previous files stay in place and
    /// the next attempt is postponed by [`GEO_RETRY_INTERVAL`].
    pub async fn update_geo(&self) -> Result<()> {
        let result = self.update_geo_from(&self.geo_base).await;
        let retry = self.dir.join(GEO_RETRY_STAMP);
        match &result {
            Ok(()) => {
                let _ = std::fs::remove_file(&retry);
            }
            Err(_) => {
                let _ = std::fs::create_dir_all(&self.dir);
                let _ = std::fs::write(&retry, b"");
            }
        }
        result
    }

    async fn update_geo_from(&self, base: &str) -> Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(|e| Error::io(&self.dir, e))?;
        for name in GEO_FILES {
            let sum = self
                .client
                .get(format!("{base}/{name}.sha256sum"))
                .timeout(self.api_timeout)
                .send()
                .await?
                .error_for_status()?
                .text()
                .await?;
            let expected = parse_sha256sum_line(&sum)
                .ok_or_else(|| Error::Subscription(format!("malformed {name}.sha256sum")))?;
            let bytes = self
                .client
                .get(format!("{base}/{name}"))
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
            let dir = self.dir.clone();
            tokio::task::spawn_blocking(move || replace_file(&dir, name, &bytes, 0o644))
                .await
                .map_err(|e| Error::io(name, std::io::Error::other(e)))??;
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

/// A temporary file in the installation directory, removed on drop unless
/// [`TempFile::persist`] moved it into place. Names are unique per process
/// and call (`.{name}.{pid}.{n}.tmp`), so concurrent installs never write
/// to the same file.
struct TempFile {
    path: PathBuf,
    armed: bool,
}

impl TempFile {
    fn create(dir: &Path, name: &str, mode: u32) -> Result<(Self, std::fs::File)> {
        use std::os::unix::fs::OpenOptionsExt;
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        loop {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(&path)
            {
                Ok(f) => return Ok((Self { path, armed: true }, f)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(Error::io(&path, e)),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Renames the file over `dest`; it is no longer removed on drop.
    fn persist(mut self, dest: &Path) -> Result<()> {
        std::fs::rename(&self.path, dest).map_err(|e| Error::io(dest, e))?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Syncs a directory so that renames inside it survive a crash. Failure
/// is not worth an error: the files themselves are already in place.
fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Atomically replaces `dir/name` with `contents` (written, synced, then
/// renamed).
fn replace_file(dir: &Path, name: &str, contents: &[u8], mode: u32) -> Result<()> {
    let (tmp, mut file) = TempFile::create(dir, name, mode)?;
    file.write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(|e| Error::io(tmp.path(), e))?;
    drop(file);
    tmp.persist(&dir.join(name))?;
    sync_dir(dir);
    Ok(())
}

/// Removes temporary files left by older versions (fixed names) or by a
/// crashed install of this one. Only our own naming patterns are touched.
fn remove_legacy_temps(dir: &Path) {
    let _ = std::fs::remove_file(dir.join("download.zip.part"));
    for name in CORE_FILES {
        let _ = std::fs::remove_file(dir.join(format!("{name}.tmp")));
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let pid = std::process::id().to_string();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(n) = file_name.to_str() else {
            continue;
        };
        let ours = CORE_FILES
            .iter()
            .chain(std::iter::once(&"download.zip.part"))
            .any(|f| n.starts_with(&format!(".{f}.")));
        // Never remove temp files of a live install in this process.
        let this_process = n.split('.').nth(n.split('.').count().saturating_sub(3)) == Some(&pid);
        if ours && n.ends_with(".tmp") && !this_process {
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age > STALE_TEMP_AGE);
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Retries running `xray version` while exec fails with ETXTBSY (errno 26),
/// for up to about a second. Any other outcome ends the wait.
async fn wait_until_executable(xray: &Path) {
    const ETXTBSY: i32 = 26;
    for _ in 0..50 {
        let out = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new(xray)
                .arg("version")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await;
        match out {
            Ok(Err(e)) if e.raw_os_error() == Some(ETXTBSY) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok(Err(e)) => {
                tracing::warn!("installed core cannot be executed: {e}");
                return;
            }
            _ => return,
        }
    }
}

/// Largest archive entry we accept. Xray is ~30 MB and the geo files are
/// below 20 MB; anything far larger is not a real release.
const MAX_ENTRY_SIZE: u64 = 512 * 1024 * 1024;

/// Extracts `xray`, `geoip.dat` and `geosite.dat` from the archive into
/// `dir`. Every file is first written to a temporary file and synced; only
/// when all three are on disk are they renamed into place, so a missing or
/// broken entry leaves the previous installation untouched. Only these
/// exact names are read, so other entries (`../x`, absolute paths) are
/// never written anywhere.
fn extract(zip_path: &Path, dir: &Path) -> Result<()> {
    extract_limited(zip_path, dir, MAX_ENTRY_SIZE)
}

fn extract_limited(zip_path: &Path, dir: &Path, max_entry: u64) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let file = std::fs::File::open(zip_path).map_err(|e| Error::io(zip_path, e))?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut staged = Vec::with_capacity(CORE_FILES.len());
    for name in CORE_FILES {
        let entry = archive.by_name(name)?;
        let too_big = || {
            Error::io(
                dir.join(name),
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("archive entry {name} is larger than {max_entry} bytes"),
                ),
            )
        };
        if entry.size() > max_entry {
            return Err(too_big());
        }
        let mode = if name == "xray" { 0o755 } else { 0o644 };
        let (tmp, out) = TempFile::create(dir, name, mode)?;
        let mut writer = std::io::BufWriter::new(out);
        // The declared size may lie: cap what is actually decompressed.
        let copied = std::io::copy(&mut entry.take(max_entry + 1), &mut writer)
            .map_err(|e| Error::io(tmp.path(), e))?;
        if copied > max_entry {
            return Err(too_big());
        }
        let out = writer
            .into_inner()
            .map_err(|e| Error::io(tmp.path(), e.into_error()))?;
        if name == "xray" {
            // Explicit, so a restrictive umask cannot drop the exec bit.
            out.set_permissions(std::fs::Permissions::from_mode(0o755))
                .map_err(|e| Error::io(tmp.path(), e))?;
        }
        out.sync_all().map_err(|e| Error::io(tmp.path(), e))?;
        drop(out);
        staged.push((tmp, dir.join(name)));
    }
    for (tmp, dest) in staged {
        tmp.persist(&dest)?;
    }
    sync_dir(dir);
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
    fn never_downgrades() {
        assert!(is_upgrade("v26.9.9", Some("v26.7.11")));
        assert!(!is_upgrade("v26.7.11", Some("v26.9.9")));
        assert!(!is_upgrade("v26.9.9", Some("v26.9.9")));
        assert!(is_upgrade("v26.9.9", None));
        assert!(is_upgrade("v26.9.9", Some("garbage")));
        assert!(!is_upgrade("garbage", Some("v26.9.9")));
    }

    /// Minimal HTTP server: `/releases` lists one release whose assets point
    /// back here; every other path answers 500. With `hang`, it accepts
    /// connections and never answers.
    async fn fake_github(hang: bool) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let asset = asset_name().unwrap();
        let body = format!(
            r#"[{{"tag_name":"v99.1.1","prerelease":true,"assets":[{{"name":"{asset}","browser_download_url":"{base}/zip"}},{{"name":"{asset}.dgst","browser_download_url":"{base}/dgst"}}]}}]"#
        );
        tokio::spawn(async move {
            let mut held = Vec::new();
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                if hang {
                    held.push(sock);
                    continue;
                }
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let resp = if req.starts_with("GET /releases") {
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    "HTTP/1.1 500 Oops\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into()
                };
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        format!("{base}/releases")
    }

    fn fake_installed_core(dir: &Path, version: &str) {
        for f in ["xray", "geoip.dat", "geosite.dat"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        std::fs::write(dir.join("VERSION"), version).unwrap();
    }

    #[tokio::test]
    async fn failed_upgrade_keeps_working_core() {
        let dir = tempfile::tempdir().unwrap();
        fake_installed_core(dir.path(), "v26.9.9");
        let api = fake_github(false).await;
        let mgr = CoreManager::new(dir.path().to_path_buf(), "test")
            .with_releases_api(&api, Duration::from_secs(5));
        let v = mgr
            .ensure_latest(|_| {})
            .await
            .expect("keeps installed core");
        assert_eq!(v, "v26.9.9");
        assert!(!mgr.core_needs_check(), "retry postponed");

        // Nothing installed: the failure is reported.
        let empty = tempfile::tempdir().unwrap();
        let mgr = CoreManager::new(empty.path().to_path_buf(), "test")
            .with_releases_api(&api, Duration::from_secs(5));
        assert!(mgr.ensure_latest(|_| {}).await.is_err());
    }

    #[tokio::test]
    async fn unresponsive_api_times_out() {
        let dir = tempfile::tempdir().unwrap();
        fake_installed_core(dir.path(), "v26.9.9");
        let api = fake_github(true).await;
        let mgr = CoreManager::new(dir.path().to_path_buf(), "test")
            .with_releases_api(&api, Duration::from_millis(500));
        let started = std::time::Instant::now();
        let v = tokio::time::timeout(Duration::from_secs(10), mgr.ensure_latest(|_| {}))
            .await
            .expect("must not hang")
            .expect("falls back to installed core");
        assert_eq!(v, "v26.9.9");
        assert!(started.elapsed() < Duration::from_secs(5));
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

    #[test]
    fn versions_with_prerelease_suffixes() {
        use std::cmp::Ordering::*;
        assert_eq!(parse_version("v26.7.11-rc1"), Some((26, 7, 11)));
        assert_eq!(parse_version("v26.7.11+build.5"), Some((26, 7, 11)));
        assert_eq!(parse_version("v26.7"), None);
        assert_eq!(parse_version("v26.x.1"), None);
        assert_eq!(compare_versions("v26.7.11-rc1", "v26.7.11"), Some(Less));
        assert_eq!(compare_versions("v26.7.11", "v26.7.11-rc1"), Some(Greater));
        assert_eq!(
            compare_versions("v26.7.11-rc2", "v26.7.11-rc10"),
            Some(Less)
        );
        assert_eq!(
            compare_versions("v26.7.11-alpha", "v26.7.11-beta"),
            Some(Less)
        );
        assert_eq!(
            compare_versions("v26.7.11-rc.1", "v26.7.11-rc.1"),
            Some(Equal)
        );
        assert_eq!(compare_versions("v26.7.11-rc1", "v26.7.10"), Some(Greater));
        assert_eq!(compare_versions("v26.7.11-", "v26.7.11"), Some(Equal));
        assert_eq!(compare_versions("garbage", "v1.0.0"), None);
        // Odd suffixes must never panic.
        for s in ["-", "-ü", "-1.ü2", "-..", "-rc.", "-9999999999999999999999"] {
            let _ = compare_versions(&format!("v1.2.3{s}"), "v1.2.3-rc1");
        }

        assert!(is_upgrade("v26.7.11", Some("v26.7.11-rc1")));
        assert!(!is_upgrade("v26.7.11-rc1", Some("v26.7.11")));
        assert!(is_upgrade("v26.7.11-rc2", Some("v26.7.11-rc1")));
        assert!(is_upgrade("v26.7.12-rc1", Some("v26.7.11")));
        assert!(!supports_tun_routing("v26.7.11-rc1"));
        assert!(supports_tun_routing("v26.7.12-rc1"));
    }

    fn temp_leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp") || n.ends_with(".part"))
            .collect()
    }

    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for (name, body) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(body).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn release_zip() -> Vec<u8> {
        make_zip(&[
            ("xray", b"#!/bin/sh\nexit 0\n"),
            ("geoip.dat", b"new-ip"),
            ("geosite.dat", b"new-site"),
            ("LICENSE", b"x"),
        ])
    }

    /// Serves fixed bodies by path; anything else answers 500.
    async fn file_server(routes: Vec<(&'static str, Vec<u8>)>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes = std::sync::Arc::new(routes);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let routes = routes.clone();
                tokio::spawn(async move {
                    let mut req = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => req.extend_from_slice(&buf[..n]),
                        }
                    }
                    let req = String::from_utf8_lossy(&req);
                    let path = req.split_whitespace().nth(1).unwrap_or("");
                    let body = routes.iter().find(|(p, _)| *p == path).map(|(_, b)| b);
                    let (head, body): (String, &[u8]) = match body {
                        Some(b) => (
                            format!(
                                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                                b.len()
                            ),
                            b,
                        ),
                        None => (
                            "HTTP/1.1 500 Oops\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                                .into(),
                            &[],
                        ),
                    };
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(body).await;
                });
            }
        });
        base
    }

    fn dgst_for(bytes: &[u8]) -> Vec<u8> {
        format!(
            "MD5= 00\nSHA2-256= {}\n",
            hex::encode(Sha256::digest(bytes))
        )
        .into_bytes()
    }

    #[tokio::test]
    async fn install_end_to_end_leaves_no_temp_files() {
        let zip = release_zip();
        let base = file_server(vec![("/dgst", dgst_for(&zip)), ("/zip", zip.clone())]).await;
        let dir = tempfile::tempdir().unwrap();
        let core = dir.path().join("core");
        // Leftovers of an older version are cleaned up.
        std::fs::create_dir_all(&core).unwrap();
        std::fs::write(core.join("download.zip.part"), b"old").unwrap();
        std::fs::write(core.join("xray.tmp"), b"old").unwrap();

        let mgr = CoreManager::new(core.clone(), "test");
        let release = Release {
            version: "v1.2.3-rc1".into(),
            zip_url: format!("{base}/zip"),
            dgst_url: format!("{base}/dgst"),
        };
        let mut seen = Vec::new();
        mgr.install(&release, |p| seen.push(p)).await.unwrap();

        assert!(mgr.is_installed());
        assert_eq!(mgr.installed_version().as_deref(), Some("v1.2.3-rc1"));
        assert_eq!(std::fs::read(core.join("geoip.dat")).unwrap(), b"new-ip");
        assert!(
            temp_leftovers(&core).is_empty(),
            "{:?}",
            temp_leftovers(&core)
        );
        assert!(!core.join("LICENSE").exists());
        assert!(seen.contains(&Progress::Downloading {
            received: zip.len() as u64,
            total: Some(zip.len() as u64)
        }));
        let tail: Vec<_> = seen.iter().rev().take(3).rev().cloned().collect();
        assert_eq!(
            tail,
            vec![
                Progress::Verifying,
                Progress::Extracting,
                Progress::Done {
                    version: "v1.2.3-rc1".into()
                }
            ]
        );
    }

    #[tokio::test]
    async fn failed_installs_keep_the_old_core_and_clean_up() {
        let good = release_zip();
        let partial = make_zip(&[("xray", b"#!/bin/sh\n"), ("geoip.dat", b"new-ip")]);
        let base = file_server(vec![
            ("/bad-dgst", dgst_for(b"something else")),
            ("/good-dgst", dgst_for(&good)),
            ("/good", good),
            ("/partial-dgst", dgst_for(&partial)),
            ("/partial", partial),
        ])
        .await;
        let dir = tempfile::tempdir().unwrap();
        fake_installed_core(dir.path(), "v1.0.0");
        let mgr = CoreManager::new(dir.path().to_path_buf(), "test");
        let rel = |zip: &str, dgst: &str| Release {
            version: "v2.0.0".into(),
            zip_url: format!("{base}/{zip}"),
            dgst_url: format!("{base}/{dgst}"),
        };

        // Checksum mismatch.
        let err = mgr.install(&rel("good", "bad-dgst"), |_| {}).await;
        assert!(matches!(err, Err(Error::Checksum { .. })), "{err:?}");
        // Archive without geosite.dat: nothing is replaced, not even xray.
        let err = mgr.install(&rel("partial", "partial-dgst"), |_| {}).await;
        assert!(matches!(err, Err(Error::Zip(_))), "{err:?}");
        // Download fails halfway (500).
        assert!(
            mgr.install(&rel("missing", "good-dgst"), |_| {})
                .await
                .is_err()
        );

        for f in ["xray", "geoip.dat", "geosite.dat"] {
            assert_eq!(std::fs::read(dir.path().join(f)).unwrap(), b"x", "{f}");
        }
        assert_eq!(mgr.installed_version().as_deref(), Some("v1.0.0"));
        assert!(
            temp_leftovers(dir.path()).is_empty(),
            "{:?}",
            temp_leftovers(dir.path())
        );
    }

    #[test]
    fn extract_ignores_zip_slip_entries() {
        let dir = tempfile::tempdir().unwrap();
        let core = dir.path().join("a/core");
        std::fs::create_dir_all(&core).unwrap();
        let zip_path = dir.path().join("x.zip");
        std::fs::write(
            &zip_path,
            make_zip(&[
                ("../evil", b"pwned"),
                ("../../evil2", b"pwned"),
                ("sub/../../evil3", b"pwned"),
                ("xray", b"bin"),
                ("geoip.dat", b"ip"),
                ("geosite.dat", b"site"),
            ]),
        )
        .unwrap();
        extract(&zip_path, &core).unwrap();
        assert!(!dir.path().join("a/evil").exists());
        assert!(!dir.path().join("evil2").exists());
        assert!(!dir.path().join("a/evil3").exists());
        let names: Vec<_> = std::fs::read_dir(&core)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 3, "{names:?}");
    }

    #[test]
    fn extract_rejects_oversized_entries() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("x.zip");
        std::fs::write(
            &zip_path,
            make_zip(&[
                ("xray", b"bin"),
                ("geoip.dat", &[7u8; 4096]),
                ("geosite.dat", b"site"),
            ]),
        )
        .unwrap();
        let core = dir.path().join("core");
        std::fs::create_dir_all(&core).unwrap();
        let err = extract_limited(&zip_path, &core, 1024).unwrap_err();
        assert!(err.to_string().contains("larger"), "{err}");
        assert!(!core.join("xray").exists(), "nothing replaced");
        assert!(temp_leftovers(&core).is_empty());
        // Corrupt archive: clean error, no panic.
        std::fs::write(&zip_path, b"PK\x03\x04garbage").unwrap();
        assert!(extract(&zip_path, &core).is_err());
    }

    #[tokio::test]
    async fn geo_failure_backs_off_and_success_clears_it() {
        use std::fs::FileTimes;
        let dir = tempfile::tempdir().unwrap();
        let broken = file_server(vec![]).await;
        let mgr = CoreManager::new(dir.path().to_path_buf(), "test")
            .with_geo_base(&broken)
            .with_releases_api(&broken, Duration::from_secs(5));
        assert!(mgr.geo_needs_update());
        assert!(mgr.update_geo().await.is_err());
        assert!(!mgr.geo_needs_update(), "retry postponed after failure");

        // An hour later it is due again.
        let retry = dir.path().join(GEO_RETRY_STAMP);
        let old = SystemTime::now() - GEO_RETRY_INTERVAL - Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&retry)
            .unwrap()
            .set_times(FileTimes::new().set_modified(old))
            .unwrap();
        assert!(mgr.geo_needs_update());

        let ip = b"geoip-bytes".to_vec();
        let site = b"geosite-bytes".to_vec();
        let sum =
            |b: &[u8], n: &str| format!("{}  {n}\n", hex::encode(Sha256::digest(b))).into_bytes();
        let good = file_server(vec![
            ("/geoip.dat.sha256sum", sum(&ip, "geoip.dat")),
            ("/geoip.dat", ip.clone()),
            ("/geosite.dat.sha256sum", sum(&site, "geosite.dat")),
            ("/geosite.dat", site.clone()),
        ])
        .await;
        let mgr = mgr.with_geo_base(&good);
        mgr.update_geo().await.unwrap();
        assert!(!retry.exists());
        assert!(!mgr.geo_needs_update());
        assert_eq!(std::fs::read(dir.path().join("geoip.dat")).unwrap(), ip);
        assert_eq!(std::fs::read(dir.path().join("geosite.dat")).unwrap(), site);
        assert!(temp_leftovers(dir.path()).is_empty());
    }

    #[test]
    fn stale_temp_files_are_removed_fresh_foreign_ones_kept() {
        use std::fs::FileTimes;
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let old = SystemTime::now() - STALE_TEMP_AGE - Duration::from_secs(60);
        let age = |p: &Path| {
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_times(FileTimes::new().set_modified(old))
                .unwrap();
        };
        for n in [
            "download.zip.part",
            "geosite.dat.tmp",
            ".xray.4000000.1.tmp",
            ".geoip.dat.4000001.0.tmp",
            ".other.4000000.1.tmp",
        ] {
            std::fs::write(d.join(n), b"").unwrap();
        }
        age(&d.join(".xray.4000000.1.tmp"));
        age(&d.join(".other.4000000.1.tmp"));
        remove_legacy_temps(d);
        let mut left = temp_leftovers(d);
        left.sort();
        // Fresh temp of another (possibly running) install stays; files
        // that are not ours stay.
        assert_eq!(
            left,
            vec![".geoip.dat.4000001.0.tmp", ".other.4000000.1.tmp"]
        );
    }
}

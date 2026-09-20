use std::path::PathBuf;

/// Errors produced by obscure-core. The GUI maps these to human messages.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unsupported link scheme `{0}`")]
    UnsupportedScheme(String),
    #[error("malformed link: {0}")]
    MalformedLink(String),
    #[error("i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("no Xray release asset for architecture `{0}`")]
    NoAssetForArch(String),
    #[error("checksum mismatch for {file}: expected {expected}, got {actual}")]
    Checksum {
        file: String,
        expected: String,
        actual: String,
    },
    #[error("archive error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("Xray core is not installed")]
    CoreNotInstalled,
    #[error("Xray rejected the configuration:\n{0}")]
    ConfigRejected(String),
    #[error("Xray exited unexpectedly ({status}):\n{output}")]
    CoreExited { status: String, output: String },
    #[error("Xray did not start listening on {0} in time")]
    StartTimeout(String),
    #[error("system proxy error: {0}")]
    SysProxy(String),
    #[error("d-bus error: {0}")]
    DBus(#[from] zbus::Error),
    #[error("stats error: {0}")]
    Stats(String),
    #[error("latency test failed: {0}")]
    Latency(String),
    #[error("subscription error: {0}")]
    Subscription(String),
    #[error("the subscription server returned a Clash/YAML profile instead of share links")]
    ClashYaml,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

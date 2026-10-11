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
    /// Built through `From<reqwest::Error>`, which strips the URL: subscription
    /// URLs carry access tokens and must not end up in messages or logs.
    #[error("network error: {0}")]
    Http(reqwest::Error),
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
    #[error("tunnel mode: {0}")]
    Tun(String),
    #[error("the permission request was dismissed")]
    TunDismissed,
    #[error("the custom configuration is not valid JSON: {0}")]
    InvalidOverride(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e.without_url())
    }
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn http_errors_do_not_leak_the_url() {
        crate::init_crypto();
        // Nothing listens on port 1; the request fails with a connect error.
        let err = reqwest::Client::new()
            .get("http://127.0.0.1:1/sub?token=s3cr3t")
            .send()
            .await
            .unwrap_err();
        assert!(err.url().is_some());
        let err: Error = err.into();
        let Error::Http(inner) = &err else {
            panic!("unexpected {err:?}");
        };
        assert!(inner.url().is_none());
        assert!(!err.to_string().contains("s3cr3t"), "{err}");
        assert!(!format!("{err:?}").contains("s3cr3t"), "{err:?}");
    }
}

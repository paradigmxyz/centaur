//! The one outbound HTTP client: proxies from `http_proxy`/`https_proxy`/
//! `no_proxy` (either case), TLS roots from `SSL_CERT_FILE` when set (only
//! that bundle is trusted) or the operating system's store, redirects off,
//! 10 s overall timeout. Area code uses `state.http`; it never builds its
//! own client. Use [`without_url`] on errors before logging or returning
//! them, since a URL may carry credentials or authorization codes.

use std::time::Duration;

use crate::env::Env;

pub const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("SSL_CERT_FILE {path}: {reason}")]
    CaFile { path: String, reason: String },
    #[error("invalid proxy setting")]
    Proxy,
    #[error("HTTP client: {0}")]
    Build(String),
}

/// Builds the client from an environment snapshot.
pub fn client(env: &Env) -> Result<reqwest::Client, ClientError> {
    let setting = |lower: &str, upper: &str| {
        env.var_present(lower)
            .or_else(|| env.var_present(upper))
            .map(str::to_owned)
    };
    let no_proxy = setting("no_proxy", "NO_PROXY")
        .as_deref()
        .and_then(reqwest::NoProxy::from_string);
    let mut builder = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy();
    if let Some(url) = setting("http_proxy", "HTTP_PROXY") {
        let proxy = reqwest::Proxy::http(url).map_err(|_| ClientError::Proxy)?;
        builder = builder.proxy(proxy.no_proxy(no_proxy.clone()));
    }
    if let Some(url) = setting("https_proxy", "HTTPS_PROXY") {
        let proxy = reqwest::Proxy::https(url).map_err(|_| ClientError::Proxy)?;
        builder = builder.proxy(proxy.no_proxy(no_proxy.clone()));
    }
    let roots = match env.var_present("SSL_CERT_FILE") {
        Some(path) => {
            let error = |reason: String| ClientError::CaFile {
                path: path.to_owned(),
                reason,
            };
            let pem = std::fs::read(path).map_err(|e| error(e.to_string()))?;
            let certs =
                reqwest::Certificate::from_pem_bundle(&pem).map_err(|e| error(e.to_string()))?;
            if certs.is_empty() {
                return Err(error("no certificates".into()));
            }
            certs
        }
        None => rustls_native_certs::load_native_certs()
            .certs
            .iter()
            .filter_map(|cert| reqwest::Certificate::from_der(cert.as_ref()).ok())
            .collect(),
    };
    builder
        .tls_certs_only(roots)
        .build()
        .map_err(|e| ClientError::Build(e.without_url().to_string()))
}

/// The error without its URL.
pub fn without_url(error: reqwest::Error) -> reqwest::Error {
    error.without_url()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_with_proxy_settings_and_rejects_a_missing_bundle() {
        let env = Env::from_pairs([
            ("HTTPS_PROXY", "http://proxy.internal:3128"),
            ("no_proxy", "localhost,.internal"),
        ]);
        assert!(client(&env).is_ok());
        let env = Env::from_pairs([("SSL_CERT_FILE", "/nonexistent/bundle.pem")]);
        assert!(matches!(client(&env), Err(ClientError::CaFile { .. })));
    }
}

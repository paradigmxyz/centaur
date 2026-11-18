//! Host allowlist: requests whose `Host` is not one the console is known by
//! are refused with 403 (DNS-rebinding protection). `/up` is exempt because
//! kubelet probes by pod IP.

use std::collections::BTreeSet;

use axum::{
    extract::{Request, State},
    http::header,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::{error::ApiError, state::Config};

#[derive(Clone, Debug)]
pub struct HostAllowlist(BTreeSet<String>);

/// Lower-cased host of `value`, which may be a bare host, `host:port`, or a
/// URL. Ports are dropped.
pub fn normalize(value: &str) -> Option<String> {
    let value = value.trim();
    let value = value.split_once("://").map_or(value, |(_, rest)| rest);
    let authority = value.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

impl HostAllowlist {
    /// The public URL's host, the in-cluster service host (`..._URL`),
    /// `..._ALLOWED_HOSTS` entries (comma or whitespace separated), and
    /// localhost addresses in debug builds.
    pub fn from_config(config: &Config) -> Self {
        let mut hosts = BTreeSet::new();
        let urls = [config.public_url.as_deref(), config.env.present("URL")];
        hosts.extend(urls.into_iter().flatten().filter_map(normalize));
        if let Some(list) = config.env.get("ALLOWED_HOSTS") {
            hosts.extend(
                list.split(|c: char| c == ',' || c.is_whitespace())
                    .filter_map(normalize),
            );
        }
        if cfg!(debug_assertions) {
            hosts.extend(["localhost", "127.0.0.1", "::1"].map(str::to_owned));
        }
        Self(hosts)
    }

    pub fn allows(&self, host: &str) -> bool {
        normalize(host).is_some_and(|host| self.0.contains(&host))
    }
}

pub async fn guard(State(allowed): State<HostAllowlist>, request: Request, next: Next) -> Response {
    if request.uri().path() == "/up" {
        return next.run(request).await;
    }
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .or_else(|| request.uri().authority().map(|a| a.as_str()));
    if host.is_some_and(|host| allowed.allows(host)) {
        next.run(request).await
    } else {
        ApiError::Forbidden("host not allowed".into()).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_normalized() {
        assert_eq!(
            normalize("Console.Example.com:8443").as_deref(),
            Some("console.example.com")
        );
        assert_eq!(normalize("https://a.b/x?y").as_deref(), Some("a.b"));
        assert_eq!(normalize("[::1]:3000").as_deref(), Some("::1"));
        assert_eq!(normalize(" ").as_deref(), None);
    }
}

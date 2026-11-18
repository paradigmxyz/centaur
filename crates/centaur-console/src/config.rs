//! Process configuration, from `CENTAUR_CONSOLE_*` / `IRON_CONTROL_*`
//! variables (see [`Env`]) and plain environment variables.
//!
//! | Variable | Used for |
//! | --- | --- |
//! | `SECRET_KEY_BASE` | session cookie, CSRF, signed state tokens (required) |
//! | `CENTAUR_CONSOLE_AR_ENCRYPTION_{PRIMARY_KEY,DETERMINISTIC_KEY,KEY_DERIVATION_SALT}` | encrypted attributes (required) |
//! | `CENTAUR_CONSOLE_DATABASE_URL`, `_DB_HOST`, `_DB_PORT`, `_DATABASE_PASSWORD`, `RAILS_MAX_THREADS` | the console database (`database.yml` production) |
//! | `CENTAUR_DATABASE_URL` (either form), `CENTAUR_DATABASE_NAME` | read-only api-rs database |
//! | `CENTAUR_CONSOLE_PUBLIC_URL` | public origin, Secure cookies |
//! | `CENTAUR_JWT_SIGNING_SECRET`, `CENTAUR_SANDBOX_ENTITLEMENTS_JWT_{AUDIENCE,ISSUER}` | JWTs |
//! | `CENTAUR_CONSOLE_INITIAL_USER_{EMAIL,PASSWORD}`, `_INITIAL_API_KEY` | bootstrap |
//! | `PORT` | listen port (3000) |
//! | `CENTAUR_CONSOLE_PUBLIC_DIR`, `_SPA_SHELL` | `public/` and `spa-shell/index.html` (Rust-only; default: the image layout) |
//! | `http_proxy`/`HTTP_PROXY`, `https_proxy`/`HTTPS_PROXY`, `no_proxy`/`NO_PROXY`, `SSL_CERT_FILE` | outbound HTTP |
//!
//! Area settings (SSO providers, `PASSWORD_LOGIN_ENABLED`,
//! `SSO_EMAIL_DOMAINS`, `BOOTSTRAP_ADMINS`, `API_CLIENTS_ENABLED`,
//! `MCP_PUBLIC_URL`, `MCP_ACCESS_TOKEN_TTL_SECONDS`, `CENTAUR_API_URL`,
//! `CENTAUR_API_SERVER_PROXY_HOSTS`, Slack, ...) are read by their areas
//! from `Config::env`.

use std::{path::PathBuf, time::Duration};

use centaur_console_api::env::Env;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// A configuration error that stops the process at boot.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

/// `database.yml`'s production primary database.
pub fn primary_database(env: &Env) -> Result<PgConnectOptions, ConfigError> {
    let mut options = PgConnectOptions::new()
        .host(env.fetch("DB_HOST", "localhost"))
        .port(
            env.fetch("DB_PORT", "5432")
                .parse()
                .map_err(|_| ConfigError(format!("{} is not a port", Env::key("DB_PORT"))))?,
        )
        .username("iron_control")
        .database("iron_control_production");
    if let Some(password) = env.get("DATABASE_PASSWORD") {
        options = options.password(password);
    }
    // A URL's components override the keys above (Rails' UrlConfig merge).
    if let Some(url) = env.get("DATABASE_URL").filter(|url| !url.is_empty()) {
        options = overlay_url(options, url)?;
    }
    Ok(options)
}

fn overlay_url(mut options: PgConnectOptions, url: &str) -> Result<PgConnectOptions, ConfigError> {
    let parsed = url::Url::parse(url).map_err(|_| ConfigError("invalid database URL".into()))?;
    if let Some(host) = parsed.host_str().filter(|host| !host.is_empty()) {
        options = options.host(host);
    }
    if let Some(port) = parsed.port() {
        options = options.port(port);
    }
    if !parsed.username().is_empty() {
        options = options.username(&percent_decode(parsed.username()));
    }
    if let Some(password) = parsed.password() {
        options = options.password(&percent_decode(password));
    }
    let database = parsed.path().trim_start_matches('/');
    if !database.is_empty() {
        options = options.database(&percent_decode(database));
    }
    for (key, value) in parsed.query_pairs() {
        if key == "sslmode" {
            options = options.ssl_mode(
                value
                    .parse()
                    .map_err(|_| ConfigError("invalid sslmode".into()))?,
            );
        }
    }
    Ok(options)
}

/// URI-component decoding: `%XX` escapes only; a literal `+` stays.
fn percent_decode(value: &str) -> String {
    urlencoding::decode(value).map_or_else(|_| value.to_owned(), std::borrow::Cow::into_owned)
}

/// `CentaurSessionRecord`: the read-only api-rs database.
pub fn api_database(env: &Env) -> Result<PgConnectOptions, ConfigError> {
    if let Some(url) = env
        .present("CENTAUR_DATABASE_URL")
        .or_else(|| env.var_present("CENTAUR_DATABASE_URL"))
    {
        return url
            .parse()
            .map_err(|_| ConfigError("invalid CENTAUR_DATABASE_URL".into()));
    }
    let name = env
        .present("CENTAUR_DATABASE_NAME")
        .or_else(|| env.var_present("CENTAUR_DATABASE_NAME"))
        .unwrap_or("ai_v2");
    Ok(primary_database(env)?.database(name))
}

/// `pool: RAILS_MAX_THREADS || 5`.
pub fn pool_size(env: &Env) -> u32 {
    env.var("RAILS_MAX_THREADS")
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(5)
}

pub fn pool_options(env: &Env) -> PgPoolOptions {
    PgPoolOptions::new()
        // Two is the floor: a worker pass holds its advisory-lock transaction
        // on one connection while its own queries use another.
        .max_connections(pool_size(env).max(2))
        .acquire_timeout(Duration::from_secs(5))
}

/// The Active Record encryption keys; all three are required.
pub struct EncryptionKeys {
    pub primary_key: String,
    pub key_derivation_salt: String,
}

pub fn encryption_keys(env: &Env) -> Result<EncryptionKeys, ConfigError> {
    let names = [
        "AR_ENCRYPTION_PRIMARY_KEY",
        "AR_ENCRYPTION_DETERMINISTIC_KEY",
        "AR_ENCRYPTION_KEY_DERIVATION_SALT",
    ];
    let missing: Vec<String> = names
        .iter()
        .filter(|name| env.get(name).is_none_or(|value| value.trim().is_empty()))
        .map(|name| Env::key(name))
        .collect();
    if !missing.is_empty() {
        return Err(ConfigError(format!(
            "ActiveRecord encryption is not configured: missing env vars {}",
            missing.join(", ")
        )));
    }
    Ok(EncryptionKeys {
        primary_key: env
            .get("AR_ENCRYPTION_PRIMARY_KEY")
            .expect("checked")
            .to_owned(),
        key_derivation_salt: env
            .get("AR_ENCRYPTION_KEY_DERIVATION_SALT")
            .expect("checked")
            .to_owned(),
    })
}

/// `SECRET_KEY_BASE`, required in production.
pub fn secret_key_base(env: &Env) -> Result<String, ConfigError> {
    env.var("SECRET_KEY_BASE")
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ConfigError("Missing `secret_key_base` for 'production' environment, set this string with `SECRET_KEY_BASE`".into()))
}

/// Where `public/` and the SPA shell live: the image layout
/// (`/usr/share/centaur-console/public`, `/usr/share/centaur-console/spa-shell/index.html`) unless overridden.
pub fn asset_paths(env: &Env) -> (PathBuf, PathBuf) {
    let public_dir = env
        .present("PUBLIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/share/centaur-console/public"));
    let spa_index = env
        .present("SPA_SHELL")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/share/centaur-console/spa-shell/index.html"));
    (public_dir, spa_index)
}

/// `PORT` (3000).
pub fn port(env: &Env) -> Result<u16, ConfigError> {
    env.var("PORT")
        .filter(|value| !value.trim().is_empty())
        .map_or(Ok(3000), |value| {
            value
                .trim()
                .parse()
                .map_err(|_| ConfigError(format!("PORT is not a port: {value}")))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_primary_database_follows_database_yml() {
        let env = Env::from_pairs([
            ("CENTAUR_CONSOLE_DB_HOST", "db"),
            ("IRON_CONTROL_DB_PORT", "6543"),
            ("CENTAUR_CONSOLE_DATABASE_PASSWORD", "pw"),
        ]);
        let options = primary_database(&env).unwrap();
        assert_eq!(options.get_host(), "db");
        assert_eq!(options.get_port(), 6543);
        assert_eq!(options.get_username(), "iron_control");
        assert_eq!(options.get_database(), Some("iron_control_production"));

        // URL parts override; absent parts keep the hash keys.
        let env = Env::from_pairs([
            ("CENTAUR_CONSOLE_DB_PORT", "6543"),
            ("CENTAUR_CONSOLE_DATABASE_URL", "postgres://h2"),
        ]);
        let options = primary_database(&env).unwrap();
        assert_eq!(options.get_host(), "h2");
        assert_eq!(options.get_port(), 6543);
        assert_eq!(options.get_database(), Some("iron_control_production"));
        let env = Env::from_pairs([("CENTAUR_CONSOLE_DATABASE_URL", "postgres://u:p@h:1/x")]);
        let options = primary_database(&env).unwrap();
        assert_eq!(
            (
                options.get_host(),
                options.get_port(),
                options.get_username(),
                options.get_database()
            ),
            ("h", 1, "u", Some("x"))
        );
    }

    #[test]
    fn the_api_database_defaults_to_ai_v2_on_the_console_server() {
        let env = Env::from_pairs([("CENTAUR_CONSOLE_DB_HOST", "db")]);
        let options = api_database(&env).unwrap();
        assert_eq!(
            (options.get_host(), options.get_database()),
            ("db", Some("ai_v2"))
        );
        let env = Env::from_pairs([("CENTAUR_DATABASE_NAME", "sessions")]);
        assert_eq!(api_database(&env).unwrap().get_database(), Some("sessions"));
        let env = Env::from_pairs([("CENTAUR_DATABASE_URL", "postgres://api/ai")]);
        assert_eq!(api_database(&env).unwrap().get_host(), "api");
    }

    #[test]
    fn encryption_keys_are_all_required() {
        let error = encryption_keys(&Env::from_pairs([(
            "CENTAUR_CONSOLE_AR_ENCRYPTION_PRIMARY_KEY",
            "k",
        )]))
        .err()
        .unwrap();
        assert_eq!(
            error.0,
            "ActiveRecord encryption is not configured: missing env vars \
             CENTAUR_CONSOLE_AR_ENCRYPTION_DETERMINISTIC_KEY, CENTAUR_CONSOLE_AR_ENCRYPTION_KEY_DERIVATION_SALT"
        );
    }

    #[test]
    fn database_url_components_keep_literal_plus_signs() {
        let env = Env::from_pairs([(
            "CENTAUR_CONSOLE_DATABASE_URL",
            "postgres://us%40er:p%2Bss+w%3Ard@h:1/d+b%2Fx",
        )]);
        let options = primary_database(&env).unwrap();
        assert_eq!(options.get_username(), "us@er");
        assert_eq!(options.get_database(), Some("d+b/x"));
        assert_eq!(percent_decode("p%2Bss+w%3Ard"), "p+ss+w:rd");
        assert_eq!(percent_decode("100%"), "100%");
    }
}

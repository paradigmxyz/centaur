use std::{net::SocketAddr, time::Duration};

use clap::Parser;

pub const PDF_MIME_TYPE: &str = "application/pdf";
pub const GOOGLE_DOC_MIME_TYPE: &str = "application/vnd.google-apps.document";
pub const GOOGLE_DOC_EXPORT_MIME_TYPE: &str = "text/plain";
pub const FOLDER_MIME_TYPE: &str = "application/vnd.google-apps.folder";
pub const QUEUE_NAME: &str = "company_context";
pub const DRIVE_SCAN_TASK: &str = "drive.user.scan";
pub const SHARED_DRIVES_DISCOVER_TASK: &str = "drive.shared_drives.discover";
pub const SHARED_DRIVE_SCAN_TASK: &str = "drive.shared_drive.scan";
pub const SHARED_FOLDERS_BATCH_TASK: &str = "drive.shared_folders.batch";
pub const DRIVE_CREDENTIALS_RECONCILE_TASK: &str = "drive.credentials.reconcile";
// Keep the original task name so durable PDF jobs queued by older versions remain runnable.
pub const DOCUMENT_EXTRACT_TASK: &str = "drive.pdf.extract";
pub const DOCUMENT_EMBED_TASK: &str = "drive.document.embed";
pub const DOCUMENT_DELETE_TASK: &str = "drive.document.delete";
pub const GRANOLA_CREDENTIALS_RECONCILE_TASK: &str = "granola.credentials.reconcile";
pub const GRANOLA_SYNC_TASK: &str = "granola.user.sync";
pub const GRANOLA_NOTES_FETCH_TASK: &str = "granola.notes.fetch";
pub const GRANOLA_NOTE_EMBED_TASK: &str = "granola.note.embed";
/// Slack tasks wait on the Slack app's shared rate limits, so they run on their
/// own queue and worker instead of delaying Drive and Granola work.
pub const SLACK_QUEUE_NAME: &str = "company_context_slack";
pub const SLACK_CREDENTIALS_RECONCILE_TASK: &str = "slack.credentials.reconcile";
pub const SLACK_USER_DISCOVER_TASK: &str = "slack.user.discover";
pub const SLACK_CONVERSATION_SYNC_TASK: &str = "slack.conversation.sync";
pub const SLACK_THREAD_SYNC_TASK: &str = "slack.thread.sync";
pub const SLACK_USERS_SYNC_TASK: &str = "slack.team.users.sync";
/// Projection does not call Slack, so it runs on the main queue, which also
/// holds the embeddings client.
pub const SLACK_CONVERSATION_PROJECT_TASK: &str = "slack.conversation.project";
pub const SLACK_CHANNEL_DAY_EMBED_TASK: &str = "slack.channel_day.embed";
/// File downloads are not paced against Slack's Web API limits, so file tasks
/// run on the main queue too.
pub const SLACK_FILE_EXTRACT_TASK: &str = "slack.file.extract";
pub const SLACK_FILE_EMBED_TASK: &str = "slack.file.embed";
/// Document ID prefixes, which identify each published document's type.
pub const GOOGLE_DRIVE_DOCUMENT_ID_PREFIX: &str = "google-drive:";
pub const SLACK_DOCUMENT_ID_PREFIX: &str = "slack:";
pub const SLACK_FILE_DOCUMENT_ID_PREFIX: &str = "slack-file:";
pub const GRANOLA_DOCUMENT_ID_PREFIX: &str = "granola:";
/// Slack conversation types that can be synchronized.
pub const SLACK_CONVERSATION_TYPES: [&str; 3] = ["public_channel", "private_channel", "im"];

#[derive(Clone, Debug, Parser)]
#[command(
    name = "centaur-company-context",
    about = "Ingest company context from Google Drive, Granola, and Slack"
)]
pub struct Config {
    #[arg(long, env = "DATABASE_URL", value_parser = nonempty)]
    pub database_url: String,
    #[arg(long, env = "IRON_CONTROL_DATABASE_URL", value_parser = nonempty)]
    pub console_database_url: String,
    #[arg(long, env = "IRON_CONTROL_DATABASE_NAME", value_parser = nonempty)]
    pub console_database_name: Option<String>,
    #[arg(
        long,
        env = "IRON_CONTROL_AR_ENCRYPTION_PRIMARY_KEY",
        value_parser = nonempty,
        hide_env_values = true
    )]
    pub active_record_primary_key: String,
    #[arg(
        long,
        env = "IRON_CONTROL_AR_ENCRYPTION_KEY_DERIVATION_SALT",
        value_parser = nonempty,
        hide_env_values = true
    )]
    pub active_record_key_derivation_salt: String,
    #[arg(
        long,
        env = "COMPANY_CONTEXT_GOOGLE_OAUTH_APP_SLUG",
        default_value = "google",
        value_parser = nonempty
    )]
    pub google_oauth_app_slug: String,
    #[arg(
        long,
        env = "COMPANY_CONTEXT_GRANOLA_OAUTH_APP_SLUG",
        default_value = "granola",
        value_parser = nonempty
    )]
    pub granola_oauth_app_slug: String,
    /// Rails Console OAuth app whose per-user Slack credentials are synchronized.
    /// Keep it aligned with the app the Console Slack DM sync uses.
    #[arg(
        long,
        env = "COMPANY_CONTEXT_SLACK_OAUTH_APP_SLUG",
        default_value = "slack",
        value_parser = nonempty
    )]
    pub slack_oauth_app_slug: String,
    /// Limits Google Drive sync to these credential emails. Unset syncs every user.
    #[arg(
        long,
        env = "COMPANY_CONTEXT_GOOGLE_DRIVE_USER_EMAILS",
        value_delimiter = ',',
        value_parser = email
    )]
    pub google_drive_user_emails: Vec<String>,
    /// Limits Granola sync to these credential emails. Unset syncs every user.
    #[arg(
        long,
        env = "COMPANY_CONTEXT_GRANOLA_USER_EMAILS",
        value_delimiter = ',',
        value_parser = email
    )]
    pub granola_user_emails: Vec<String>,
    /// Limits Slack sync to these Slack user IDs. Unset syncs every user.
    #[arg(
        long,
        env = "COMPANY_CONTEXT_SLACK_USER_IDS",
        value_delimiter = ',',
        value_parser = nonempty
    )]
    pub slack_user_ids: Vec<String>,
    /// Limits Slack sync to these conversation IDs. Unset syncs every
    /// conversation of an allowed type.
    #[arg(
        long,
        env = "COMPANY_CONTEXT_SLACK_CHANNEL_IDS",
        value_delimiter = ',',
        value_parser = nonempty
    )]
    pub slack_channel_ids: Vec<String>,
    /// Slack conversation types to sync.
    #[arg(
        long,
        env = "COMPANY_CONTEXT_SLACK_CONVERSATION_TYPES",
        value_delimiter = ',',
        default_value = "public_channel,private_channel",
        value_parser = slack_conversation_type
    )]
    pub slack_conversation_types: Vec<String>,
    #[arg(
        long,
        env = "OPENAI_API_KEY",
        value_parser = nonempty,
        hide_env_values = true
    )]
    pub openai_api_key: String,
    /// Bot token of the Slack app, used to list workspace users so that
    /// documents show names.
    #[arg(
        long,
        env = "SLACK_BOT_TOKEN",
        value_parser = nonempty,
        hide_env_values = true
    )]
    pub slack_bot_token: String,
    /// Secret the Console signs principal API JWTs with. `POST /query`
    /// accepts the same tokens as api-rs.
    #[arg(
        long,
        env = "CENTAUR_JWT_SIGNING_SECRET",
        value_parser = nonempty,
        hide_env_values = true
    )]
    pub jwt_signing_secret: String,
    #[arg(
        long,
        env = "CENTAUR_API_JWT_AUDIENCE",
        default_value = "centaur-api",
        value_parser = nonempty
    )]
    pub jwt_audience: String,
    #[arg(
        long,
        env = "CENTAUR_API_JWT_ISSUER",
        default_value = "centaur-console",
        value_parser = nonempty
    )]
    pub jwt_issuer: String,
    #[arg(long, env = "BIND_ADDR", default_value = "0.0.0.0:8080")]
    pub bind_addr: SocketAddr,
    #[arg(
        long,
        env = "GOOGLE_DRIVE_API_BASE_URL",
        default_value = "https://www.googleapis.com/drive/v3",
        value_parser = normalized_base_url
    )]
    pub google_api_base_url: String,
    #[arg(
        long,
        env = "GRANOLA_MCP_URL",
        default_value = "https://mcp.granola.ai/mcp",
        value_parser = nonempty
    )]
    pub granola_mcp_url: String,
    #[arg(
        long,
        env = "SLACK_API_BASE_URL",
        default_value = "https://slack.com/api",
        value_parser = normalized_base_url
    )]
    pub slack_api_base_url: String,
    /// The only origin Slack file downloads, which carry user tokens, go to.
    #[arg(
        long,
        env = "SLACK_FILES_BASE_URL",
        default_value = "https://files.slack.com",
        value_parser = normalized_base_url
    )]
    pub slack_files_base_url: String,
    #[arg(
        long,
        env = "OPENAI_BASE_URL",
        default_value = "https://api.openai.com/v1",
        value_parser = normalized_base_url
    )]
    pub openai_base_url: String,
    #[arg(
        long,
        env = "COMPANY_CONTEXT_EMBEDDINGS_MODEL",
        default_value = "text-embedding-3-small",
        value_parser = nonempty
    )]
    pub embeddings_model: String,
    #[arg(
        long,
        env = "COMPANY_CONTEXT_EMBEDDINGS_DIMENSIONS",
        default_value = "1536",
        value_parser = embeddings_dimensions
    )]
    pub embeddings_dimensions: usize,
    #[arg(
        long = "scan-interval-seconds",
        env = "COMPANY_CONTEXT_SCAN_INTERVAL_SECONDS",
        default_value = "300",
        value_parser = positive_duration
    )]
    pub scan_interval: Duration,
    #[arg(
        long = "granola-sync-interval-seconds",
        env = "COMPANY_CONTEXT_GRANOLA_SYNC_INTERVAL_SECONDS",
        default_value = "1800",
        value_parser = positive_duration
    )]
    pub granola_sync_interval: Duration,
    /// Days of meetings listed for a Granola account without a checkpoint.
    #[arg(
        long,
        env = "COMPANY_CONTEXT_GRANOLA_INITIAL_LOOKBACK_DAYS",
        default_value = "365",
        value_parser = positive_usize
    )]
    pub granola_initial_lookback_days: usize,
    #[arg(
        long = "slack-discovery-interval-seconds",
        env = "COMPANY_CONTEXT_SLACK_DISCOVERY_INTERVAL_SECONDS",
        default_value = "1800",
        value_parser = positive_duration
    )]
    pub slack_discovery_interval: Duration,
    /// Days of Slack message history to synchronize.
    #[arg(
        long,
        env = "COMPANY_CONTEXT_SLACK_HISTORY_DAYS",
        default_value = "90",
        value_parser = history_days
    )]
    pub slack_history_days: usize,
    /// Per-conversation overrides of the history days, as `ID=DAYS` pairs.
    #[arg(
        long,
        env = "COMPANY_CONTEXT_SLACK_CHANNEL_HISTORY_DAYS",
        value_delimiter = ',',
        value_parser = channel_history_days
    )]
    pub slack_channel_history_days: Vec<(String, usize)>,
    /// Fraction of each Slack method's documented rate limit that ingestion
    /// may use. Other services calling Slack as the same app share the rest.
    #[arg(
        long,
        env = "COMPANY_CONTEXT_SLACK_RATE_LIMIT_SHARE",
        default_value = "0.3",
        value_parser = rate_limit_share
    )]
    pub slack_rate_limit_share: f64,
    #[arg(
        long,
        env = "COMPANY_CONTEXT_SLACK_WORKER_CONCURRENCY",
        default_value = "4",
        value_parser = positive_usize
    )]
    pub slack_worker_concurrency: usize,
    #[arg(
        long = "drive-page-size",
        env = "COMPANY_CONTEXT_DRIVE_PAGE_SIZE",
        default_value = "100",
        value_parser = drive_page_size
    )]
    pub scan_page_size: u16,
    #[arg(
        long,
        env = "COMPANY_CONTEXT_MAX_SCAN_PAGES",
        default_value = "10",
        value_parser = positive_usize
    )]
    pub max_scan_pages: usize,
    /// Folders listed per Drive search while walking shared folders.
    #[arg(
        long,
        env = "COMPANY_CONTEXT_FOLDER_WALK_BATCH_SIZE",
        default_value = "50",
        value_parser = folder_walk_batch_size
    )]
    pub folder_walk_batch_size: usize,
    /// Largest Drive PDF or Slack file downloaded.
    #[arg(
        long,
        env = "COMPANY_CONTEXT_MAX_PDF_BYTES",
        default_value = "26214400",
        value_parser = positive_usize
    )]
    pub max_pdf_bytes: usize,
    #[arg(
        long,
        env = "COMPANY_CONTEXT_MAX_EXTRACTED_BYTES",
        default_value = "52428800",
        value_parser = positive_usize
    )]
    pub max_extracted_bytes: usize,
    #[arg(
        long = "extraction-timeout-seconds",
        env = "COMPANY_CONTEXT_EXTRACTION_TIMEOUT_SECONDS",
        default_value = "120",
        value_parser = positive_duration
    )]
    pub extraction_timeout: Duration,
    #[arg(
        long,
        env = "COMPANY_CONTEXT_CHUNK_CHARS",
        default_value = "6000",
        value_parser = positive_usize
    )]
    pub chunk_chars: usize,
    #[arg(
        long,
        env = "COMPANY_CONTEXT_WORKER_CONCURRENCY",
        default_value = "4",
        value_parser = positive_usize
    )]
    pub worker_concurrency: usize,
}

impl Config {
    pub fn from_args() -> Self {
        Self::parse()
    }
}

fn nonempty(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("value must not be empty".to_owned());
    }
    Ok(value.to_owned())
}

fn email(value: &str) -> Result<String, String> {
    Ok(nonempty(value)?.to_lowercase())
}

fn slack_conversation_type(value: &str) -> Result<String, String> {
    let value = value.trim();
    if !SLACK_CONVERSATION_TYPES.contains(&value) {
        return Err(format!(
            "value must be one of {}",
            SLACK_CONVERSATION_TYPES.join(", ")
        ));
    }
    Ok(value.to_owned())
}

fn channel_history_days(value: &str) -> Result<(String, usize), String> {
    let (channel_id, days) = value
        .split_once('=')
        .ok_or_else(|| "value must be CONVERSATION_ID=DAYS".to_owned())?;
    Ok((nonempty(channel_id)?, history_days(days.trim())?))
}

fn history_days(value: &str) -> Result<usize, String> {
    let value = positive_usize(value)?;
    if value > 36_500 {
        return Err("value must not exceed 36500".to_owned());
    }
    Ok(value)
}

fn normalized_base_url(value: &str) -> Result<String, String> {
    let value = nonempty(value)?;
    Ok(value.trim_end_matches('/').to_owned())
}

fn positive_usize(value: &str) -> Result<usize, String> {
    let value = value
        .parse::<usize>()
        .map_err(|_| "value must be a positive integer".to_owned())?;
    if value == 0 {
        return Err("value must be greater than zero".to_owned());
    }
    Ok(value)
}

fn positive_duration(value: &str) -> Result<Duration, String> {
    let seconds = positive_usize(value)?;
    Ok(Duration::from_secs(seconds as u64))
}

fn drive_page_size(value: &str) -> Result<u16, String> {
    let value = positive_usize(value)?;
    if value > 1_000 {
        return Err("value must not exceed 1000".to_owned());
    }
    Ok(value as u16)
}

fn folder_walk_batch_size(value: &str) -> Result<usize, String> {
    let value = positive_usize(value)?;
    if value > 100 {
        return Err("value must not exceed 100".to_owned());
    }
    Ok(value)
}

fn rate_limit_share(value: &str) -> Result<f64, String> {
    let value = value
        .trim()
        .parse::<f64>()
        .map_err(|_| "value must be a number".to_owned())?;
    if !(value > 0.0 && value <= 1.0) {
        return Err("value must be greater than 0 and at most 1".to_owned());
    }
    Ok(value)
}

fn embeddings_dimensions(value: &str) -> Result<usize, String> {
    let value = positive_usize(value)?;
    if value != 1_536 {
        return Err("value must be 1536 for the current schema".to_owned());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn required_args() -> Vec<&'static str> {
        vec![
            "centaur-company-context",
            "--database-url",
            "postgresql://context",
            "--console-database-url",
            "postgresql://console",
            "--active-record-primary-key",
            "primary",
            "--active-record-key-derivation-salt",
            "salt",
            "--openai-api-key",
            "test-key",
            "--slack-bot-token",
            "xoxb-test",
            "--jwt-signing-secret",
            "jwt-secret",
        ]
    }

    #[test]
    fn parses_cli_arguments_and_normalizes_urls() {
        let mut args = required_args();
        args.extend(["--openai-base-url", "http://localhost:8080/v1///"]);
        let config = Config::try_parse_from(args).unwrap();
        assert_eq!(config.openai_base_url, "http://localhost:8080/v1");
        assert_eq!(config.scan_interval, Duration::from_secs(300));
        assert_eq!(config.slack_rate_limit_share, 0.3);
    }

    #[test]
    fn parses_sync_limits() {
        let config = Config::try_parse_from(required_args()).unwrap();
        assert!(config.google_drive_user_emails.is_empty());
        assert!(config.slack_channel_ids.is_empty());
        assert_eq!(
            config.slack_conversation_types,
            ["public_channel", "private_channel"]
        );

        let mut args = required_args();
        args.extend([
            "--google-drive-user-emails",
            "Ada@Example.com, grace@example.com",
            "--slack-channel-ids",
            "C1,G2",
            "--slack-conversation-types",
            "im, public_channel",
            "--slack-channel-history-days",
            "C1=3650, G2=365",
        ]);
        let config = Config::try_parse_from(args).unwrap();
        assert_eq!(
            config.google_drive_user_emails,
            ["ada@example.com", "grace@example.com"]
        );
        assert_eq!(config.slack_channel_ids, ["C1", "G2"]);
        assert_eq!(config.slack_conversation_types, ["im", "public_channel"]);
        assert_eq!(
            config.slack_channel_history_days,
            [("C1".to_owned(), 3650), ("G2".to_owned(), 365)]
        );

        let mut args = required_args();
        args.extend(["--slack-conversation-types", "mpim"]);
        assert!(Config::try_parse_from(args).is_err());

        for invalid in ["C1", "C1=0", "=30", "C1=36501"] {
            let mut args = required_args();
            args.extend(["--slack-channel-history-days", invalid]);
            assert!(Config::try_parse_from(args).is_err(), "{invalid}");
        }
    }

    #[test]
    fn rate_limit_share_must_be_a_fraction() {
        assert_eq!(rate_limit_share("1"), Ok(1.0));
        assert!(rate_limit_share("0").is_err());
        assert!(rate_limit_share("1.5").is_err());
        assert!(rate_limit_share("NaN").is_err());
    }
}

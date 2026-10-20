use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use absurd::{Client, SpawnOptions};
use serde::Serialize;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{error, info};

use crate::{
    attio_tasks::{AttioReconcileParams, AttioSyncParams},
    calendar_tasks::{CalendarDiscoverParams, CalendarReconcileParams},
    config::{
        ATTIO_CREDENTIALS_RECONCILE_TASK, ATTIO_SYNC_TASK, CALENDAR_CREDENTIALS_RECONCILE_TASK,
        CALENDAR_DISCOVER_TASK, Config, DRIVE_CREDENTIALS_RECONCILE_TASK, DRIVE_SCAN_TASK,
        GRANOLA_CREDENTIALS_RECONCILE_TASK, GRANOLA_SYNC_TASK, IMPORTED_PROJECT_TASK,
        SHARED_DRIVES_DISCOVER_TASK, SLACK_CREDENTIALS_RECONCILE_TASK, SLACK_USER_DISCOVER_TASK,
        SLACK_USERS_SYNC_TASK,
    },
    credentials::{ConsoleCredentials, GoogleProduct},
    granola_tasks::{GranolaReconcileParams, GranolaSyncParams},
    imported_documents::ImportedProjectParams,
    slack_tasks::{SlackDiscoverParams, SlackReconcileParams, UsersSyncParams},
    tasks::{DiscoverSharedDrivesParams, ReconcileCredentialsParams, ScanParams},
    telemetry,
};

pub async fn run(config: Arc<Config>, client: Client, credentials: Arc<ConsoleCredentials>) {
    let mut ticker = interval(config.scan_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let bucket = now / config.scan_interval.as_secs().max(1);
        match client
            .spawn(
                DRIVE_CREDENTIALS_RECONCILE_TASK,
                ReconcileCredentialsParams { bucket },
                SpawnOptions {
                    idempotency_key: Some(format!("drive.credentials.reconcile:{bucket}")),
                    ..SpawnOptions::default()
                },
            )
            .await
        {
            Ok(result) => {
                telemetry::task_enqueued(DRIVE_CREDENTIALS_RECONCILE_TASK, result.created);
                info!(
                    event = "company_context_credentials_reconcile_enqueued",
                    task_id = result.task_id,
                    created = result.created
                );
            }
            Err(error) => {
                telemetry::scheduler_error("drive", "enqueue");
                error!(event = "company_context_credentials_reconcile_enqueue_failed", error = %error);
            }
        }
        let credential_ids = match credentials
            .google_credential_ids(GoogleProduct::Drive)
            .await
        {
            Ok(ids) => ids,
            Err(error) => {
                telemetry::scheduler_error("drive", "load_credentials");
                error!(event = "company_context_credentials_load_failed", error = %error);
                continue;
            }
        };
        for credential_id in credential_ids {
            spawn_credential_task(
                &client,
                "drive",
                DRIVE_SCAN_TASK,
                ScanParams {
                    credential_id,
                    requested_at: chrono::Utc::now().to_rfc3339(),
                },
                format!("drive.user.scan:{credential_id}:{bucket}"),
                credential_id,
            )
            .await;
            spawn_credential_task(
                &client,
                "drive",
                SHARED_DRIVES_DISCOVER_TASK,
                DiscoverSharedDrivesParams {
                    credential_id,
                    bucket,
                },
                format!("drive.shared_drives.discover:{credential_id}:{bucket}"),
                credential_id,
            )
            .await;
        }
    }
}

/// Enqueues Calendar reconciliation and per-credential calendar discovery.
pub async fn run_calendar(
    config: Arc<Config>,
    client: Client,
    credentials: Arc<ConsoleCredentials>,
) {
    let mut ticker = interval(config.calendar_sync_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let bucket = now / config.calendar_sync_interval.as_secs().max(1);
        match client
            .spawn(
                CALENDAR_CREDENTIALS_RECONCILE_TASK,
                CalendarReconcileParams { bucket },
                SpawnOptions {
                    idempotency_key: Some(format!("calendar.credentials.reconcile:{bucket}")),
                    ..SpawnOptions::default()
                },
            )
            .await
        {
            Ok(result) => {
                telemetry::task_enqueued(CALENDAR_CREDENTIALS_RECONCILE_TASK, result.created);
            }
            Err(error) => {
                telemetry::scheduler_error("calendar", "enqueue");
                error!(event = "company_context_calendar_reconcile_enqueue_failed", error = %error);
            }
        }
        let credential_ids = match credentials
            .google_credential_ids(GoogleProduct::Calendar)
            .await
        {
            Ok(ids) => ids,
            Err(error) => {
                telemetry::scheduler_error("calendar", "load_credentials");
                error!(event = "company_context_calendar_credentials_load_failed", error = %error);
                continue;
            }
        };
        for credential_id in credential_ids {
            spawn_credential_task(
                &client,
                "calendar",
                CALENDAR_DISCOVER_TASK,
                CalendarDiscoverParams {
                    credential_id,
                    bucket,
                },
                format!("calendar.user.discover:{credential_id}:{bucket}"),
                credential_id,
            )
            .await;
        }
    }
}

pub async fn run_granola(
    config: Arc<Config>,
    client: Client,
    credentials: Arc<ConsoleCredentials>,
) {
    let mut ticker = interval(config.granola_sync_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let bucket = now / config.granola_sync_interval.as_secs().max(1);
        match client
            .spawn(
                GRANOLA_CREDENTIALS_RECONCILE_TASK,
                GranolaReconcileParams { bucket },
                SpawnOptions {
                    idempotency_key: Some(format!("granola.credentials.reconcile:{bucket}")),
                    ..SpawnOptions::default()
                },
            )
            .await
        {
            Ok(result) => {
                telemetry::task_enqueued(GRANOLA_CREDENTIALS_RECONCILE_TASK, result.created);
            }
            Err(error) => {
                telemetry::scheduler_error("granola", "enqueue");
                error!(event = "company_context_granola_reconcile_enqueue_failed", error = %error);
            }
        }
        let credential_ids = match credentials.granola_credential_ids().await {
            Ok(ids) => ids,
            Err(error) => {
                telemetry::scheduler_error("granola", "load_credentials");
                error!(event = "company_context_granola_credentials_load_failed", error = %error);
                continue;
            }
        };
        for credential_id in credential_ids {
            spawn_credential_task(
                &client,
                "granola",
                GRANOLA_SYNC_TASK,
                GranolaSyncParams {
                    credential_id,
                    bucket,
                },
                format!("granola.user.sync:{credential_id}:{bucket}"),
                credential_id,
            )
            .await;
        }
    }
}

/// Enqueues Attio reconciliation and per-workspace meeting sync.
pub async fn run_attio(config: Arc<Config>, client: Client, credentials: Arc<ConsoleCredentials>) {
    let mut ticker = interval(config.attio_sync_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let bucket = now / config.attio_sync_interval.as_secs().max(1);
        match client
            .spawn(
                ATTIO_CREDENTIALS_RECONCILE_TASK,
                AttioReconcileParams { bucket },
                SpawnOptions {
                    idempotency_key: Some(format!("attio.credentials.reconcile:{bucket}")),
                    ..SpawnOptions::default()
                },
            )
            .await
        {
            Ok(result) => {
                telemetry::task_enqueued(ATTIO_CREDENTIALS_RECONCILE_TASK, result.created);
            }
            Err(error) => {
                telemetry::scheduler_error("attio", "enqueue");
                error!(event = "company_context_attio_reconcile_enqueue_failed", error = %error);
            }
        }
        let credential_ids = match credentials.attio_credential_ids().await {
            Ok(ids) => ids,
            Err(error) => {
                telemetry::scheduler_error("attio", "load_credentials");
                error!(event = "company_context_attio_credentials_load_failed", error = %error);
                continue;
            }
        };
        for credential_id in credential_ids {
            spawn_credential_task(
                &client,
                "attio",
                ATTIO_SYNC_TASK,
                AttioSyncParams {
                    credential_id,
                    bucket,
                },
                format!("attio.workspace.sync:{credential_id}:{bucket}"),
                credential_id,
            )
            .await;
        }
    }
}

/// Enqueues projection of imported documents.
pub async fn run_imported(config: Arc<Config>, client: Client) {
    let mut ticker = interval(config.imported_project_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let bucket = now / config.imported_project_interval.as_secs().max(1);
        match client
            .spawn(
                IMPORTED_PROJECT_TASK,
                ImportedProjectParams { bucket },
                SpawnOptions {
                    idempotency_key: Some(format!("imported.project:{bucket}")),
                    ..SpawnOptions::default()
                },
            )
            .await
        {
            Ok(result) => telemetry::task_enqueued(IMPORTED_PROJECT_TASK, result.created),
            Err(error) => {
                telemetry::scheduler_error("imported", "enqueue");
                error!(event = "company_context_imported_project_enqueue_failed", error = %error);
            }
        }
    }
}

/// Enqueues Slack reconciliation, the bot's users sync, and per-credential
/// discovery on the Slack queue.
pub async fn run_slack(config: Arc<Config>, client: Client, credentials: Arc<ConsoleCredentials>) {
    let mut ticker = interval(config.slack_discovery_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let bucket = now / config.slack_discovery_interval.as_secs().max(1);
        match client
            .spawn(
                SLACK_CREDENTIALS_RECONCILE_TASK,
                SlackReconcileParams { bucket },
                SpawnOptions {
                    idempotency_key: Some(format!("slack.credentials.reconcile:{bucket}")),
                    ..SpawnOptions::default()
                },
            )
            .await
        {
            Ok(result) => {
                telemetry::task_enqueued(SLACK_CREDENTIALS_RECONCILE_TASK, result.created);
            }
            Err(error) => {
                telemetry::scheduler_error("slack", "enqueue");
                error!(event = "company_context_slack_reconcile_enqueue_failed", error = %error);
            }
        }
        match client
            .spawn(
                SLACK_USERS_SYNC_TASK,
                UsersSyncParams { bucket },
                SpawnOptions {
                    idempotency_key: Some(format!("slack.team.users.sync:{bucket}")),
                    ..SpawnOptions::default()
                },
            )
            .await
        {
            Ok(result) => telemetry::task_enqueued(SLACK_USERS_SYNC_TASK, result.created),
            Err(error) => {
                telemetry::scheduler_error("slack", "enqueue");
                error!(event = "company_context_slack_users_enqueue_failed", error = %error);
            }
        }
        let credential_ids = match credentials.slack_credential_ids().await {
            Ok(ids) => ids,
            Err(error) => {
                telemetry::scheduler_error("slack", "load_credentials");
                error!(event = "company_context_slack_credentials_load_failed", error = %error);
                continue;
            }
        };
        for credential_id in credential_ids {
            spawn_credential_task(
                &client,
                "slack",
                SLACK_USER_DISCOVER_TASK,
                SlackDiscoverParams {
                    credential_id,
                    bucket,
                },
                format!("slack.user.discover:{credential_id}:{bucket}"),
                credential_id,
            )
            .await;
        }
    }
}

async fn spawn_credential_task<P: Serialize>(
    client: &Client,
    source: &'static str,
    task_name: &'static str,
    params: P,
    idempotency_key: String,
    credential_id: i64,
) {
    match client
        .spawn(
            task_name,
            params,
            SpawnOptions {
                idempotency_key: Some(idempotency_key),
                ..SpawnOptions::default()
            },
        )
        .await
    {
        Ok(result) => {
            telemetry::task_enqueued(task_name, result.created);
            info!(
                event = "company_context_credential_task_enqueued",
                task_name,
                credential_id,
                task_id = result.task_id,
                created = result.created
            );
        }
        Err(error) => {
            telemetry::scheduler_error(source, "enqueue");
            error!(
                event = "company_context_credential_task_enqueue_failed",
                task_name,
                credential_id,
                error = %error
            );
        }
    }
}

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use absurd::{Client, SpawnOptions};
use serde::Serialize;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{error, info};

use crate::{
    config::{
        Config, DRIVE_CREDENTIALS_RECONCILE_TASK, DRIVE_SCAN_TASK,
        GRANOLA_CREDENTIALS_RECONCILE_TASK, GRANOLA_SYNC_TASK, SHARED_DRIVES_DISCOVER_TASK,
    },
    credentials::ConsoleCredentials,
    granola_tasks::{GranolaReconcileParams, GranolaSyncParams},
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
                metrics::counter!("company_context_scheduler_errors_total").increment(1);
                error!(event = "company_context_credentials_reconcile_enqueue_failed", error = %error);
            }
        }
        let credential_ids = match credentials.google_credential_ids().await {
            Ok(ids) => ids,
            Err(error) => {
                metrics::counter!("company_context_scheduler_errors_total").increment(1);
                error!(event = "company_context_credentials_load_failed", error = %error);
                continue;
            }
        };
        for credential_id in credential_ids {
            spawn_credential_task(
                &client,
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
                metrics::counter!("company_context_scheduler_errors_total").increment(1);
                error!(event = "company_context_granola_reconcile_enqueue_failed", error = %error);
            }
        }
        let credential_ids = match credentials.granola_credential_ids().await {
            Ok(ids) => ids,
            Err(error) => {
                metrics::counter!("company_context_scheduler_errors_total").increment(1);
                error!(event = "company_context_granola_credentials_load_failed", error = %error);
                continue;
            }
        };
        for credential_id in credential_ids {
            spawn_credential_task(
                &client,
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

async fn spawn_credential_task<P: Serialize>(
    client: &Client,
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
            metrics::counter!("company_context_scheduler_errors_total").increment(1);
            error!(
                event = "company_context_credential_task_enqueue_failed",
                task_name,
                credential_id,
                error = %error
            );
        }
    }
}

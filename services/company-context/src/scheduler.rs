use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use absurd::{Client, SpawnOptions};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{error, info};

use crate::{
    config::{Config, DRIVE_CREDENTIALS_RECONCILE_TASK, DRIVE_SCAN_TASK},
    credentials::ConsoleCredentials,
    tasks::{ReconcileCredentialsParams, ScanParams},
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
            let requested_at = chrono::Utc::now().to_rfc3339();
            match client
                .spawn(
                    DRIVE_SCAN_TASK,
                    ScanParams {
                        credential_id,
                        requested_at,
                    },
                    SpawnOptions {
                        idempotency_key: Some(format!("drive.user.scan:{credential_id}:{bucket}")),
                        ..SpawnOptions::default()
                    },
                )
                .await
            {
                Ok(result) => {
                    telemetry::task_enqueued(DRIVE_SCAN_TASK, result.created);
                    info!(
                        event = "company_context_user_scan_enqueued",
                        credential_id,
                        task_id = result.task_id,
                        created = result.created
                    );
                }
                Err(error) => {
                    metrics::counter!("company_context_scheduler_errors_total").increment(1);
                    error!(
                        event = "company_context_user_scan_enqueue_failed",
                        credential_id,
                        error = %error
                    );
                }
            }
        }
    }
}

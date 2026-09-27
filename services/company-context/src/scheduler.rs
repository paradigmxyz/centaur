use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use absurd::{Client, SpawnOptions};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{error, info};

use crate::{
    config::{Config, DRIVE_SCAN_TASK},
    tasks::ScanParams,
    telemetry,
};

pub async fn run(config: Arc<Config>, client: Client) {
    let mut ticker = interval(config.scan_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let bucket = now / config.scan_interval.as_secs().max(1);
        let requested_at = chrono::Utc::now().to_rfc3339();
        match client
            .spawn(
                DRIVE_SCAN_TASK,
                ScanParams { requested_at },
                SpawnOptions {
                    idempotency_key: Some(format!("drive.scan:{bucket}")),
                    ..SpawnOptions::default()
                },
            )
            .await
        {
            Ok(result) => {
                telemetry::task_enqueued(DRIVE_SCAN_TASK, result.created);
                info!(
                    event = "company_context_scan_enqueued",
                    task_id = result.task_id,
                    created = result.created
                );
            }
            Err(error) => {
                metrics::counter!("company_context_scheduler_errors_total").increment(1);
                error!(event = "company_context_scan_enqueue_failed", error = %error);
            }
        }
    }
}

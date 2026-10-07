//! Samples queue depth, staged items, and sync freshness from the database.
//! The values are global, so every replica reports the same gauges.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use anyhow::Result;
use sqlx::{PgPool, Row};
use tokio::time::{MissedTickBehavior, interval};
use tracing::warn;

use crate::{
    config::{QUEUE_NAME, SLACK_QUEUE_NAME},
    credentials::ConsoleCredentials,
    telemetry,
};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(30);

type Labels = Vec<(&'static str, String)>;
type Sample = (&'static str, Labels, f64);

/// Gauges published by one sample. Series missing from the next sample are
/// reset to zero, so a drained queue state reads 0 rather than its last value.
#[derive(Default)]
struct GaugeSet {
    published: BTreeSet<(&'static str, Labels)>,
}

impl GaugeSet {
    fn publish(&mut self, samples: Vec<Sample>) {
        let mut current = BTreeSet::new();
        for (name, labels, value) in samples {
            metrics::gauge!(name, &labels).set(value);
            current.insert((name, labels));
        }
        for (name, labels) in self.published.difference(&current) {
            metrics::gauge!(*name, labels).set(0.0);
        }
        self.published = current;
    }

    fn update(&mut self, part: &'static str, samples: Result<Vec<Sample>>) {
        match samples {
            Ok(samples) => self.publish(samples),
            Err(error) => {
                telemetry::sample_error();
                warn!(event = "company_context_metrics_sample_failed", part, error = %error);
            }
        }
    }
}

pub async fn run(pool: PgPool, credentials: Arc<ConsoleCredentials>) {
    let mut ticker = interval(SAMPLE_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut queues = GaugeSet::default();
    let mut items = GaugeSet::default();
    let mut freshness = GaugeSet::default();
    loop {
        ticker.tick().await;
        queues.update("queues", queue_samples(&pool).await);
        items.update("items", item_samples(&pool).await);
        freshness.update("freshness", freshness_samples(&pool, &credentials).await);
    }
}

async fn queue_samples(pool: &PgPool) -> Result<Vec<Sample>> {
    let mut samples = Vec::new();
    for queue in [QUEUE_NAME, SLACK_QUEUE_NAME] {
        samples.extend(queue_samples_for(pool, queue).await?);
    }
    Ok(samples)
}

/// Classifies each unfinished task by its live run: claimable now (`ready`),
/// `running`, or `waiting` for a retry backoff, durable sleep, or event.
async fn queue_samples_for(pool: &PgPool, queue: &'static str) -> Result<Vec<Sample>> {
    let rows = sqlx::query(&format!(
        r#"
        SELECT
            t.task_name,
            CASE
                WHEN r.state = 'running' THEN 'running'
                WHEN r.available_at <= now() THEN 'ready'
                ELSE 'waiting'
            END AS state,
            count(*)::float8 AS tasks,
            coalesce(extract(epoch FROM now() - min(
                CASE
                    WHEN r.state = 'running' THEN r.started_at
                    WHEN r.available_at <= now() THEN r.available_at
                    ELSE t.enqueue_at
                END
            )), 0)::float8 AS oldest_age_seconds
        FROM absurd."r_{queue}" r
        JOIN absurd."t_{queue}" t ON t.task_id = r.task_id
        WHERE r.state IN ('pending', 'running', 'sleeping')
          AND t.state IN ('pending', 'running', 'sleeping')
        GROUP BY 1, 2
        "#
    ))
    .fetch_all(pool)
    .await?;
    let mut samples = Vec::with_capacity(rows.len() * 2);
    for row in rows {
        let labels = vec![
            ("queue", queue.to_owned()),
            ("task", row.try_get("task_name")?),
            ("state", row.try_get("state")?),
        ];
        samples.push((
            telemetry::QUEUE_TASKS,
            labels.clone(),
            row.try_get("tasks")?,
        ));
        samples.push((
            telemetry::QUEUE_OLDEST_TASK_AGE,
            labels,
            row.try_get("oldest_age_seconds")?,
        ));
    }
    Ok(samples)
}

async fn item_samples(pool: &PgPool) -> Result<Vec<Sample>> {
    let rows = sqlx::query(
        r#"
        SELECT 'drive' AS source, 'extraction' AS stage, extraction_status AS status, count(*)::float8 AS items
        FROM company_context_system.google_drive_files GROUP BY extraction_status
        UNION ALL
        SELECT 'drive', 'embedding', embedding_status, count(*)::float8
        FROM company_context_system.google_drive_files GROUP BY embedding_status
        UNION ALL
        SELECT 'granola', 'embedding', embedding_status, count(*)::float8
        FROM company_context_system.granola_notes GROUP BY embedding_status
        UNION ALL
        SELECT 'slack', 'embedding', embedding_status, count(*)::float8
        FROM company_context_system.slack_channel_days GROUP BY embedding_status
        UNION ALL
        SELECT 'slack_file', 'extraction', extraction_status, count(*)::float8
        FROM company_context_system.slack_files GROUP BY extraction_status
        UNION ALL
        SELECT 'slack_file', 'embedding', embedding_status, count(*)::float8
        FROM company_context_system.slack_files
        WHERE extraction_status = 'completed' GROUP BY embedding_status
        "#,
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok((
                telemetry::ITEMS,
                vec![
                    ("source", row.try_get("source")?),
                    ("stage", row.try_get("stage")?),
                    ("status", row.try_get("status")?),
                ],
                row.try_get("items")?,
            ))
        })
        .collect()
}

/// Checkpoints of dead credentials are kept, so only those of live
/// credentials count toward freshness.
async fn freshness_samples(pool: &PgPool, credentials: &ConsoleCredentials) -> Result<Vec<Sample>> {
    let google = credentials.google_credential_ids().await?;
    let granola = credentials.granola_credential_ids().await?;
    let slack = credentials.slack_credential_ids().await?;
    let mut samples: Vec<Sample> = [("drive", &google), ("granola", &granola), ("slack", &slack)]
        .into_iter()
        .map(|(source, ids)| {
            (
                telemetry::LIVE_CREDENTIALS,
                vec![("source", source.to_owned())],
                ids.len() as f64,
            )
        })
        .collect();
    let ids = |ids: &[i64]| ids.iter().map(i64::to_string).collect::<Vec<_>>();
    let rows = sqlx::query(
        r#"
        SELECT 'drive' AS source,
               count(*)::float8 AS checkpoints,
               count(*) FILTER (WHERE last_error <> '')::float8 AS errors,
               coalesce(extract(epoch FROM now() - min(coalesce(last_success_at, created_at))), 0)::float8 AS oldest_success_age_seconds
        FROM company_context_system.google_drive_checkpoints
        WHERE split_part(scope_id, ':broker:', 2) = ANY($1)
        UNION ALL
        SELECT 'granola',
               count(*)::float8,
               count(*) FILTER (WHERE last_error <> '')::float8,
               coalesce(extract(epoch FROM now() - min(coalesce(last_success_at, created_at))), 0)::float8
        FROM company_context_system.granola_checkpoints
        WHERE split_part(scope_id, ':broker:', 2) = ANY($2)
        UNION ALL
        SELECT 'slack',
               count(*)::float8,
               count(*) FILTER (WHERE history_last_error <> '')::float8,
               coalesce(extract(epoch FROM now() - min(coalesce(history_synced_until, first_seen_at))), 0)::float8
        FROM company_context_system.slack_conversations
        "#,
    )
    .bind(ids(&google))
    .bind(ids(&granola))
    .fetch_all(pool)
    .await?;
    for row in rows {
        let labels = vec![("source", row.try_get::<String, _>("source")?)];
        samples.push((
            telemetry::CHECKPOINTS_WITH_ERRORS,
            labels.clone(),
            row.try_get("errors")?,
        ));
        samples.push((
            telemetry::CHECKPOINT_OLDEST_SUCCESS_AGE,
            labels,
            row.try_get("oldest_success_age_seconds")?,
        ));
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use std::env;

    use absurd::{Client, ClientOptions, CreateQueueOptions, SpawnOptions, WorkBatchOptions};
    use serde_json::json;

    use super::*;
    use crate::test_support::TestDatabase;

    const ABSURD_SCHEMA: &str = include_str!(
        "../../api-rs/crates/centaur-session-sqlx/migrations/0007_absurd_workflows.sql"
    );

    fn tasks(samples: &[Sample], state: &str) -> f64 {
        samples
            .iter()
            .filter(|(name, labels, _)| {
                *name == telemetry::QUEUE_TASKS
                    && labels.contains(&("task", "drive.document.embed".to_owned()))
                    && labels.contains(&("state", state.to_owned()))
            })
            .map(|(_, _, value)| *value)
            .sum()
    }

    #[tokio::test]
    async fn queue_samples_split_ready_running_and_waiting_tasks() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "sampler").await;
        let pool = &database.pool;
        sqlx::raw_sql(ABSURD_SCHEMA).execute(pool).await.unwrap();
        for queue in [QUEUE_NAME, SLACK_QUEUE_NAME] {
            let client = Client::from_pool_with_options(
                pool.clone(),
                ClientOptions {
                    pool: Some(pool.clone()),
                    queue_name: queue.to_owned(),
                    ..ClientOptions::default()
                },
            )
            .unwrap();
            client
                .create_queue(None, CreateQueueOptions::default())
                .await
                .unwrap();
        }
        let client = Client::from_pool_with_options(
            pool.clone(),
            ClientOptions {
                pool: Some(pool.clone()),
                queue_name: QUEUE_NAME.to_owned(),
                ..ClientOptions::default()
            },
        )
        .unwrap();
        for _ in 0..4 {
            client
                .spawn(
                    "drive.document.embed",
                    json!({}),
                    SpawnOptions {
                        queue: Some(QUEUE_NAME.to_owned()),
                        ..SpawnOptions::default()
                    },
                )
                .await
                .unwrap();
        }
        let claimed = client
            .claim_tasks(WorkBatchOptions {
                batch_size: 2,
                ..WorkBatchOptions::default()
            })
            .await
            .unwrap();
        // One claimed task goes into a durable sleep.
        sqlx::query("SELECT absurd.schedule_run($1, $2::uuid, now() + interval '1 hour')")
            .bind(QUEUE_NAME)
            .bind(&claimed[0].run_id)
            .execute(pool)
            .await
            .unwrap();

        let samples = queue_samples(pool).await.unwrap();
        assert_eq!(tasks(&samples, "ready"), 2.0);
        assert_eq!(tasks(&samples, "running"), 1.0);
        assert_eq!(tasks(&samples, "waiting"), 1.0);
        database.drop().await;
    }
}

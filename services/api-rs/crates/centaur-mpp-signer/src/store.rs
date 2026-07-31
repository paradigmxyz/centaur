use anyhow::Context as _;
use async_trait::async_trait;
use serde_json::Value;
use sqlx::{PgPool, Row as _};
use uuid::Uuid;

use crate::model::{
    ActiveExecution, BeginAttempt, Catalog, CompletedAttempt, CompletionOutcome, NewAttempt,
    RegistrySnapshot,
};

#[async_trait]
pub trait SignerStore: Send + Sync {
    async fn load_registry_cache(&self) -> anyhow::Result<Option<RegistrySnapshot>>;
    async fn save_registry_cache(&self, snapshot: &RegistrySnapshot) -> anyhow::Result<()>;
    async fn active_execution(&self, sandbox_id: &str) -> anyhow::Result<Option<ActiveExecution>>;
    async fn active_execution_lease_count(&self) -> anyhow::Result<i64>;
    async fn active_reservation_count(&self) -> anyhow::Result<i64>;
    async fn begin_attempt(
        &self,
        attempt: &NewAttempt,
        max_per_charge_atomic: Option<i64>,
        max_daily_atomic: Option<i64>,
    ) -> anyhow::Result<BeginAttempt>;
    async fn mark_authorized(&self, attempt_id: Uuid) -> anyhow::Result<()>;
    async fn mark_sign_failed(&self, attempt_id: Uuid, error_code: &str) -> anyhow::Result<()>;
    async fn complete_attempt(
        &self,
        attempt_id: Uuid,
        outcome: CompletionOutcome,
        replay_status: Option<u16>,
        receipt_hash: Option<&str>,
        error_code: Option<&str>,
    ) -> anyhow::Result<Option<CompletedAttempt>>;
    async fn ready(&self) -> bool;
}

#[derive(Clone)]
pub struct PgSignerStore {
    pool: PgPool,
}

impl PgSignerStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SignerStore for PgSignerStore {
    async fn load_registry_cache(&self) -> anyhow::Result<Option<RegistrySnapshot>> {
        let row = sqlx::query(
            r#"
            select catalog, fetched_at, etag, last_modified
            from mpp_registry_cache
            where singleton = true and schema_version = 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let catalog_value: Value = row.try_get("catalog")?;
        let catalog = serde_json::from_value::<Catalog>(catalog_value)
            .context("decode cached MPP registry")?;
        Ok(Some(RegistrySnapshot {
            catalog,
            fetched_at: row.try_get("fetched_at")?,
            etag: row.try_get("etag")?,
            last_modified: row.try_get("last_modified")?,
        }))
    }

    async fn save_registry_cache(&self, snapshot: &RegistrySnapshot) -> anyhow::Result<()> {
        let catalog = serde_json::to_value(&snapshot.catalog)?;
        sqlx::query(
            r#"
            insert into mpp_registry_cache (
                singleton, schema_version, catalog, fetched_at, etag, last_modified, updated_at
            )
            values (true, 1, $1, $2, $3, $4, now())
            on conflict (singleton) do update
            set schema_version = excluded.schema_version,
                catalog = excluded.catalog,
                fetched_at = excluded.fetched_at,
                etag = excluded.etag,
                last_modified = excluded.last_modified,
                updated_at = now()
            "#,
        )
        .bind(catalog)
        .bind(snapshot.fetched_at)
        .bind(&snapshot.etag)
        .bind(&snapshot.last_modified)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn active_execution(&self, sandbox_id: &str) -> anyhow::Result<Option<ActiveExecution>> {
        let rows = sqlx::query(
            r#"
            select e.execution_id, e.thread_key, s.sandbox_id
            from session_executions e
            join sessions s on s.thread_key = e.thread_key
            where s.sandbox_id = $1
              and e.status = 'running'
              and e.stdout_owner_id is not null
              and e.stdout_owner_lease_expires_at > now()
            order by e.started_at desc nulls last
            limit 2
            "#,
        )
        .bind(sandbox_id)
        .fetch_all(&self.pool)
        .await?;
        anyhow::ensure!(
            rows.len() <= 1,
            "sandbox has multiple active execution leases"
        );
        Ok(rows.first().map(|row| ActiveExecution {
            execution_id: row.get("execution_id"),
            sandbox_id: row.get("sandbox_id"),
            thread_key: row.get("thread_key"),
        }))
    }

    async fn active_execution_lease_count(&self) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar(
            r#"
            select count(*)::bigint
            from session_executions
            where status = 'running'
              and stdout_owner_id is not null
              and stdout_owner_lease_expires_at > now()
            "#,
        )
        .fetch_one(&self.pool)
        .await?)
    }

    async fn active_reservation_count(&self) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar(
            r#"
            select count(*)::bigint
            from mpp_charge_attempts
            where status in ('reserving', 'authorized')
            "#,
        )
        .fetch_one(&self.pool)
        .await?)
    }

    async fn begin_attempt(
        &self,
        attempt: &NewAttempt,
        max_per_charge_atomic: Option<i64>,
        max_daily_atomic: Option<i64>,
    ) -> anyhow::Result<BeginAttempt> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(hashtext('centaur-mpp-budget'))")
            .execute(&mut *transaction)
            .await?;

        if let Some(existing) = sqlx::query(
            "select attempt_id, sandbox_id, execution_id from mpp_charge_attempts where challenge_hash = $1",
        )
        .bind(&attempt.challenge_hash)
        .fetch_optional(&mut *transaction)
        .await?
        {
            transaction.commit().await?;
            return Ok(BeginAttempt::Duplicate {
                attempt_id: existing.get("attempt_id"),
                sandbox_id: existing.get("sandbox_id"),
                execution_id: existing.get("execution_id"),
            });
        }

        if max_per_charge_atomic.is_some_and(|maximum| attempt.amount_atomic > maximum) {
            transaction.rollback().await?;
            return Ok(BeginAttempt::BudgetDenied {
                reason: "per_charge_budget_exceeded",
            });
        }
        if let Some(maximum) = max_daily_atomic {
            let spent = sqlx::query_scalar::<_, i64>(
                r#"
                select coalesce(sum(amount_atomic), 0)::bigint
                from mpp_charge_attempts
                where created_at >= date_trunc('day', now() at time zone 'UTC') at time zone 'UTC'
                  and status in ('reserving', 'authorized', 'settled', 'unknown')
                "#,
            )
            .fetch_one(&mut *transaction)
            .await?;
            if spent.saturating_add(attempt.amount_atomic) > maximum {
                transaction.rollback().await?;
                return Ok(BeginAttempt::BudgetDenied {
                    reason: "daily_budget_exceeded",
                });
            }
        }

        sqlx::query(
            r#"
            insert into mpp_charge_attempts (
                attempt_id,
                challenge_hash,
                service_id,
                method,
                path_template,
                amount_atomic,
                currency,
                sandbox_id,
                execution_id,
                status
            )
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'reserving')
            "#,
        )
        .bind(attempt.attempt_id)
        .bind(&attempt.challenge_hash)
        .bind(&attempt.service_id)
        .bind(&attempt.method)
        .bind(&attempt.path_template)
        .bind(attempt.amount_atomic)
        .bind(&attempt.currency)
        .bind(&attempt.sandbox_id)
        .bind(&attempt.execution_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(BeginAttempt::Created)
    }

    async fn mark_authorized(&self, attempt_id: Uuid) -> anyhow::Result<()> {
        let changed = sqlx::query(
            r#"
            update mpp_charge_attempts
            set status = 'authorized', authorized_at = now()
            where attempt_id = $1 and status = 'reserving'
            "#,
        )
        .bind(attempt_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        anyhow::ensure!(changed == 1, "MPP attempt was not reserving");
        Ok(())
    }

    async fn mark_sign_failed(&self, attempt_id: Uuid, error_code: &str) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            update mpp_charge_attempts
            set status = 'sign_failed', error_code = $2, completed_at = now()
            where attempt_id = $1 and status = 'reserving'
            "#,
        )
        .bind(attempt_id)
        .bind(error_code)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn complete_attempt(
        &self,
        attempt_id: Uuid,
        outcome: CompletionOutcome,
        replay_status: Option<u16>,
        receipt_hash: Option<&str>,
        error_code: Option<&str>,
    ) -> anyhow::Result<Option<CompletedAttempt>> {
        let row = sqlx::query(
            r#"
            update mpp_charge_attempts
            set status = $2,
                replay_status = $3,
                receipt_hash = $4,
                error_code = $5,
                completed_at = now()
            where attempt_id = $1 and status in ('reserving', 'authorized')
            returning service_id, method, amount_atomic, currency
            "#,
        )
        .bind(attempt_id)
        .bind(outcome.as_str())
        .bind(replay_status.map(i32::from))
        .bind(receipt_hash)
        .bind(error_code)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| CompletedAttempt {
            service_id: row.get("service_id"),
            method: row.get("method"),
            amount_atomic: row.get("amount_atomic"),
            currency: row.get("currency"),
            outcome,
            reason: error_code.unwrap_or("none").to_owned(),
        }))
    }

    async fn ready(&self) -> bool {
        sqlx::query_scalar::<_, i32>("select 1")
            .fetch_one(&self.pool)
            .await
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use std::{env, sync::OnceLock};

    use centaur_session_sqlx::PgSessionStore;
    use sqlx::PgPool;

    use super::*;

    fn database_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    async fn test_pool() -> Option<PgPool> {
        let database_url = env::var("SESSION_SQLX_TEST_DATABASE_URL")
            .or_else(|_| env::var("SESSION_RUNTIME_TEST_DATABASE_URL"))
            .ok()?;
        let session_store = PgSessionStore::connect(&database_url)
            .await
            .expect("connect test database");
        session_store
            .run_migrations()
            .await
            .expect("run test migrations");
        Some(session_store.pool().clone())
    }

    fn attempt(execution_id: &str, suffix: &str) -> NewAttempt {
        NewAttempt {
            attempt_id: Uuid::new_v4(),
            challenge_hash: format!("challenge-{suffix}-{}", Uuid::new_v4()),
            service_id: "budget-test".to_owned(),
            method: "GET".to_owned(),
            path_template: "/paid".to_owned(),
            amount_atomic: 60,
            currency: "test-token".to_owned(),
            sandbox_id: "sandbox-budget-test".to_owned(),
            execution_id: execution_id.to_owned(),
        }
    }

    #[tokio::test]
    async fn concurrent_attempts_reserve_daily_budget_atomically() {
        let _guard = database_lock().lock().await;
        let Some(pool) = test_pool().await else {
            eprintln!("skipping: no session SQLx test database configured");
            return;
        };
        sqlx::query("delete from mpp_charge_attempts")
            .execute(&pool)
            .await
            .expect("clear attempts");
        let suffix = Uuid::new_v4().to_string();
        let thread_key = format!("mpp-budget:{suffix}");
        let execution_id = format!("exe-mpp-budget-{suffix}");
        sqlx::query(
            "insert into sessions (thread_key, sandbox_id, harness_type, status) values ($1, $2, 'codex', 'active')",
        )
        .bind(&thread_key)
        .bind("sandbox-budget-test")
        .execute(&pool)
        .await
        .expect("insert session");
        sqlx::query(
            "insert into session_executions (execution_id, thread_key, status) values ($1, $2, 'completed')",
        )
        .bind(&execution_id)
        .bind(&thread_key)
        .execute(&pool)
        .await
        .expect("insert execution");

        let store = PgSignerStore::new(pool.clone());
        let first = attempt(&execution_id, "first");
        let second = attempt(&execution_id, "second");
        let (first_result, second_result) = tokio::join!(
            store.begin_attempt(&first, None, Some(100)),
            store.begin_attempt(&second, None, Some(100)),
        );
        let results = [first_result.unwrap(), second_result.unwrap()];
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, BeginAttempt::Created))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(
                    result,
                    BeginAttempt::BudgetDenied {
                        reason: "daily_budget_exceeded"
                    }
                ))
                .count(),
            1
        );
        assert_eq!(store.active_reservation_count().await.unwrap(), 1);

        sqlx::query("delete from mpp_charge_attempts")
            .execute(&pool)
            .await
            .expect("clear attempts");
        let unlimited_first = attempt(&execution_id, "unlimited-first");
        let unlimited_second = attempt(&execution_id, "unlimited-second");
        let (first_result, second_result) = tokio::join!(
            store.begin_attempt(&unlimited_first, None, None),
            store.begin_attempt(&unlimited_second, None, None),
        );
        assert!(matches!(first_result.unwrap(), BeginAttempt::Created));
        assert!(matches!(second_result.unwrap(), BeginAttempt::Created));
        assert_eq!(store.active_reservation_count().await.unwrap(), 2);

        sqlx::query("delete from mpp_charge_attempts")
            .execute(&pool)
            .await
            .expect("clear attempts");
        sqlx::query("delete from sessions where thread_key = $1")
            .bind(thread_key)
            .execute(&pool)
            .await
            .expect("delete session");
    }
}

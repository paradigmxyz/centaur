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
            where singleton = true
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
                singleton, catalog, fetched_at, etag, last_modified, updated_at
            )
            values (true, $1, $2, $3, $4, now())
            on conflict (singleton) do update
            set catalog = excluded.catalog,
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
            select e.execution_id, s.sandbox_id
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

        if let Some(existing) = sqlx::query_scalar::<_, Uuid>(
            "select attempt_id from mpp_charge_attempts where challenge_hash = $1",
        )
        .bind(&attempt.challenge_hash)
        .fetch_optional(&mut *transaction)
        .await?
        {
            transaction.commit().await?;
            return Ok(BeginAttempt::Duplicate {
                attempt_id: existing,
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

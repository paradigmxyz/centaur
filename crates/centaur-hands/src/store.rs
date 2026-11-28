use std::collections::BTreeSet;

use async_trait::async_trait;
use sqlx::PgPool;
use thiserror::Error;
use time::OffsetDateTime;

use crate::model::{HandBox, HandSize, LeaseClaim};

pub type HandsResult<T> = Result<T, HandsError>;

#[derive(Debug, Error)]
pub enum HandsError {
    #[error(transparent)]
    Store(#[from] sqlx::Error),
    #[error("hand backend failed: {0}")]
    Backend(String),
    #[error("hand lease not found")]
    NotFound,
    #[error("the hand pool is at capacity")]
    Capacity,
    #[error("no hand became ready: {0}")]
    NotReady(String),
    #[error("invalid hand request: {0}")]
    Invalid(String),
}

/// The durable pool and lease state. Kept behind a trait so pool decisions are
/// testable without Postgres; [`PgHandStore`] carries the SQL that matters for
/// concurrent claims.
#[async_trait]
pub trait HandStore: Send + Sync {
    async fn insert_warming(
        &self,
        id: &str,
        size: HandSize,
        pod_name: &str,
        pod_ip: Option<&str>,
        iron_proxy_id: &str,
    ) -> HandsResult<HandBox>;
    async fn mark_ready(&self, id: &str, pod_ip: &str) -> HandsResult<Option<HandBox>>;
    async fn list_warming(&self) -> HandsResult<Vec<HandBox>>;
    async fn claim_ready(&self, size: HandSize, claim: &LeaseClaim)
    -> HandsResult<Option<HandBox>>;
    async fn lease_specific(&self, id: &str, claim: &LeaseClaim) -> HandsResult<Option<HandBox>>;
    async fn find_thread_lease(&self, thread_key: &str) -> HandsResult<Option<HandBox>>;
    async fn find_by_lease(&self, lease_id: &str) -> HandsResult<Option<HandBox>>;
    /// Rewrite the bound identity on a live lease without changing its expiry.
    async fn update_binding(
        &self,
        lease_id: &str,
        thread_key: &str,
        principal: &str,
        requester: Option<&str>,
    ) -> HandsResult<Option<HandBox>>;
    async fn renew_lease(
        &self,
        lease_id: &str,
        thread_key: &str,
        principal: &str,
        requester: Option<&str>,
        expires_at: OffsetDateTime,
    ) -> HandsResult<Option<HandBox>>;
    /// Transition an expired leased row to `deleting`. Returns `None` when a
    /// concurrent renew made it live, so the caller must not delete the pod.
    async fn expire_lease(&self, id: &str, now: OffsetDateTime) -> HandsResult<Option<HandBox>>;
    /// Transition a live leased row to `deleting` because its hand is unusable.
    /// The lease id guards against deleting a different thread's replacement.
    async fn discard_lease(&self, id: &str, lease_id: &str) -> HandsResult<Option<HandBox>>;
    /// Transition an unleased (`warming` or `ready`) row to `deleting`.
    /// Leased rows use the conditional expiry/discard transitions instead.
    async fn set_deleting(&self, id: &str) -> HandsResult<()>;
    async fn delete(&self, id: &str) -> HandsResult<()>;
    /// Hands that consume the global cap: warming, ready, and leased.
    async fn total_hands(&self) -> HandsResult<usize>;
    /// Ready plus warming rows for one size.
    async fn count_pool(&self, size: HandSize) -> HandsResult<usize>;
    async fn list_expired(&self, now: OffsetDateTime) -> HandsResult<Vec<HandBox>>;
    async fn list_leased(&self) -> HandsResult<Vec<HandBox>>;
    async fn list_stuck_warming(&self, older_than: OffsetDateTime) -> HandsResult<Vec<HandBox>>;
    async fn list_deleting(&self, older_than: OffsetDateTime) -> HandsResult<Vec<HandBox>>;
    async fn live_ids(&self) -> HandsResult<BTreeSet<String>>;
}

/// Postgres-backed pool and lease state. api-rs applies the `hand_boxes`
/// migration through the session database migrator.
#[derive(Clone, Debug)]
pub struct PgHandStore {
    pool: PgPool,
}

impl PgHandStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Database(database) if database.code().as_deref() == Some("23505")
    )
}

#[async_trait]
impl HandStore for PgHandStore {
    async fn insert_warming(
        &self,
        id: &str,
        size: HandSize,
        pod_name: &str,
        pod_ip: Option<&str>,
        iron_proxy_id: &str,
    ) -> HandsResult<HandBox> {
        Ok(sqlx::query_as::<_, HandBox>(
            r#"
            insert into hand_boxes
                (id, size, state, pod_name, pod_ip, iron_proxy_id)
            values ($1, $2, 'warming', $3, $4, $5)
            returning *
            "#,
        )
        .bind(id)
        .bind(size.as_str())
        .bind(pod_name)
        .bind(pod_ip)
        .bind(iron_proxy_id)
        .fetch_one(&self.pool)
        .await?)
    }

    async fn mark_ready(&self, id: &str, pod_ip: &str) -> HandsResult<Option<HandBox>> {
        // Idempotent: the replenisher often marks the same warming hand ready
        // just before a cold checkout's wait loop does. Returning the existing
        // ready row keeps a healthy hand from being destroyed as "not ready".
        Ok(sqlx::query_as::<_, HandBox>(
            r#"
            update hand_boxes
            set state = 'ready',
                pod_ip = case when state = 'warming' then $2 else pod_ip end,
                updated_at = case when state = 'warming' then now() else updated_at end
            where id = $1 and state in ('warming', 'ready')
            returning *
            "#,
        )
        .bind(id)
        .bind(pod_ip)
        .fetch_optional(&self.pool)
        .await?)
    }

    async fn list_warming(&self) -> HandsResult<Vec<HandBox>> {
        Ok(sqlx::query_as::<_, HandBox>(
            "select * from hand_boxes where state = 'warming' order by created_at",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    async fn claim_ready(
        &self,
        size: HandSize,
        claim: &LeaseClaim,
    ) -> HandsResult<Option<HandBox>> {
        let claimed = sqlx::query_as::<_, HandBox>(
            r#"
            update hand_boxes
            set state = 'leased',
                lease_id = $2,
                thread_key = $3,
                principal = $4,
                requester = $5,
                leased_at = now(),
                lease_expires_at = $6,
                updated_at = now()
            where id = (
                select id from hand_boxes
                where state = 'ready' and size = $1
                order by created_at
                for update skip locked
                limit 1
            )
            returning *
            "#,
        )
        .bind(size.as_str())
        .bind(&claim.lease_id)
        .bind(&claim.thread_key)
        .bind(&claim.principal)
        .bind(&claim.requester)
        .bind(claim.expires_at)
        .fetch_optional(&self.pool)
        .await;
        match claimed {
            Ok(row) => Ok(row),
            Err(error) if is_unique_violation(&error) => {
                self.find_thread_lease(&claim.thread_key).await
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn lease_specific(&self, id: &str, claim: &LeaseClaim) -> HandsResult<Option<HandBox>> {
        let claimed = sqlx::query_as::<_, HandBox>(
            r#"
            update hand_boxes
            set state = 'leased',
                lease_id = $2,
                thread_key = $3,
                principal = $4,
                requester = $5,
                leased_at = now(),
                lease_expires_at = $6,
                updated_at = now()
            where id = $1 and state in ('warming', 'ready')
            returning *
            "#,
        )
        .bind(id)
        .bind(&claim.lease_id)
        .bind(&claim.thread_key)
        .bind(&claim.principal)
        .bind(&claim.requester)
        .bind(claim.expires_at)
        .fetch_optional(&self.pool)
        .await;
        match claimed {
            Ok(row) => Ok(row),
            Err(error) if is_unique_violation(&error) => {
                self.find_thread_lease(&claim.thread_key).await
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn find_thread_lease(&self, thread_key: &str) -> HandsResult<Option<HandBox>> {
        Ok(sqlx::query_as::<_, HandBox>(
            r#"
            select * from hand_boxes
            where thread_key = $1 and state = 'leased'
            order by leased_at desc nulls last
            limit 1
            "#,
        )
        .bind(thread_key)
        .fetch_optional(&self.pool)
        .await?)
    }

    async fn find_by_lease(&self, lease_id: &str) -> HandsResult<Option<HandBox>> {
        Ok(
            sqlx::query_as::<_, HandBox>("select * from hand_boxes where lease_id = $1 limit 1")
                .bind(lease_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    async fn update_binding(
        &self,
        lease_id: &str,
        thread_key: &str,
        principal: &str,
        requester: Option<&str>,
    ) -> HandsResult<Option<HandBox>> {
        Ok(sqlx::query_as::<_, HandBox>(
            r#"
            update hand_boxes
            set principal = $3, requester = $4, updated_at = now()
            where lease_id = $1 and thread_key = $2 and state = 'leased'
            returning *
            "#,
        )
        .bind(lease_id)
        .bind(thread_key)
        .bind(principal)
        .bind(requester)
        .fetch_optional(&self.pool)
        .await?)
    }

    async fn renew_lease(
        &self,
        lease_id: &str,
        thread_key: &str,
        principal: &str,
        requester: Option<&str>,
        expires_at: OffsetDateTime,
    ) -> HandsResult<Option<HandBox>> {
        Ok(sqlx::query_as::<_, HandBox>(
            r#"
            update hand_boxes
            set principal = $3,
                requester = $4,
                lease_expires_at = $5,
                updated_at = now()
            where lease_id = $1
              and thread_key = $2
              and state = 'leased'
              and lease_expires_at > now()
            returning *
            "#,
        )
        .bind(lease_id)
        .bind(thread_key)
        .bind(principal)
        .bind(requester)
        .bind(expires_at)
        .fetch_optional(&self.pool)
        .await?)
    }

    async fn expire_lease(&self, id: &str, now: OffsetDateTime) -> HandsResult<Option<HandBox>> {
        Ok(sqlx::query_as::<_, HandBox>(
            r#"
            update hand_boxes
            set state = 'deleting', updated_at = now()
            where id = $1 and state = 'leased' and lease_expires_at <= $2
            returning *
            "#,
        )
        .bind(id)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?)
    }

    async fn discard_lease(&self, id: &str, lease_id: &str) -> HandsResult<Option<HandBox>> {
        Ok(sqlx::query_as::<_, HandBox>(
            r#"
            update hand_boxes
            set state = 'deleting', updated_at = now()
            where id = $1 and lease_id = $2 and state = 'leased'
            returning *
            "#,
        )
        .bind(id)
        .bind(lease_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    async fn set_deleting(&self, id: &str) -> HandsResult<()> {
        sqlx::query(
            "update hand_boxes set state = 'deleting', updated_at = now() \
             where id = $1 and state in ('warming', 'ready')",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self, id: &str) -> HandsResult<()> {
        sqlx::query("delete from hand_boxes where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn total_hands(&self) -> HandsResult<usize> {
        let count: i64 =
            sqlx::query_scalar("select count(*) from hand_boxes where state <> 'deleting'")
                .fetch_one(&self.pool)
                .await?;
        Ok(count.max(0) as usize)
    }

    async fn count_pool(&self, size: HandSize) -> HandsResult<usize> {
        let count: i64 = sqlx::query_scalar(
            "select count(*) from hand_boxes \
             where size = $1 and state in ('warming', 'ready')",
        )
        .bind(size.as_str())
        .fetch_one(&self.pool)
        .await?;
        Ok(count.max(0) as usize)
    }

    async fn list_expired(&self, now: OffsetDateTime) -> HandsResult<Vec<HandBox>> {
        Ok(sqlx::query_as::<_, HandBox>(
            "select * from hand_boxes \
             where state = 'leased' and lease_expires_at <= $1 order by lease_expires_at",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn list_leased(&self) -> HandsResult<Vec<HandBox>> {
        Ok(sqlx::query_as::<_, HandBox>(
            "select * from hand_boxes where state = 'leased' order by leased_at",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    async fn list_stuck_warming(&self, older_than: OffsetDateTime) -> HandsResult<Vec<HandBox>> {
        Ok(sqlx::query_as::<_, HandBox>(
            "select * from hand_boxes \
             where state = 'warming' and updated_at < $1 order by updated_at",
        )
        .bind(older_than)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn list_deleting(&self, older_than: OffsetDateTime) -> HandsResult<Vec<HandBox>> {
        Ok(sqlx::query_as::<_, HandBox>(
            "select * from hand_boxes \
             where state = 'deleting' and updated_at < $1 order by updated_at",
        )
        .bind(older_than)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn live_ids(&self) -> HandsResult<BTreeSet<String>> {
        let ids: Vec<String> = sqlx::query_scalar("select id from hand_boxes")
            .fetch_all(&self.pool)
            .await?;
        Ok(ids.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use centaur_session_sqlx::{PgSessionStore, TextSearchBackend};
    use sqlx::postgres::PgPoolOptions;

    use super::*;
    use crate::model::new_lease_id;

    /// Postgres stores microseconds; truncate so round-tripped timestamps
    /// compare equal.
    fn now_micros() -> OffsetDateTime {
        let now = OffsetDateTime::now_utc();
        now.replace_nanosecond(now.nanosecond() / 1_000 * 1_000)
            .expect("a truncated nanosecond is valid")
    }

    /// Migrations take an advisory lock; concurrent tests running them on one
    /// database deadlock, so run them once per process.
    static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

    /// Store tests claim from one shared table, so they run one at a time on
    /// an emptied table. Use a dedicated test database.
    static EXCLUSIVE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn test_store() -> Option<(PgHandStore, tokio::sync::MutexGuard<'static, ()>)> {
        let url = std::env::var("HANDS_TEST_DATABASE_URL")
            .or_else(|_| std::env::var("SESSION_RUNTIME_TEST_DATABASE_URL"))
            .or_else(|_| std::env::var("SESSION_SQLX_TEST_DATABASE_URL"))
            .ok()?;
        let session = PgSessionStore::new(
            PgPoolOptions::new()
                .connect(&url)
                .await
                .expect("connect hand test database"),
        );
        MIGRATED
            .get_or_init(|| async {
                session
                    .run_migrations(TextSearchBackend::Postgres)
                    .await
                    .expect("run session migrations");
            })
            .await;
        let guard = EXCLUSIVE.lock().await;
        sqlx::query("delete from hand_boxes")
            .execute(session.pool())
            .await
            .expect("empty hand_boxes");
        Some((PgHandStore::new(session.pool().clone()), guard))
    }

    fn claim(thread: &str, now: OffsetDateTime) -> LeaseClaim {
        LeaseClaim {
            lease_id: new_lease_id(),
            thread_key: thread.to_owned(),
            principal: "prn_hand_test".to_owned(),
            requester: Some("prn_requester".to_owned()),
            expires_at: now + time::Duration::seconds(60),
        }
    }

    #[tokio::test]
    async fn claim_skips_locked_and_wrong_size_rows_and_is_idempotent() {
        let Some((store, _exclusive)) = test_store().await else {
            eprintln!("skipping: set HANDS_TEST_DATABASE_URL to a Postgres URL");
            return;
        };
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let size = HandSize::Default;
        let thread = format!("test:hands:{suffix}");
        let ready_id = format!("hnd_{}", &suffix[..10]);
        let large_id = format!("hnd_{}", &suffix[10..20]);
        store
            .insert_warming(&ready_id, size, "hand-test-ready", None, "prx_ready")
            .await
            .expect("insert ready");
        store
            .mark_ready(&ready_id, "10.0.0.9")
            .await
            .expect("mark ready");
        store
            .insert_warming(
                &large_id,
                HandSize::Large,
                "hand-test-large",
                None,
                "prx_large",
            )
            .await
            .expect("insert large");
        store
            .mark_ready(&large_id, "10.0.0.10")
            .await
            .expect("mark large ready");

        let now = now_micros();
        let first = claim(&thread, now);
        let leased = store
            .claim_ready(size, &first)
            .await
            .expect("claim ready")
            .expect("a default hand");
        assert_eq!(leased.id, ready_id);
        assert_eq!(leased.lease_id.as_deref(), Some(first.lease_id.as_str()));
        assert_eq!(leased.pod_ip.as_deref(), Some("10.0.0.9"));

        let other_thread = format!("test:hands:other:{suffix}");
        let second = claim(&other_thread, now);
        assert!(
            store
                .claim_ready(size, &second)
                .await
                .expect("claim again")
                .is_none(),
            "a leased hand is not ready"
        );

        let found = store
            .find_thread_lease(&thread)
            .await
            .expect("find lease")
            .expect("live lease");
        assert_eq!(found.id, ready_id);

        let renewed_at = now + time::Duration::seconds(120);
        let renewed = store
            .renew_lease(&first.lease_id, &thread, "prn_hand_test", None, renewed_at)
            .await
            .expect("renew")
            .expect("renewed row");
        assert_eq!(renewed.lease_expires_at, Some(renewed_at));
        assert_eq!(renewed.requester, None);

        store.set_deleting(&ready_id).await.expect("mark deleting");
        store.delete(&ready_id).await.expect("delete");
        assert!(
            store
                .find_by_lease(&first.lease_id)
                .await
                .expect("find deleted")
                .is_none()
        );
        store.delete(&large_id).await.expect("delete large");
    }

    #[tokio::test]
    async fn mark_ready_is_idempotent_and_a_duplicate_thread_claim_returns_the_live_lease() {
        let Some((store, _exclusive)) = test_store().await else {
            eprintln!("skipping: set HANDS_TEST_DATABASE_URL to a Postgres URL");
            return;
        };
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let thread = format!("test:hands:duplicate:{suffix}");
        let first_id = format!("hnd_{}", &suffix[..10]);
        let second_id = format!("hnd_{}", &suffix[10..20]);
        let size = HandSize::Default;
        for id in [&first_id, &second_id] {
            store
                .insert_warming(id, size, "hand-test-duplicate", None, "prx_duplicate")
                .await
                .expect("insert warming");
            store
                .mark_ready(id, "10.0.0.21")
                .await
                .expect("mark ready")
                .expect("warming row");
        }
        // The replenisher marks the same hand ready again before checkout.
        let again = store
            .mark_ready(&first_id, "10.0.0.22")
            .await
            .expect("mark ready again")
            .expect("ready row is idempotent");
        assert_eq!(again.id, first_id);
        assert_eq!(again.state, "ready");
        assert_eq!(again.pod_ip.as_deref(), Some("10.0.0.21"));

        let now = now_micros();
        let first = claim(&thread, now);
        let leased = store
            .claim_ready(size, &first)
            .await
            .expect("first claim")
            .expect("a hand");
        // A concurrent checkout for the same thread races past the pool's
        // find-then-claim check and hits the partial unique index.
        let duplicate = claim(&thread, now);
        let existing = store
            .claim_ready(size, &duplicate)
            .await
            .expect("duplicate claim returns the live lease")
            .expect("the thread's existing lease");
        assert_eq!(existing.id, leased.id);
        assert_eq!(existing.lease_id.as_deref(), Some(first.lease_id.as_str()));

        store.delete(&first_id).await.expect("delete first");
        store.delete(&second_id).await.expect("delete second");
    }
}

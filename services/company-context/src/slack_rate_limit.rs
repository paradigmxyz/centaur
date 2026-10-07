//! Paces Slack Web API calls against the Slack app's rate limits.
//!
//! Slack limits each method per workspace per app and reports no remaining
//! quota, only a 429 with Retry-After. Every token the app issues in a
//! workspace, including bot tokens used by other services, shares one budget
//! per method. Workers therefore reserve send slots from a shared schedule in
//! Postgres, spaced by a configured share of the method's documented tier.
//! A rate limit blocks the method for every worker until Retry-After passes
//! and widens the spacing, which relaxes again while no rate limits occur.

use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use crate::{slack::SlackMethod, telemetry};

const MAX_BACKOFF: f64 = 16.0;
/// How long a method must go without a rate limit before its backoff halves.
const BACKOFF_RECOVERY: chrono::Duration = chrono::Duration::minutes(10);

#[derive(Clone)]
pub struct RateLimiter {
    pool: PgPool,
    app_slug: String,
    share: f64,
}

#[derive(Clone, Debug, PartialEq)]
struct Bucket {
    next_slot_at: DateTime<Utc>,
    blocked_until: Option<DateTime<Utc>>,
    backoff: f64,
    backoff_adjusted_at: DateTime<Utc>,
}

impl Bucket {
    /// Grants the earliest send slot at `now`, returning it with the bucket
    /// state after the grant.
    fn grant(&self, now: DateTime<Utc>, interval: chrono::Duration) -> (DateTime<Utc>, Self) {
        let mut next = self.clone();
        if next.backoff > 1.0 && now - next.backoff_adjusted_at >= BACKOFF_RECOVERY {
            next.backoff = (next.backoff / 2.0).max(1.0);
            next.backoff_adjusted_at = now;
        }
        let slot = now
            .max(self.next_slot_at)
            .max(self.blocked_until.unwrap_or(now));
        next.next_slot_at = slot + scale(interval, next.backoff);
        (slot, next)
    }

    fn rate_limited(&self, now: DateTime<Utc>, retry_after: chrono::Duration) -> Self {
        let blocked_until = now + retry_after;
        Self {
            next_slot_at: self.next_slot_at,
            blocked_until: Some(
                self.blocked_until
                    .map_or(blocked_until, |current| current.max(blocked_until)),
            ),
            backoff: (self.backoff * 2.0).min(MAX_BACKOFF),
            backoff_adjusted_at: now,
        }
    }
}

fn scale(interval: chrono::Duration, factor: f64) -> chrono::Duration {
    chrono::Duration::microseconds(
        (interval.num_microseconds().unwrap_or(i64::MAX) as f64 * factor) as i64,
    )
}

impl RateLimiter {
    pub fn new(pool: PgPool, app_slug: String, share: f64) -> Self {
        Self {
            pool,
            app_slug,
            share,
        }
    }

    /// Reserves the next send slot for the method and returns how long the
    /// caller must wait before sending.
    pub async fn reserve(&self, team_id: &str, method: SlackMethod) -> Result<Duration> {
        let interval = chrono::Duration::microseconds(
            (60_000_000.0 / (method.tier_per_minute() * self.share)) as i64,
        );
        let mut tx = self.pool.begin().await?;
        let (bucket, now) = self.lock(&mut tx, team_id, method).await?;
        let (slot, next) = bucket.grant(now, interval);
        self.store(&mut tx, team_id, method, &next, false).await?;
        tx.commit().await?;
        let wait = (slot - now).to_std().unwrap_or_default();
        metrics::histogram!(telemetry::SLACK_RATE_LIMIT_WAIT, "method" => method.name())
            .record(wait.as_secs_f64());
        Ok(wait)
    }

    /// Blocks the method for every worker until Slack's Retry-After passes.
    pub async fn rate_limited(
        &self,
        team_id: &str,
        method: SlackMethod,
        retry_after: Duration,
    ) -> Result<()> {
        let retry_after = chrono::Duration::from_std(retry_after)?;
        let mut tx = self.pool.begin().await?;
        let (bucket, now) = self.lock(&mut tx, team_id, method).await?;
        self.store(
            &mut tx,
            team_id,
            method,
            &bucket.rate_limited(now, retry_after),
            true,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Locks the method's bucket, creating it if needed, and returns it with
    /// the database clock so every replica schedules against one clock.
    async fn lock(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        team_id: &str,
        method: SlackMethod,
    ) -> Result<(Bucket, DateTime<Utc>)> {
        sqlx::query(
            r#"
            INSERT INTO company_context_system.slack_rate_limits (app_slug, team_id, method)
            VALUES ($1, $2, $3)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(&self.app_slug)
        .bind(team_id)
        .bind(method.name())
        .execute(&mut **tx)
        .await?;
        let row = sqlx::query(
            r#"
            SELECT next_slot_at, blocked_until, backoff, backoff_adjusted_at,
                   clock_timestamp() AS now
            FROM company_context_system.slack_rate_limits
            WHERE app_slug = $1 AND team_id = $2 AND method = $3
            FOR UPDATE
            "#,
        )
        .bind(&self.app_slug)
        .bind(team_id)
        .bind(method.name())
        .fetch_one(&mut **tx)
        .await?;
        Ok((
            Bucket {
                next_slot_at: row.try_get("next_slot_at")?,
                blocked_until: row.try_get("blocked_until")?,
                backoff: row.try_get("backoff")?,
                backoff_adjusted_at: row.try_get("backoff_adjusted_at")?,
            },
            row.try_get("now")?,
        ))
    }

    async fn store(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        team_id: &str,
        method: SlackMethod,
        bucket: &Bucket,
        rate_limited: bool,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE company_context_system.slack_rate_limits
            SET next_slot_at = $4,
                blocked_until = $5,
                backoff = $6,
                backoff_adjusted_at = $7,
                last_rate_limited_at = CASE WHEN $8 THEN NOW() ELSE last_rate_limited_at END,
                updated_at = NOW()
            WHERE app_slug = $1 AND team_id = $2 AND method = $3
            "#,
        )
        .bind(&self.app_slug)
        .bind(team_id)
        .bind(method.name())
        .bind(bucket.next_slot_at)
        .bind(bucket.blocked_until)
        .bind(bucket.backoff)
        .bind(bucket.backoff_adjusted_at)
        .bind(rate_limited)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use chrono::TimeZone;

    use super::*;
    use crate::test_support::TestDatabase;

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + seconds, 0).unwrap()
    }

    fn bucket() -> Bucket {
        Bucket {
            next_slot_at: at(0),
            blocked_until: None,
            backoff: 1.0,
            backoff_adjusted_at: at(0),
        }
    }

    #[test]
    fn grants_are_spaced_by_the_interval() {
        let interval = chrono::Duration::seconds(4);
        let (first, state) = bucket().grant(at(10), interval);
        let (second, state) = state.grant(at(10), interval);
        let (third, _) = state.grant(at(11), interval);
        assert_eq!([first, second, third], [at(10), at(14), at(18)]);
        // An idle bucket does not bank slots for a later burst.
        let (late, _) = bucket().grant(at(100), interval);
        assert_eq!(late, at(100));
    }

    #[test]
    fn rate_limits_block_widen_and_then_relax() {
        let interval = chrono::Duration::seconds(4);
        let limited = bucket().rate_limited(at(10), chrono::Duration::seconds(30));
        assert_eq!(limited.blocked_until, Some(at(40)));
        assert_eq!(limited.backoff, 2.0);

        let (slot, next) = limited.grant(at(11), interval);
        assert_eq!(slot, at(40));
        assert_eq!(next.next_slot_at, at(48));

        // A shorter Retry-After never shortens an existing block.
        let again = limited.rate_limited(at(12), chrono::Duration::seconds(5));
        assert_eq!(again.blocked_until, Some(at(40)));
        assert_eq!(again.backoff, 4.0);

        let quiet = at(12) + BACKOFF_RECOVERY;
        let (_, relaxed) = again.grant(quiet, interval);
        assert_eq!(relaxed.backoff, 2.0);
        assert_eq!(relaxed.next_slot_at, quiet + chrono::Duration::seconds(8));

        let mut capped = bucket();
        for _ in 0..10 {
            capped = capped.rate_limited(at(0), chrono::Duration::seconds(1));
        }
        assert_eq!(capped.backoff, MAX_BACKOFF);
    }

    #[tokio::test]
    async fn concurrent_reservations_share_one_schedule() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "slack_rate_limit").await;
        // Tier 3 at a 0.3 share spaces requests four seconds apart.
        let limiter = RateLimiter::new(database.pool.clone(), "slack".to_owned(), 0.3);
        let other_replica = RateLimiter::new(database.pool.clone(), "slack".to_owned(), 0.3);

        let mut waits = Vec::new();
        let reservations = (0..5).map(|index| {
            let limiter = if index % 2 == 0 {
                &limiter
            } else {
                &other_replica
            };
            limiter.reserve("T1", SlackMethod::UsersConversations)
        });
        for wait in futures_util::future::join_all(reservations).await {
            waits.push(wait.unwrap().as_secs_f64());
        }
        waits.sort_by(f64::total_cmp);
        for (index, wait) in waits.iter().enumerate() {
            let expected = index as f64 * 4.0;
            assert!((wait - expected).abs() < 1.0, "wait {wait} != {expected}");
        }

        // Another workspace has its own budget.
        let other_team = limiter
            .reserve("T2", SlackMethod::UsersConversations)
            .await
            .unwrap();
        assert!(other_team < Duration::from_secs(1));

        // A rate limit seen by one replica blocks the other.
        limiter
            .rate_limited(
                "T2",
                SlackMethod::UsersConversations,
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        let blocked = other_replica
            .reserve("T2", SlackMethod::UsersConversations)
            .await
            .unwrap();
        assert!(blocked > Duration::from_secs(29), "blocked for {blocked:?}");

        database.drop().await;
    }
}

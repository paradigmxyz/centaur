//! The refresh loop. Each tick re-derives the due credentials from the
//! database, so a missed tick or a restart is caught up by the next one.

use std::{collections::BTreeMap, sync::Arc, time::Instant};

use active_record_encryption::ActiveRecordEncryption;
use chrono::{NaiveDateTime, TimeDelta, Utc};
use futures_util::{StreamExt, stream};
use sqlx::PgPool;
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::{
    client::TokenClient,
    grants::{self, Outcome},
    identifiers::oid,
    schedule,
    store::{self, Error, RefreshTokenChange, Refreshed},
    telemetry,
};

#[derive(Clone)]
pub(crate) struct Broker {
    pub pool: PgPool,
    pub encryption: Arc<ActiveRecordEncryption>,
    pub client: TokenClient,
    pub concurrency: usize,
}

impl Broker {
    /// Refreshes every due credential, then returns. Stops starting new
    /// refreshes once `shutdown` is set; refreshes already running finish so
    /// a token the endpoint has just rotated is not lost.
    pub async fn tick(&self, shutdown: &watch::Receiver<bool>) -> Result<(), sqlx::Error> {
        // The gauges are only for observability; failing to count must not
        // hold up refreshes.
        match store::credential_counts(&self.pool).await {
            Ok(rows) => {
                let mut counts = BTreeMap::<String, (i64, i64)>::new();
                for row in rows {
                    let provider =
                        grants::provider_label(row.provider.as_deref(), &row.token_endpoint);
                    let count = counts.entry(provider).or_default();
                    count.0 += row.dead;
                    count.1 += row.backing_off;
                }
                telemetry::record_credential_counts(&counts);
            }
            Err(error) => warn!(%error, "counting broker credentials for metrics failed"),
        }
        let due = store::due_credential_ids(&self.pool, Utc::now().naive_utc()).await?;
        telemetry::record_due_credentials(due.len());
        stream::iter(due)
            .take_while(|_| std::future::ready(!*shutdown.borrow()))
            .for_each_concurrent(self.concurrency, |id| self.refresh(id))
            .await;
        telemetry::record_tick_completed();
        Ok(())
    }

    pub async fn refresh(&self, id: i64) {
        let started = Instant::now();
        match self.try_refresh(id).await {
            Ok(Some((provider, grant, outcome))) => {
                telemetry::record_refresh(&provider, &grant, outcome, started)
            }
            Ok(None) => {}
            Err(error) => {
                error!(credential_id = %oid("bcr", id), %error, "broker credential refresh failed");
                telemetry::record_refresh("unknown", "unknown", "error", started);
            }
        }
    }

    /// One refresh attempt under the credential's row lock. Endpoint and
    /// configuration failures are recorded in the row; the error is only for
    /// database and decryption failures, which leave the row unchanged.
    async fn try_refresh(&self, id: i64) -> Result<Option<(String, String, &'static str)>, Error> {
        let now = Utc::now().naive_utc();
        let mut transaction = self.pool.begin().await?;
        let Some(due) =
            store::lock_due_credential(&mut transaction, &self.encryption, id, now).await?
        else {
            return Ok(None);
        };
        let credential = &due.credential;
        let outcome = match grants::refresh(credential, &self.client).await {
            Ok(Outcome::Refreshed {
                token,
                clear_refresh_token,
            }) => {
                let expires_in = schedule::expires_in(token.expires_in);
                let expires_at = now + TimeDelta::seconds(expires_in);
                let refresh_token = match token.refresh_token {
                    Some(rotated) => RefreshTokenChange::Replace(rotated),
                    None if clear_refresh_token => RefreshTokenChange::Clear,
                    None => RefreshTokenChange::Keep,
                };
                store::record_refresh(
                    &mut transaction,
                    &self.encryption,
                    id,
                    now,
                    Refreshed {
                        access_token: &token.access_token,
                        refresh_token,
                        expires_at,
                        next_attempt_at: schedule::next_attempt_at(now, expires_in, &due.schedule),
                    },
                )
                .await?;
                info!(credential_id = %credential.oid, %expires_at, "broker credential refreshed");
                "success"
            }
            Ok(Outcome::Dead(reason)) => {
                mark_dead(&mut transaction, id, &credential.oid, now, reason).await?
            }
            Err(error) if !error.retryable => {
                mark_dead(&mut transaction, id, &credential.oid, now, error.reason()).await?
            }
            Err(error) => {
                let failures = due.failure_count + 1;
                store::record_retry(
                    &mut transaction,
                    id,
                    now,
                    failures,
                    schedule::retry_at(now, failures),
                )
                .await?;
                warn!(
                    credential_id = %credential.oid,
                    attempt = failures,
                    stage = error.stage,
                    code = error.code.as_deref(),
                    status = error.status,
                    "broker credential refresh failed; retrying"
                );
                "retry"
            }
        };
        transaction.commit().await?;
        Ok(Some((
            credential.provider_label(),
            credential.grant.clone(),
            outcome,
        )))
    }
}

async fn mark_dead(
    transaction: &mut sqlx::PgConnection,
    id: i64,
    credential_oid: &str,
    now: NaiveDateTime,
    reason: &str,
) -> Result<&'static str, Error> {
    store::record_dead(transaction, id, now, reason).await?;
    error!(
        credential_id = credential_oid,
        reason, "broker credential marked dead; re-authorization required"
    );
    Ok("dead")
}

//! Which nanocode session holds each Centaur thread.
//!
//! A thread gets a fresh UUIDv7 session id the first time it appears. With a
//! database the assignment lives in `nanocode.thread_sessions`, so every brain
//! process and restart resolves a thread to the same session; without one it
//! lives in memory. A thread's session never changes, so lookups are cached.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use sqlx::PgPool;

#[derive(Clone)]
pub struct SessionIds {
    store: Option<PgPool>,
    cache: Arc<Mutex<HashMap<String, String>>>,
}

impl SessionIds {
    /// Lazily connected: the brain serves health checks while its database is
    /// still coming up.
    pub fn new(database_url: Option<&str>) -> Result<Self, sqlx::Error> {
        Ok(Self {
            store: database_url.map(PgPool::connect_lazy).transpose()?,
            cache: Arc::default(),
        })
    }

    /// The thread's session id, assigning one if the thread is new.
    pub async fn resolve(&self, thread_key: &str) -> Result<String, sqlx::Error> {
        if let Some(session_id) = self.cached(thread_key) {
            return Ok(session_id);
        }
        let fresh = uuid::Uuid::now_v7().to_string();
        let session_id = match &self.store {
            Some(pool) => {
                // Concurrent first turns race to insert; every one then reads
                // the winner.
                sqlx::query(
                    "insert into nanocode.thread_sessions (thread_key, session_id) \
                     values ($1, $2) on conflict (thread_key) do nothing",
                )
                .bind(thread_key)
                .bind(&fresh)
                .execute(pool)
                .await?;
                sqlx::query_scalar(
                    "select session_id from nanocode.thread_sessions where thread_key = $1",
                )
                .bind(thread_key)
                .fetch_one(pool)
                .await?
            }
            None => fresh,
        };
        Ok(self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(thread_key.to_owned())
            .or_insert(session_id)
            .clone())
    }

    fn cached(&self, thread_key: &str) -> Option<String> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(thread_key)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_threads_keep_one_v7_session() {
        let ids = SessionIds::new(None).unwrap();
        let first = ids.resolve("slack:T:C:1").await.unwrap();
        assert_eq!(ids.resolve("slack:T:C:1").await.unwrap(), first);
        assert_ne!(ids.resolve("slack:T:C:2").await.unwrap(), first);
        assert_eq!(uuid::Uuid::parse_str(&first).unwrap().get_version_num(), 7);
    }
}

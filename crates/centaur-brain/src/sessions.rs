//! Which nanocode session holds each Centaur thread, and the custom system
//! prompt the thread runs with.
//!
//! A thread gets a fresh UUIDv7 session id the first time it appears. Its
//! first turn pins the custom system prompt it was submitted with (empty for
//! none), and the thread keeps that prompt for life: a Console edit applies to
//! new threads. With a database both live in `nanocode.thread_sessions`, so
//! every brain process and restart resolves a thread to the same session and
//! prompt; without one they live in memory. Neither changes once set, so
//! lookups are cached.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use sqlx::PgPool;

/// A thread's session id and its pinned custom system prompt, `None` until
/// its first turn pins one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub session_id: String,
    pub system_prompt: Option<String>,
}

#[derive(Clone)]
pub struct SessionIds {
    store: Option<PgPool>,
    cache: Arc<Mutex<HashMap<String, Resolved>>>,
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

    /// The thread's session id and pinned prompt, assigning a session if the
    /// thread is new. It pins no prompt.
    pub async fn resolve(&self, thread_key: &str) -> Result<Resolved, sqlx::Error> {
        // An unpinned thread is looked up again, since its first turn may
        // pin a prompt at any moment.
        if let Some(resolved) = self.cached(thread_key)
            && (resolved.system_prompt.is_some() || self.store.is_none())
        {
            return Ok(resolved);
        }
        let fresh = uuid::Uuid::now_v7().to_string();
        let resolved = match &self.store {
            Some(pool) => {
                // Concurrent first requests race to insert; every one then
                // reads the winner.
                sqlx::query(
                    "insert into nanocode.thread_sessions (thread_key, session_id) \
                     values ($1, $2) on conflict (thread_key) do nothing",
                )
                .bind(thread_key)
                .bind(&fresh)
                .execute(pool)
                .await?;
                let (session_id, system_prompt) = sqlx::query_as(
                    "select session_id, system_prompt from nanocode.thread_sessions \
                     where thread_key = $1",
                )
                .bind(thread_key)
                .fetch_one(pool)
                .await?;
                Resolved {
                    session_id,
                    system_prompt,
                }
            }
            None => Resolved {
                session_id: fresh,
                system_prompt: None,
            },
        };
        Ok(self.remember(thread_key, resolved))
    }

    /// Like [`Self::resolve`], but pins `system_prompt` (`None` pins none) if
    /// the thread has no prompt yet. A pinned prompt never changes.
    pub async fn pin(
        &self,
        thread_key: &str,
        system_prompt: Option<&str>,
    ) -> Result<Resolved, sqlx::Error> {
        if let Some(resolved) = self.cached(thread_key)
            && resolved.system_prompt.is_some()
        {
            return Ok(resolved);
        }
        let wanted = system_prompt.unwrap_or_default().to_owned();
        let resolved = match &self.store {
            Some(pool) => {
                let (session_id, system_prompt) = sqlx::query_as(
                    "insert into nanocode.thread_sessions (thread_key, session_id, system_prompt) \
                     values ($1, $2, $3) on conflict (thread_key) do update \
                     set system_prompt = coalesce(thread_sessions.system_prompt, excluded.system_prompt) \
                     returning session_id, system_prompt",
                )
                .bind(thread_key)
                .bind(uuid::Uuid::now_v7().to_string())
                .bind(&wanted)
                .fetch_one(pool)
                .await?;
                Resolved {
                    session_id,
                    system_prompt,
                }
            }
            None => {
                let session_id = match self.cached(thread_key) {
                    Some(resolved) => resolved.session_id,
                    None => uuid::Uuid::now_v7().to_string(),
                };
                Resolved {
                    session_id,
                    system_prompt: Some(wanted),
                }
            }
        };
        Ok(self.remember(thread_key, resolved))
    }

    fn cached(&self, thread_key: &str) -> Option<Resolved> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(thread_key)
            .cloned()
    }

    /// Cache what was resolved, keeping a prompt another request pinned
    /// first, and return the cached value.
    fn remember(&self, thread_key: &str, resolved: Resolved) -> Resolved {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = cache
            .entry(thread_key.to_owned())
            .or_insert(resolved.clone());
        if entry.system_prompt.is_none() {
            entry.system_prompt = resolved.system_prompt;
        }
        entry.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_threads_keep_one_v7_session() {
        let ids = SessionIds::new(None).unwrap();
        let first = ids.resolve("slack:T:C:1").await.unwrap().session_id;
        assert_eq!(ids.resolve("slack:T:C:1").await.unwrap().session_id, first);
        assert_ne!(ids.resolve("slack:T:C:2").await.unwrap().session_id, first);
        assert_eq!(uuid::Uuid::parse_str(&first).unwrap().get_version_num(), 7);
    }

    #[tokio::test]
    async fn the_first_turn_pins_the_prompt_for_life() {
        let ids = SessionIds::new(None).unwrap();
        let unpinned = ids.resolve("slack:T:C:1").await.unwrap();
        assert_eq!(unpinned.system_prompt, None);

        let pinned = ids.pin("slack:T:C:1", Some("pirate")).await.unwrap();
        assert_eq!(pinned.session_id, unpinned.session_id);
        assert_eq!(pinned.system_prompt.as_deref(), Some("pirate"));
        for later in [Some("poet"), None] {
            assert_eq!(ids.pin("slack:T:C:1", later).await.unwrap(), pinned);
        }
        assert_eq!(ids.resolve("slack:T:C:1").await.unwrap(), pinned);

        // No prompt pins none.
        let none = ids.pin("slack:T:C:2", None).await.unwrap();
        assert_eq!(none.system_prompt.as_deref(), Some(""));
        assert_eq!(ids.pin("slack:T:C:2", Some("pirate")).await.unwrap(), none);
    }
}

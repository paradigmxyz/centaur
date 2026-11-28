use std::{collections::BTreeMap, sync::Arc, time::Duration};

use time::OffsetDateTime;
use tokio::time::{MissedTickBehavior, interval, sleep};
use tracing::{info, warn};

use crate::{
    k8s::{HandInfra, HandObservation},
    model::{HandBox, HandLease, HandSize, LeaseClaim, LeaseRequest, new_hand_id, new_lease_id},
    store::{HandStore, HandsError, HandsResult},
};

const READY_POLL_INTERVAL: Duration = Duration::from_millis(200);
/// A deleting row this old means a release died between marking and deleting.
const STALE_DELETING_ROW_AGE: Duration = Duration::from_secs(60);

/// Pool targets, lifetimes, and sweep cadence.
#[derive(Clone, Debug)]
pub struct HandPoolConfig {
    pub pool_sizes: BTreeMap<HandSize, usize>,
    pub replenish_interval: Duration,
    pub sweep_interval: Duration,
    pub lease_duration: Duration,
    pub cold_create_timeout: Duration,
    pub warming_timeout: Duration,
    pub orphan_grace: Duration,
    /// Total warming + ready + leased hands. 0 means no cap. This is a soft
    /// cap: capacity is checked before a cold create, so concurrent checkouts
    /// can briefly overshoot it. Kubernetes remains the hard boundary.
    pub max_hands: usize,
}

/// Warm-pool replenishment, lease transitions, and orphan cleanup.
pub struct HandPool {
    store: Arc<dyn HandStore>,
    infra: Arc<dyn HandInfra>,
    config: HandPoolConfig,
}

impl HandPool {
    #[must_use]
    pub fn new(
        store: Arc<dyn HandStore>,
        infra: Arc<dyn HandInfra>,
        config: HandPoolConfig,
    ) -> Self {
        Self {
            store,
            infra,
            config,
        }
    }

    /// Start the per-size replenisher and the lease/orphan sweeper.
    pub fn spawn_background(self: Arc<Self>) {
        let replenisher = self.clone();
        tokio::spawn(async move {
            let mut tick = interval(replenisher.config.replenish_interval);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if let Err(error) = replenisher.replenish_once().await {
                    warn!(%error, "hand pool replenishment failed");
                }
            }
        });

        tokio::spawn(async move {
            let mut tick = interval(self.config.sweep_interval);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if let Err(error) = self.sweep_once().await {
                    warn!(%error, "hand pool sweep failed");
                }
            }
        });
    }

    /// Lease a hand to the token's thread. Idempotent per thread: an existing
    /// live lease is returned, rebound only if the token's identity changed.
    /// A live lease whose pod or Secret vanished is destroyed, then replaced
    /// with a fresh claim.
    pub async fn lease(&self, request: LeaseRequest<'_>) -> HandsResult<HandLease> {
        let now = OffsetDateTime::now_utc();
        if let Some(existing) = self.store.find_thread_lease(request.thread_key).await? {
            if existing.is_live_lease(now) {
                if let Some(lease) = self
                    .resume_existing(existing, request.principal, request.requester)
                    .await?
                {
                    return Ok(lease);
                }
            } else {
                self.destroy_expired(&existing, now).await?;
            }
        }

        let size = request.size.unwrap_or(HandSize::Default);
        let claim = LeaseClaim {
            lease_id: new_lease_id(),
            thread_key: request.thread_key.to_owned(),
            principal: request.principal.to_owned(),
            requester: request.requester.map(ToOwned::to_owned),
            expires_at: now + self.config.lease_duration,
        };

        if let Some(hand) = self.store.claim_ready(size, &claim).await? {
            match self
                .bind_claimed(hand, request.principal, request.requester)
                .await
            {
                Ok(lease) => return Ok(lease),
                Err(error) => return Err(error),
            }
        }

        if !self.has_capacity().await? {
            return Err(HandsError::Capacity);
        }
        let warming = self.create_warming(size).await?;
        let ready = match self.wait_until_ready(&warming).await {
            Ok(hand) => hand,
            Err(error) => {
                self.destroy(&warming).await;
                return Err(error);
            }
        };
        let Some(hand) = self.store.lease_specific(&ready.id, &claim).await? else {
            self.destroy(&ready).await;
            return Err(HandsError::NotReady(
                "the cold-created hand was claimed before checkout".to_owned(),
            ));
        };
        self.bind_claimed(hand, request.principal, request.requester)
            .await
    }

    /// The thread's live lease, or `None`. An expired lease is deleted and
    /// reported as absent so the brain checks out a fresh hand; a live lease
    /// whose hand is gone is destroyed and also reported absent.
    pub async fn current(&self, thread_key: &str) -> HandsResult<Option<HandLease>> {
        let now = OffsetDateTime::now_utc();
        let Some(hand) = self.store.find_thread_lease(thread_key).await? else {
            return Ok(None);
        };
        if !hand.is_live_lease(now) {
            self.destroy_expired(&hand, now).await?;
            return Ok(None);
        }
        if !self.observe_leased(&hand).await {
            self.discard_leased(&hand).await?;
            return Ok(None);
        }
        match self.render(&hand).await {
            Ok(lease) => Ok(Some(lease)),
            Err(error) => {
                warn!(hand_id = %hand.id, %error, "live hand lease cannot be rendered; replacing it");
                self.discard_leased(&hand).await?;
                Ok(None)
            }
        }
    }

    /// Extend a lease and rebind its proxy when the token's principal or
    /// requester changed.
    pub async fn renew(
        &self,
        lease_id: &str,
        thread_key: &str,
        principal: &str,
        requester: Option<&str>,
    ) -> HandsResult<HandLease> {
        let now = OffsetDateTime::now_utc();
        let hand = self
            .store
            .find_by_lease(lease_id)
            .await?
            .filter(|hand| hand.thread_key.as_deref() == Some(thread_key) && hand.is_leased())
            .ok_or(HandsError::NotFound)?;
        if !hand.is_live_lease(now) {
            self.destroy_expired(&hand, now).await?;
            return Err(HandsError::NotFound);
        }
        if self.binding_changed(&hand, principal, requester)
            && let Err(error) = self.infra.bind_principal(&hand, principal, requester).await
        {
            warn!(hand_id = %hand.id, %error, "rebinding a hand lease failed; replacing it");
            self.discard_leased(&hand).await?;
            return Err(error);
        }
        let expires_at = now + self.config.lease_duration;
        let hand = self
            .store
            .renew_lease(lease_id, thread_key, principal, requester, expires_at)
            .await?
            .ok_or(HandsError::NotFound)?;
        self.render(&hand).await
    }

    /// Release a lease. A missing lease is already released, so this is
    /// idempotent; a lease owned by another thread is not found.
    pub async fn release(&self, lease_id: &str, thread_key: &str) -> HandsResult<()> {
        let Some(hand) = self.store.find_by_lease(lease_id).await? else {
            return Ok(());
        };
        if hand.thread_key.as_deref() != Some(thread_key) {
            return Err(HandsError::NotFound);
        }
        self.discard_leased(&hand).await?;
        Ok(())
    }

    /// Bring each size pool back to its target and reconcile warming hands
    /// whose pods became ready or vanished.
    pub async fn replenish_once(&self) -> HandsResult<()> {
        for hand in self.store.list_warming().await? {
            match self.infra.observe_hand(&hand.id, &hand.pod_name).await {
                Ok(HandObservation::Ready { pod_ip }) => {
                    if let Some(ready) = self.store.mark_ready(&hand.id, &pod_ip).await? {
                        info!(hand_id = %ready.id, size = %ready.size, "hand became ready");
                    }
                }
                Ok(HandObservation::Gone) => {
                    warn!(hand_id = %hand.id, "warming hand pod disappeared");
                    self.destroy(&hand).await;
                }
                Ok(HandObservation::Pending) => {}
                Err(error) => warn!(hand_id = %hand.id, %error, "failed to observe warming hand"),
            }
        }

        for size in HandSize::ALL {
            let target = self.config.pool_sizes.get(&size).copied().unwrap_or(0);
            if target == 0 {
                continue;
            }
            let active = self.store.count_pool(size).await?;
            let mut needed = target.saturating_sub(active);
            if !self.config.max_hands.eq(&0) {
                let total = self.store.total_hands().await?;
                needed = needed.min(self.config.max_hands.saturating_sub(total));
            }
            for _ in 0..needed {
                if let Err(error) = self.create_warming(size).await {
                    warn!(%size, %error, "failed to add a warming hand");
                    break;
                }
            }
        }
        Ok(())
    }

    /// Delete expired leases, stuck warming rows, stale deleting rows, and
    /// hand pods or Secrets with no row.
    pub async fn sweep_once(&self) -> HandsResult<()> {
        let now = OffsetDateTime::now_utc();
        for hand in self.store.list_expired(now).await? {
            info!(hand_id = %hand.id, lease_id = ?hand.lease_id, "deleting expired hand lease");
            // Only a row that is still expired transitions to deleting; a
            // concurrent renew wins and keeps its pod.
            self.destroy_expired(&hand, now).await?;
        }
        for hand in self.store.list_leased().await? {
            if !self.observe_leased(&hand).await {
                warn!(hand_id = %hand.id, lease_id = ?hand.lease_id, "leased hand pod disappeared");
                self.discard_leased(&hand).await?;
            }
        }
        let stuck_before = now - self.config.warming_timeout;
        for hand in self.store.list_stuck_warming(stuck_before).await? {
            warn!(hand_id = %hand.id, "deleting a hand stuck warming");
            self.destroy(&hand).await;
        }
        for hand in self
            .store
            .list_deleting(now - STALE_DELETING_ROW_AGE)
            .await?
        {
            self.finish_delete(&hand).await;
        }
        let live = self.store.live_ids().await?;
        match self
            .infra
            .sweep_orphans(&live, self.config.orphan_grace)
            .await
        {
            Ok(0) => {}
            Ok(deleted) => info!(deleted, "swept orphaned hand pods"),
            Err(error) => warn!(%error, "failed to sweep orphaned hand resources"),
        }
        Ok(())
    }

    async fn create_warming(&self, size: HandSize) -> HandsResult<HandBox> {
        let id = new_hand_id();
        let resources = self.infra.create_hand(&id, size).await?;
        match self
            .store
            .insert_warming(
                &id,
                size,
                &resources.pod_name,
                resources.pod_ip.as_deref(),
                &resources.iron_proxy_id,
            )
            .await
        {
            Ok(hand) => Ok(hand),
            Err(error) => {
                let _ = self
                    .infra
                    .delete_hand(&id, Some(&resources.iron_proxy_id))
                    .await;
                Err(error)
            }
        }
    }

    async fn wait_until_ready(&self, hand: &HandBox) -> HandsResult<HandBox> {
        let deadline = tokio::time::Instant::now() + self.config.cold_create_timeout;
        loop {
            match self.infra.observe_hand(&hand.id, &hand.pod_name).await? {
                HandObservation::Ready { pod_ip } => {
                    return self
                        .store
                        .mark_ready(&hand.id, &pod_ip)
                        .await?
                        .ok_or_else(|| {
                            HandsError::NotReady(format!(
                                "hand {} left the warming state before readiness",
                                hand.id
                            ))
                        });
                }
                HandObservation::Gone => {
                    return Err(HandsError::NotReady(format!(
                        "hand {} pod disappeared before it became ready",
                        hand.id
                    )));
                }
                HandObservation::Pending => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(HandsError::NotReady(format!(
                            "hand {} did not become ready in time",
                            hand.id
                        )));
                    }
                    sleep(READY_POLL_INTERVAL).await;
                }
            }
        }
    }

    async fn bind_claimed(
        &self,
        hand: HandBox,
        principal: &str,
        requester: Option<&str>,
    ) -> HandsResult<HandLease> {
        if let Err(error) = self.infra.bind_principal(&hand, principal, requester).await {
            self.discard_leased(&hand).await?;
            return Err(error);
        }
        self.render(&hand).await
    }

    /// Reuse an existing live lease, but only if its hand is still there and
    /// its token and placeholders can still be read. Any failure makes the
    /// hand unusable, so it is destroyed and the caller falls through to a
    /// fresh claim instead of returning a permanent 500.
    async fn resume_existing(
        &self,
        hand: HandBox,
        principal: &str,
        requester: Option<&str>,
    ) -> HandsResult<Option<HandLease>> {
        let original = hand.clone();
        let hand = match self.rebind_if_needed(hand, principal, requester).await {
            Ok(hand) => hand,
            Err(error) => {
                warn!(hand_id = %original.id, %error, "rebinding the existing hand lease failed; replacing it");
                self.discard_leased(&original).await?;
                return Ok(None);
            }
        };
        if !self.observe_leased(&hand).await {
            self.discard_leased(&hand).await?;
            return Ok(None);
        }
        match self.render(&hand).await {
            Ok(lease) => Ok(Some(lease)),
            Err(error) => {
                warn!(hand_id = %hand.id, %error, "live hand lease cannot be rendered; replacing it");
                self.discard_leased(&hand).await?;
                Ok(None)
            }
        }
    }

    async fn observe_leased(&self, hand: &HandBox) -> bool {
        match self.infra.observe_hand(&hand.id, &hand.pod_name).await {
            Ok(HandObservation::Ready { .. }) => true,
            Ok(observation) => {
                warn!(hand_id = %hand.id, ?observation, "leased hand is not ready");
                false
            }
            Err(error) => {
                warn!(hand_id = %hand.id, %error, "failed to observe leased hand");
                false
            }
        }
    }

    async fn rebind_if_needed(
        &self,
        hand: HandBox,
        principal: &str,
        requester: Option<&str>,
    ) -> HandsResult<HandBox> {
        if !self.binding_changed(&hand, principal, requester) {
            return Ok(hand);
        }
        self.infra
            .bind_principal(&hand, principal, requester)
            .await?;
        let Some(lease_id) = hand.lease_id.as_deref() else {
            return Err(HandsError::NotFound);
        };
        let Some(thread_key) = hand.thread_key.as_deref() else {
            return Err(HandsError::NotFound);
        };
        self.store
            .update_binding(lease_id, thread_key, principal, requester)
            .await?
            .ok_or(HandsError::NotFound)
    }

    fn binding_changed(&self, hand: &HandBox, principal: &str, requester: Option<&str>) -> bool {
        hand.principal.as_deref() != Some(principal) || hand.requester.as_deref() != requester
    }

    async fn render(&self, hand: &HandBox) -> HandsResult<HandLease> {
        let lease_id = hand.lease_id.clone().ok_or(HandsError::NotFound)?;
        let pod_ip = hand
            .pod_ip
            .as_deref()
            .ok_or_else(|| HandsError::NotReady(format!("hand {} has no pod IP", hand.id)))?;
        let expires_at = hand.lease_expires_at.ok_or(HandsError::NotFound)?;
        let principal = hand.principal.as_deref().ok_or(HandsError::NotFound)?;
        let token = self.infra.read_token(&hand.id).await?;
        let env = self.infra.placeholders(principal).await?;
        let host = if pod_ip.contains(':') {
            format!("[{pod_ip}]")
        } else {
            pod_ip.to_owned()
        };
        Ok(HandLease {
            lease_id,
            hand_id: hand.id.clone(),
            size: hand.size(),
            url: format!("ws://{host}:7070"),
            token,
            env,
            expires_at,
        })
    }

    /// A soft cap: check-then-act, not a reservation. Concurrent checkouts can
    /// overshoot `max_hands` by the number of in-flight creates; the sweeper
    /// and the next replenish settle back under it.
    async fn has_capacity(&self) -> HandsResult<bool> {
        if self.config.max_hands == 0 {
            return Ok(true);
        }
        Ok(self.store.total_hands().await? < self.config.max_hands)
    }

    /// Mark a row deleting, remove its pod/Secret/proxy, then drop the row.
    /// Resource deletion is best effort: if it fails, the row still goes away
    /// and the orphan sweeper removes the pod after its grace.
    async fn destroy(&self, hand: &HandBox) {
        if let Err(error) = self.store.set_deleting(&hand.id).await {
            warn!(hand_id = %hand.id, %error, "failed to mark hand deleting");
            return;
        }
        self.finish_delete(hand).await;
    }

    /// Destroy an expired lease only if its row has not been renewed since the
    /// sweeper read it. The conditional transition is what makes renew and
    /// expiry atomic.
    async fn destroy_expired(&self, hand: &HandBox, now: OffsetDateTime) -> HandsResult<()> {
        if let Some(row) = self.store.expire_lease(&hand.id, now).await? {
            self.finish_delete(&row).await;
        }
        Ok(())
    }

    /// Destroy a live lease whose hand became unusable. The lease id is part
    /// of the transition so a replacement lease on the same row cannot be
    /// deleted by a stale caller.
    async fn discard_leased(&self, hand: &HandBox) -> HandsResult<()> {
        let Some(lease_id) = hand.lease_id.as_deref() else {
            self.destroy(hand).await;
            return Ok(());
        };
        if let Some(row) = self.store.discard_lease(&hand.id, lease_id).await? {
            self.finish_delete(&row).await;
        }
        Ok(())
    }

    async fn finish_delete(&self, hand: &HandBox) {
        if let Err(error) = self
            .infra
            .delete_hand(&hand.id, hand.iron_proxy_id.as_deref())
            .await
        {
            warn!(hand_id = %hand.id, %error, "failed to delete hand resources");
        }
        if let Err(error) = self.store.delete(&hand.id).await {
            warn!(hand_id = %hand.id, %error, "failed to delete hand row");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::Mutex,
    };

    use async_trait::async_trait;

    use super::*;

    #[derive(Default)]
    struct FakeStore {
        hands: Mutex<Vec<HandBox>>,
    }

    impl FakeStore {
        fn with_ready(count: usize, size: HandSize) -> Self {
            let mut hands = Vec::new();
            for index in 0..count {
                hands.push(hand(
                    &format!("hnd_ready{index}"),
                    size,
                    "ready",
                    Some(&format!("10.0.0.{}", index + 2)),
                ));
            }
            Self {
                hands: Mutex::new(hands),
            }
        }

        fn hands(&self) -> std::sync::MutexGuard<'_, Vec<HandBox>> {
            self.hands.lock().expect("fake store lock")
        }
    }

    fn hand(id: &str, size: HandSize, state: &str, pod_ip: Option<&str>) -> HandBox {
        let now = OffsetDateTime::now_utc();
        HandBox {
            id: id.to_owned(),
            size: size.as_str().to_owned(),
            state: state.to_owned(),
            pod_name: crate::model::pod_name_for_hand(id),
            pod_ip: pod_ip.map(ToOwned::to_owned),
            iron_proxy_id: Some(format!("prx_{id}")),
            lease_id: (state == "leased").then(|| format!("hls_{id}")),
            thread_key: (state == "leased").then(|| "thread:old".to_owned()),
            principal: (state == "leased").then(|| "prn_old".to_owned()),
            requester: None,
            leased_at: (state == "leased").then_some(now),
            lease_expires_at: (state == "leased").then_some(now + time::Duration::seconds(60)),
            created_at: now,
            updated_at: now,
        }
    }

    #[async_trait]
    impl HandStore for FakeStore {
        async fn insert_warming(
            &self,
            id: &str,
            size: HandSize,
            _pod_name: &str,
            pod_ip: Option<&str>,
            _iron_proxy_id: &str,
        ) -> HandsResult<HandBox> {
            let hand = hand(id, size, "warming", pod_ip);
            self.hands().push(hand.clone());
            Ok(hand)
        }

        async fn mark_ready(&self, id: &str, pod_ip: &str) -> HandsResult<Option<HandBox>> {
            let mut hands = self.hands();
            let Some(hand) = hands.iter_mut().find(|hand| hand.id == id) else {
                return Ok(None);
            };
            if hand.state == "warming" {
                hand.state = "ready".to_owned();
                hand.pod_ip = Some(pod_ip.to_owned());
                return Ok(Some(hand.clone()));
            }
            // Idempotent, like the Postgres transition: a replenisher that got
            // here first leaves a healthy ready row for the checkout's wait.
            Ok((hand.state == "ready").then(|| hand.clone()))
        }

        async fn list_warming(&self) -> HandsResult<Vec<HandBox>> {
            Ok(self
                .hands()
                .iter()
                .filter(|hand| hand.is_warming())
                .cloned()
                .collect())
        }

        async fn claim_ready(
            &self,
            size: HandSize,
            claim: &LeaseClaim,
        ) -> HandsResult<Option<HandBox>> {
            let mut hands = self.hands();
            let Some(hand) = hands
                .iter_mut()
                .find(|hand| hand.is_ready() && hand.size() == size)
            else {
                return Ok(None);
            };
            claim_row(hand, claim);
            Ok(Some(hand.clone()))
        }

        async fn lease_specific(
            &self,
            id: &str,
            claim: &LeaseClaim,
        ) -> HandsResult<Option<HandBox>> {
            let mut hands = self.hands();
            let Some(hand) = hands
                .iter_mut()
                .find(|hand| hand.id == id && (hand.is_ready() || hand.is_warming()))
            else {
                return Ok(None);
            };
            claim_row(hand, claim);
            Ok(Some(hand.clone()))
        }

        async fn find_thread_lease(&self, thread_key: &str) -> HandsResult<Option<HandBox>> {
            Ok(self
                .hands()
                .iter()
                .find(|hand| hand.is_leased() && hand.thread_key.as_deref() == Some(thread_key))
                .cloned())
        }

        async fn find_by_lease(&self, lease_id: &str) -> HandsResult<Option<HandBox>> {
            Ok(self
                .hands()
                .iter()
                .find(|hand| hand.lease_id.as_deref() == Some(lease_id))
                .cloned())
        }

        async fn update_binding(
            &self,
            lease_id: &str,
            thread_key: &str,
            principal: &str,
            requester: Option<&str>,
        ) -> HandsResult<Option<HandBox>> {
            let mut hands = self.hands();
            let Some(hand) = hands.iter_mut().find(|hand| {
                hand.lease_id.as_deref() == Some(lease_id)
                    && hand.thread_key.as_deref() == Some(thread_key)
                    && hand.is_leased()
            }) else {
                return Ok(None);
            };
            hand.principal = Some(principal.to_owned());
            hand.requester = requester.map(ToOwned::to_owned);
            Ok(Some(hand.clone()))
        }

        async fn renew_lease(
            &self,
            lease_id: &str,
            thread_key: &str,
            principal: &str,
            requester: Option<&str>,
            expires_at: OffsetDateTime,
        ) -> HandsResult<Option<HandBox>> {
            let now = OffsetDateTime::now_utc();
            let mut hands = self.hands();
            let Some(hand) = hands.iter_mut().find(|hand| {
                hand.lease_id.as_deref() == Some(lease_id)
                    && hand.thread_key.as_deref() == Some(thread_key)
                    && hand.is_live_lease(now)
            }) else {
                return Ok(None);
            };
            hand.principal = Some(principal.to_owned());
            hand.requester = requester.map(ToOwned::to_owned);
            hand.lease_expires_at = Some(expires_at);
            Ok(Some(hand.clone()))
        }

        async fn expire_lease(
            &self,
            id: &str,
            now: OffsetDateTime,
        ) -> HandsResult<Option<HandBox>> {
            let mut hands = self.hands();
            let Some(hand) = hands.iter_mut().find(|hand| {
                hand.id == id
                    && hand.is_leased()
                    && hand
                        .lease_expires_at
                        .is_some_and(|expires_at| expires_at <= now)
            }) else {
                return Ok(None);
            };
            hand.state = "deleting".to_owned();
            Ok(Some(hand.clone()))
        }

        async fn discard_lease(&self, id: &str, lease_id: &str) -> HandsResult<Option<HandBox>> {
            let mut hands = self.hands();
            let Some(hand) = hands.iter_mut().find(|hand| {
                hand.id == id && hand.lease_id.as_deref() == Some(lease_id) && hand.is_leased()
            }) else {
                return Ok(None);
            };
            hand.state = "deleting".to_owned();
            Ok(Some(hand.clone()))
        }

        async fn set_deleting(&self, id: &str) -> HandsResult<()> {
            if let Some(hand) = self
                .hands()
                .iter_mut()
                .find(|hand| hand.id == id && (hand.is_warming() || hand.is_ready()))
            {
                hand.state = "deleting".to_owned();
            }
            Ok(())
        }

        async fn delete(&self, id: &str) -> HandsResult<()> {
            self.hands().retain(|hand| hand.id != id);
            Ok(())
        }

        async fn total_hands(&self) -> HandsResult<usize> {
            Ok(self
                .hands()
                .iter()
                .filter(|hand| hand.state != "deleting")
                .count())
        }

        async fn count_pool(&self, size: HandSize) -> HandsResult<usize> {
            Ok(self
                .hands()
                .iter()
                .filter(|hand| hand.size() == size && (hand.is_ready() || hand.is_warming()))
                .count())
        }

        async fn list_expired(&self, now: OffsetDateTime) -> HandsResult<Vec<HandBox>> {
            Ok(self
                .hands()
                .iter()
                .filter(|hand| {
                    hand.is_leased()
                        && hand
                            .lease_expires_at
                            .is_some_and(|expires_at| expires_at <= now)
                })
                .cloned()
                .collect())
        }

        async fn list_leased(&self) -> HandsResult<Vec<HandBox>> {
            Ok(self
                .hands()
                .iter()
                .filter(|hand| hand.is_leased())
                .cloned()
                .collect())
        }

        async fn list_stuck_warming(
            &self,
            older_than: OffsetDateTime,
        ) -> HandsResult<Vec<HandBox>> {
            Ok(self
                .hands()
                .iter()
                .filter(|hand| hand.is_warming() && hand.updated_at < older_than)
                .cloned()
                .collect())
        }

        async fn list_deleting(&self, older_than: OffsetDateTime) -> HandsResult<Vec<HandBox>> {
            Ok(self
                .hands()
                .iter()
                .filter(|hand| hand.state == "deleting" && hand.updated_at < older_than)
                .cloned()
                .collect())
        }

        async fn live_ids(&self) -> HandsResult<BTreeSet<String>> {
            Ok(self.hands().iter().map(|hand| hand.id.clone()).collect())
        }
    }

    fn claim_row(hand: &mut HandBox, claim: &LeaseClaim) {
        hand.state = "leased".to_owned();
        hand.lease_id = Some(claim.lease_id.clone());
        hand.thread_key = Some(claim.thread_key.clone());
        hand.principal = Some(claim.principal.clone());
        hand.requester = claim.requester.clone();
        hand.lease_expires_at = Some(claim.expires_at);
        hand.pod_ip.get_or_insert_with(|| "10.0.0.99".to_owned());
    }

    #[derive(Default)]
    struct FakeInfra {
        state: Mutex<FakeInfraState>,
    }

    #[derive(Default)]
    struct FakeInfraState {
        created: Vec<(String, HandSize)>,
        deleted: Vec<String>,
        bound: Vec<(String, String, Option<String>)>,
        observation: BTreeMap<String, HandObservation>,
        missing_tokens: BTreeSet<String>,
    }

    impl FakeInfra {
        fn ready() -> Self {
            Self::default()
        }

        fn state(&self) -> std::sync::MutexGuard<'_, FakeInfraState> {
            self.state.lock().expect("fake infra lock")
        }

        fn observe(&self, id: &str, observation: HandObservation) {
            self.state().observation.insert(id.to_owned(), observation);
        }

        fn lose_token(&self, id: &str) {
            self.state().missing_tokens.insert(id.to_owned());
        }
    }

    #[async_trait]
    impl HandInfra for FakeInfra {
        async fn create_hand(&self, id: &str, size: HandSize) -> HandsResult<crate::HandResources> {
            self.state().created.push((id.to_owned(), size));
            Ok(crate::HandResources {
                pod_name: crate::model::pod_name_for_hand(id),
                pod_ip: Some("10.0.0.50".to_owned()),
                iron_proxy_id: format!("prx_{id}"),
            })
        }

        async fn observe_hand(&self, id: &str, _pod_name: &str) -> HandsResult<HandObservation> {
            Ok(self
                .state()
                .observation
                .get(id)
                .cloned()
                .unwrap_or(HandObservation::Ready {
                    pod_ip: "10.0.0.50".to_owned(),
                }))
        }

        async fn bind_principal(
            &self,
            hand: &HandBox,
            principal: &str,
            requester: Option<&str>,
        ) -> HandsResult<()> {
            self.state().bound.push((
                hand.id.clone(),
                principal.to_owned(),
                requester.map(ToOwned::to_owned),
            ));
            Ok(())
        }

        async fn placeholders(&self, principal: &str) -> HandsResult<BTreeMap<String, String>> {
            Ok(BTreeMap::from([(
                "GITHUB_TOKEN".to_owned(),
                format!("placeholder-{principal}"),
            )]))
        }

        async fn read_token(&self, id: &str) -> HandsResult<String> {
            if self.state().missing_tokens.contains(id) {
                return Err(HandsError::Backend(format!("hand {id} Secret is gone")));
            }
            Ok(format!("hand-token-{id}"))
        }

        async fn delete_hand(&self, id: &str, _proxy_id: Option<&str>) -> HandsResult<()> {
            self.state().deleted.push(id.to_owned());
            Ok(())
        }

        async fn sweep_orphans(
            &self,
            _live: &BTreeSet<String>,
            _grace: Duration,
        ) -> HandsResult<u32> {
            Ok(0)
        }
    }

    struct TestPool {
        pool: Arc<HandPool>,
        store: Arc<FakeStore>,
        infra: Arc<FakeInfra>,
    }

    impl std::ops::Deref for TestPool {
        type Target = HandPool;

        fn deref(&self) -> &Self::Target {
            &self.pool
        }
    }

    impl TestPool {
        fn store_hands(&self) -> std::sync::MutexGuard<'_, Vec<HandBox>> {
            self.store.hands()
        }
        fn infra_created(&self) -> Vec<(String, HandSize)> {
            self.infra.state().created.clone()
        }
        fn infra_deleted(&self) -> Vec<String> {
            self.infra.state().deleted.clone()
        }
        fn infra_bound(&self) -> Vec<(String, String, Option<String>)> {
            self.infra.state().bound.clone()
        }
    }

    fn pool(store: FakeStore, infra: FakeInfra, max_hands: usize) -> TestPool {
        let store = Arc::new(store);
        let infra = Arc::new(infra);
        TestPool {
            pool: Arc::new(HandPool::new(
                store.clone(),
                infra.clone(),
                HandPoolConfig {
                    pool_sizes: BTreeMap::from([(HandSize::Default, 2), (HandSize::Large, 0)]),
                    replenish_interval: Duration::from_secs(60),
                    sweep_interval: Duration::from_secs(60),
                    lease_duration: Duration::from_secs(300),
                    cold_create_timeout: Duration::from_secs(5),
                    warming_timeout: Duration::from_secs(300),
                    orphan_grace: Duration::from_secs(60),
                    max_hands,
                },
            )),
            store,
            infra,
        }
    }

    #[tokio::test]
    async fn checkout_claims_a_ready_hand_and_renders_the_lease() {
        let store = FakeStore::with_ready(1, HandSize::Default);
        let infra = FakeInfra::ready();
        let pool = pool(store, infra, 10);
        let lease = pool
            .lease(LeaseRequest {
                thread_key: "thread:a",
                principal: "prn_channel",
                requester: Some("prn_user"),
                size: None,
            })
            .await
            .expect("lease");
        assert_eq!(lease.hand_id, "hnd_ready0");
        assert_eq!(lease.url, "ws://10.0.0.2:7070");
        assert_eq!(lease.token, "hand-token-hnd_ready0");
        assert_eq!(lease.env["GITHUB_TOKEN"], "placeholder-prn_channel");
        assert_eq!(pool.store.total_hands().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn checkout_is_idempotent_and_rebinds_when_the_requester_changes() {
        let store = FakeStore::with_ready(1, HandSize::Default);
        let infra = FakeInfra::ready();
        let pool = pool(store, infra, 10);
        let first = pool
            .lease(LeaseRequest {
                thread_key: "thread:a",
                principal: "prn_channel",
                requester: Some("prn_user_one"),
                size: None,
            })
            .await
            .expect("first lease");
        let again = pool
            .lease(LeaseRequest {
                thread_key: "thread:a",
                principal: "prn_channel",
                requester: Some("prn_user_two"),
                size: None,
            })
            .await
            .expect("idempotent lease");
        assert_eq!(again.lease_id, first.lease_id);
        assert_eq!(again.hand_id, first.hand_id);
        assert_eq!(again.expires_at, first.expires_at);
        let bound = pool.infra_bound();
        assert_eq!(bound.len(), 2);
        assert_eq!(bound[1].2.as_deref(), Some("prn_user_two"));
    }

    #[tokio::test]
    async fn cold_create_waits_for_readiness_and_release_is_idempotent() {
        let store = FakeStore::default();
        let infra = FakeInfra::ready();
        let pool = pool(store, infra, 10);
        let lease = pool
            .lease(LeaseRequest {
                thread_key: "thread:a",
                principal: "prn_channel",
                requester: None,
                size: Some(HandSize::Default),
            })
            .await
            .expect("cold lease");
        assert_eq!(lease.size, HandSize::Default);
        assert!(lease.hand_id.starts_with("hnd_"));
        pool.release(&lease.lease_id, "thread:a")
            .await
            .expect("release");
        pool.release(&lease.lease_id, "thread:a")
            .await
            .expect("release again");
    }

    #[tokio::test]
    async fn release_by_another_thread_does_not_delete_the_hand() {
        let store = FakeStore::with_ready(1, HandSize::Default);
        let infra = FakeInfra::ready();
        let pool = pool(store, infra, 10);
        let lease = pool
            .lease(LeaseRequest {
                thread_key: "thread:a",
                principal: "prn_channel",
                requester: None,
                size: None,
            })
            .await
            .expect("lease");
        assert!(matches!(
            pool.release(&lease.lease_id, "thread:b").await,
            Err(HandsError::NotFound)
        ));
        assert_eq!(pool.store.total_hands().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn replenisher_fills_each_size_to_target_and_respects_the_cap() {
        let store = FakeStore::default();
        let infra = FakeInfra::ready();
        let pool = pool(store, infra, 15);
        pool.replenish_once().await.expect("replenish");
        assert_eq!(pool.store.count_pool(HandSize::Default).await.unwrap(), 2);
        assert_eq!(pool.store.count_pool(HandSize::Large).await.unwrap(), 0);
        assert_eq!(pool.infra_created().len(), 2);
    }

    #[tokio::test]
    async fn sweeper_deletes_expired_leases() {
        let store = FakeStore::with_ready(1, HandSize::Default);
        let infra = FakeInfra::ready();
        let pool = pool(store, infra, 10);
        let lease = pool
            .lease(LeaseRequest {
                thread_key: "thread:a",
                principal: "prn_channel",
                requester: None,
                size: None,
            })
            .await
            .expect("lease");
        assert!(pool.current("thread:a").await.expect("current").is_some());

        {
            let mut hands = pool.store_hands();
            hands[0].lease_expires_at =
                Some(OffsetDateTime::now_utc() - time::Duration::seconds(1));
        }
        pool.sweep_once().await.expect("sweep");
        assert!(pool.current("thread:a").await.expect("current").is_none());
        assert!(
            pool.infra_deleted()
                .iter()
                .any(|deleted| deleted == &lease.hand_id)
        );
    }

    #[tokio::test]
    async fn lease_at_capacity_is_rejected_without_creating() {
        let store = FakeStore::default();
        let infra = FakeInfra::ready();
        let pool = pool(store, infra, 1);
        let first = pool
            .lease(LeaseRequest {
                thread_key: "thread:a",
                principal: "prn_a",
                requester: None,
                size: None,
            })
            .await
            .expect("first fits");
        assert!(!first.hand_id.is_empty());
        assert!(matches!(
            pool.lease(LeaseRequest {
                thread_key: "thread:b",
                principal: "prn_b",
                requester: None,
                size: None,
            })
            .await,
            Err(HandsError::Capacity)
        ));
    }
    #[tokio::test]
    async fn readiness_is_idempotent_when_the_replenisher_marks_the_hand_first() {
        let store = FakeStore::default();
        let infra = FakeInfra::ready();
        let pool = pool(store, infra, 10);
        let warming = pool
            .pool
            .create_warming(HandSize::Default)
            .await
            .expect("create warming");
        // The replenisher wins the race and marks this same hand ready.
        let ready = pool
            .store
            .mark_ready(&warming.id, "10.0.0.50")
            .await
            .expect("replenisher mark ready")
            .expect("replenisher sees the row");
        assert!(ready.is_ready());
        // The cold checkout's wait must accept the already-ready row.
        let waited = pool
            .pool
            .wait_until_ready(&warming)
            .await
            .expect("idempotent readiness");
        assert_eq!(waited.id, warming.id);
        assert!(waited.is_ready());
        assert!(pool.infra_deleted().is_empty());
    }

    #[tokio::test]
    async fn lease_replaces_a_live_lease_whose_hand_vanished() {
        let store = FakeStore::with_ready(2, HandSize::Default);
        let infra = FakeInfra::ready();
        let pool = pool(store, infra, 10);
        let first = pool
            .lease(LeaseRequest {
                thread_key: "thread:a",
                principal: "prn_channel",
                requester: None,
                size: None,
            })
            .await
            .expect("first lease");
        pool.infra.observe(&first.hand_id, HandObservation::Gone);

        let replacement = pool
            .lease(LeaseRequest {
                thread_key: "thread:a",
                principal: "prn_channel",
                requester: None,
                size: None,
            })
            .await
            .expect("replacement lease");
        assert_ne!(replacement.hand_id, first.hand_id);
        assert!(
            pool.infra_deleted()
                .iter()
                .any(|deleted| deleted == &first.hand_id)
        );
        assert_eq!(pool.store.total_hands().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn lease_replaces_a_live_lease_whose_secret_vanished() {
        let store = FakeStore::with_ready(2, HandSize::Default);
        let infra = FakeInfra::ready();
        let pool = pool(store, infra, 10);
        let first = pool
            .lease(LeaseRequest {
                thread_key: "thread:a",
                principal: "prn_channel",
                requester: None,
                size: None,
            })
            .await
            .expect("first lease");
        pool.infra.lose_token(&first.hand_id);

        let replacement = pool
            .lease(LeaseRequest {
                thread_key: "thread:a",
                principal: "prn_channel",
                requester: None,
                size: None,
            })
            .await
            .expect("replacement lease");
        assert_ne!(replacement.hand_id, first.hand_id);
        assert!(
            pool.infra_deleted()
                .iter()
                .any(|deleted| deleted == &first.hand_id)
        );
    }

    #[tokio::test]
    async fn current_destroys_a_live_lease_whose_hand_vanished() {
        let store = FakeStore::with_ready(1, HandSize::Default);
        let infra = FakeInfra::ready();
        let pool = pool(store, infra, 10);
        let lease = pool
            .lease(LeaseRequest {
                thread_key: "thread:a",
                principal: "prn_channel",
                requester: None,
                size: None,
            })
            .await
            .expect("lease");
        pool.infra.observe(&lease.hand_id, HandObservation::Gone);
        assert!(pool.current("thread:a").await.expect("current").is_none());
        assert!(
            pool.infra_deleted()
                .iter()
                .any(|deleted| deleted == &lease.hand_id)
        );
    }

    #[tokio::test]
    async fn sweeper_destroys_a_leased_hand_whose_pod_is_gone() {
        let store = FakeStore::with_ready(1, HandSize::Default);
        let infra = FakeInfra::ready();
        let pool = pool(store, infra, 10);
        let lease = pool
            .lease(LeaseRequest {
                thread_key: "thread:a",
                principal: "prn_channel",
                requester: None,
                size: None,
            })
            .await
            .expect("lease");
        pool.infra.observe(&lease.hand_id, HandObservation::Gone);
        pool.sweep_once().await.expect("sweep");
        assert!(
            pool.infra_deleted()
                .iter()
                .any(|deleted| deleted == &lease.hand_id)
        );
        assert!(pool.current("thread:a").await.expect("current").is_none());
    }

    #[tokio::test]
    async fn expiry_transition_does_not_delete_a_concurrently_renewed_hand() {
        let store = FakeStore::with_ready(1, HandSize::Default);
        let infra = FakeInfra::ready();
        let pool = pool(store, infra, 10);
        let lease = pool
            .lease(LeaseRequest {
                thread_key: "thread:a",
                principal: "prn_channel",
                requester: None,
                size: None,
            })
            .await
            .expect("lease");
        let expired = {
            let mut hands = pool.store_hands();
            hands[0].lease_expires_at =
                Some(OffsetDateTime::now_utc() - time::Duration::seconds(1));
            hands[0].clone()
        };
        let now = OffsetDateTime::now_utc();
        // A renew lands after the sweeper read the expired row but before its
        // conditional transition.
        {
            let mut hands = pool.store_hands();
            hands[0].lease_expires_at = Some(now + time::Duration::seconds(60));
        }
        pool.pool
            .destroy_expired(&expired, now)
            .await
            .expect("conditional expiry");
        assert!(pool.infra_deleted().is_empty());
        assert!(pool.current("thread:a").await.expect("current").is_some());
        assert!(
            pool.store
                .find_by_lease(&lease.lease_id)
                .await
                .unwrap()
                .is_some()
        );
    }
}

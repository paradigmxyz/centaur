//! Warm pool, lease store, and Kubernetes driver for Centaur hands.
//!
//! A hand is a single-use pod with a `hand` container and a per-hand
//! `iron-proxy` sidecar. The pool keeps a few unbound hands ready, leases one
//! to a thread, and deletes it when the lease ends. Credentials stay in the
//! proxy: a lease carries only the principal's replace placeholders and the
//! proxy is rebound to the turn's principal at checkout.

mod k8s;
mod model;
mod pool;
mod store;

pub use k8s::{HandInfra, HandK8sConfig, HandObservation, HandResources, KubernetesHandInfra};
pub use model::{
    HandBox, HandLease, HandSize, LeaseClaim, LeaseRequest, new_hand_id, new_lease_id,
    pod_name_for_hand, secret_name_for_hand,
};
pub use pool::{HandPool, HandPoolConfig};
pub use store::{HandStore, HandsError, HandsResult, PgHandStore};

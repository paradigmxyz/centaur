use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use centaur_sandbox_core::{DesiredSandboxState, SandboxId};

pub trait DesiredStateStore: Send + Sync {
    fn get(&self, id: &SandboxId) -> Option<DesiredSandboxState>;
    fn set(&self, id: SandboxId, state: DesiredSandboxState);
    fn remove(&self, id: &SandboxId) -> Option<DesiredSandboxState>;
    fn list(&self) -> Vec<(SandboxId, DesiredSandboxState)>;
}

#[derive(Clone, Default)]
pub struct InMemoryDesiredStateStore {
    inner: Arc<RwLock<HashMap<SandboxId, DesiredSandboxState>>>,
}

impl InMemoryDesiredStateStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl DesiredStateStore for InMemoryDesiredStateStore {
    fn get(&self, id: &SandboxId) -> Option<DesiredSandboxState> {
        self.inner
            .read()
            .expect("desired state lock poisoned")
            .get(id)
            .cloned()
    }

    fn set(&self, id: SandboxId, state: DesiredSandboxState) {
        self.inner
            .write()
            .expect("desired state lock poisoned")
            .insert(id, state);
    }

    fn remove(&self, id: &SandboxId) -> Option<DesiredSandboxState> {
        self.inner
            .write()
            .expect("desired state lock poisoned")
            .remove(id)
    }

    fn list(&self) -> Vec<(SandboxId, DesiredSandboxState)> {
        self.inner
            .read()
            .expect("desired state lock poisoned")
            .iter()
            .map(|(id, state)| (id.clone(), state.clone()))
            .collect()
    }
}

//! Logical "sandboxes": the handles api-rs holds for brain sessions, each with
//! one attachable output stream and a replay buffer.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::sync::mpsc;

const OUTPUT_LINES_MAX: usize = 10_000;

#[derive(Clone, Default)]
pub struct Registry {
    inner: Arc<Mutex<Sandboxes>>,
}

#[derive(Default)]
struct Sandboxes {
    live: HashMap<String, Sandbox>,
    /// Ids deleted in this process stay gone. Unknown ids are recreated on use
    /// because a restarted brain forgets its handles.
    deleted: HashSet<String>,
}

#[derive(Default)]
struct Sandbox {
    thread_key: Option<String>,
    output: VecDeque<(u64, String)>,
    attached: Option<(u64, mpsc::UnboundedSender<String>)>,
    attachments: u64,
}

pub struct Attachment {
    pub generation: u64,
    pub lines: mpsc::UnboundedReceiver<String>,
}

impl Registry {
    /// Record a sandbox, or confirm a known one; repeating a create keeps its
    /// buffer. Returns false for a deleted id.
    pub fn create(&self, id: &str, thread_key: Option<String>) -> bool {
        let mut sandboxes = self.lock();
        if sandboxes.deleted.contains(id) {
            return false;
        }
        let sandbox = sandboxes.live.entry(id.to_owned()).or_default();
        if thread_key.is_some() {
            sandbox.thread_key = thread_key;
        }
        true
    }

    pub fn delete(&self, id: &str) -> bool {
        let mut sandboxes = self.lock();
        sandboxes.deleted.insert(id.to_owned());
        sandboxes.live.remove(id).is_some()
    }

    pub fn thread_key(&self, id: &str) -> Option<String> {
        self.lock()
            .live
            .get(id)
            .and_then(|sandbox| sandbox.thread_key.clone())
    }

    pub fn set_thread_key(&self, id: &str, thread_key: &str) {
        if let Some(sandbox) = self.lock().live.get_mut(id) {
            sandbox.thread_key = Some(thread_key.to_owned());
        }
    }

    /// Attach the sole live reader, replacing any previous one. None for a
    /// deleted id.
    pub fn attach(&self, id: &str) -> Option<Attachment> {
        let (sender, lines) = mpsc::unbounded_channel();
        let mut sandboxes = self.lock();
        if sandboxes.deleted.contains(id) {
            return None;
        }
        let sandbox = sandboxes.live.entry(id.to_owned()).or_default();
        sandbox.attachments += 1;
        let generation = sandbox.attachments;
        sandbox.attached = Some((generation, sender));
        Some(Attachment { generation, lines })
    }

    pub fn detach(&self, id: &str, generation: u64) {
        if let Some(sandbox) = self.lock().live.get_mut(id)
            && sandbox
                .attached
                .as_ref()
                .is_some_and(|(current, _)| *current == generation)
        {
            sandbox.attached = None;
        }
    }

    /// Buffer one output line and hand it to the attached reader, if any.
    /// Returns false when the sandbox no longer exists.
    pub fn emit(&self, id: &str, line: String) -> bool {
        let mut sandboxes = self.lock();
        let Some(sandbox) = sandboxes.live.get_mut(id) else {
            return false;
        };
        if sandbox.output.len() == OUTPUT_LINES_MAX {
            sandbox.output.pop_front();
        }
        sandbox.output.push_back((now_ms(), line.clone()));
        if let Some((_, sender)) = &sandbox.attached
            && sender.send(line).is_err()
        {
            sandbox.attached = None;
        }
        true
    }

    pub fn output_since(&self, id: &str, since_ms: Option<u64>) -> Option<Vec<String>> {
        let sandboxes = self.lock();
        let sandbox = sandboxes.live.get(id)?;
        Some(
            sandbox
                .output
                .iter()
                .filter(|(at, _)| since_ms.is_none_or(|since| *at >= since))
                .map(|(_, line)| line.clone())
                .collect(),
        )
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Sandboxes> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffers_output_and_streams_to_the_latest_attachment() {
        let registry = Registry::default();
        assert!(registry.create("brain-1", Some("t".into())));
        assert!(registry.emit("brain-1", "a".into()));
        let mut first = registry.attach("brain-1").unwrap();
        assert!(registry.emit("brain-1", "b".into()));
        assert_eq!(first.lines.try_recv().unwrap(), "b");
        let mut second = registry.attach("brain-1").unwrap();
        assert!(registry.emit("brain-1", "c".into()));
        assert_eq!(second.lines.try_recv().unwrap(), "c");
        assert!(first.lines.try_recv().is_err());
        registry.detach("brain-1", first.generation);
        assert!(registry.emit("brain-1", "d".into()));
        assert_eq!(second.lines.try_recv().unwrap(), "d");
        assert_eq!(
            registry.output_since("brain-1", None).unwrap(),
            vec!["a", "b", "c", "d"]
        );
        assert!(
            registry
                .output_since("brain-1", Some(u64::MAX))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn deleted_sandboxes_stay_gone() {
        let registry = Registry::default();
        assert!(registry.create("brain-1", None));
        registry.set_thread_key("brain-1", "t");
        assert_eq!(registry.thread_key("brain-1").as_deref(), Some("t"));
        assert!(registry.delete("brain-1"));
        assert!(!registry.delete("brain-1"));
        assert!(!registry.emit("brain-1", "x".into()));
        assert!(registry.output_since("brain-1", None).is_none());
        assert!(!registry.create("brain-1", None));
        assert!(registry.attach("brain-1").is_none());
        assert!(registry.create("brain-2", None));
    }
}

//! Hot reload of the connector catalog.
//!
//! Connector roots are overlay checkouts the platform may update under a
//! running process. Every interval the reloader hashes them; when the hash
//! changes it loads a new catalog and swaps it in. A failed load keeps the old
//! catalog, and cells already running keep the catalog they started with.

use std::{path::PathBuf, sync::Arc, time::Duration};

use crate::{catalog, engine::Engine};

pub struct Reloader {
    roots: Vec<PathBuf>,
    interval: Duration,
}

impl Reloader {
    pub fn new(roots: Vec<PathBuf>, interval: Duration) -> Self {
        Self {
            roots,
            interval: interval.max(Duration::from_secs(1)),
        }
    }

    /// Check once; returns whether a new catalog was swapped in.
    pub fn once(&self, engine: &Engine) -> bool {
        let revision = match catalog::revision(&self.roots) {
            Ok(revision) => revision,
            Err(error) => {
                tracing::warn!(
                    error = %format!("{error:#}"),
                    "connector roots are unreadable; keeping the loaded catalog"
                );
                return false;
            }
        };
        if revision == engine.catalog().revision {
            return false;
        }
        match catalog::Catalog::load(&self.roots, engine.catalog().files) {
            Ok(catalog) if catalog.connectors.is_empty() => {
                tracing::warn!("catalog reload found no connectors; keeping the loaded catalog");
                false
            }
            Ok(catalog) => {
                tracing::info!(
                    revision = %catalog.revision,
                    connectors = catalog.connectors.len(),
                    "catalog reloaded"
                );
                engine.replace_catalog(Arc::new(catalog));
                true
            }
            Err(error) => {
                tracing::warn!(
                    error = %format!("{error:#}"),
                    "catalog reload failed; keeping the loaded catalog"
                );
                false
            }
        }
    }

    /// Check every interval, for as long as the process lives.
    pub async fn run(self, engine: Engine) {
        let mut ticker = tokio::time::interval(self.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            self.once(&engine);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn write_connector(root: &Path, function: &str, doc: &str) {
        let dir = root.join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("connector.toml"), "description = \"Demo\"\n").unwrap();
        std::fs::write(
            dir.join("index.ts"),
            format!(
                "import type {{ Ctx }} from \"../centaur\";\n\n/** {doc} */\n\
                 export async function {function}(args: {{}}, ctx: Ctx): Promise<{{}}> {{ return {{}}; }}\n"
            ),
        )
        .unwrap();
    }

    fn engine(root: &Path) -> Engine {
        let catalog = catalog::Catalog::load(&[root.to_owned()], false).unwrap();
        Engine::new(Arc::new(catalog), None, None, None).unwrap()
    }

    fn reloader(root: &Path) -> Reloader {
        Reloader::new(vec![root.to_owned()], Duration::from_secs(10))
    }

    #[test]
    fn swaps_the_catalog_and_bumps_the_revision() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("connectors");
        write_connector(&root, "first", "Do the first thing.");
        let engine = engine(&root);
        let reloader = reloader(&root);
        let before = engine.catalog();
        assert!(!reloader.once(&engine), "nothing changed yet");

        write_connector(&root, "second", "Do the second thing.");
        assert!(reloader.once(&engine));
        let after = engine.catalog();
        assert_ne!(before.revision, after.revision);
        assert!(
            after.connectors["demo"]
                .functions
                .iter()
                .any(|function| function.name == "second")
        );
        assert!(
            before.connectors["demo"]
                .functions
                .iter()
                .all(|function| function.name == "first"),
            "the old catalog a running cell holds is untouched"
        );
        assert!(!reloader.once(&engine), "the revision has settled");
    }

    #[test]
    fn a_failed_or_empty_load_keeps_the_old_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("connectors");
        write_connector(&root, "first", "Do the first thing.");
        let engine = engine(&root);
        let reloader = reloader(&root);
        let revision = engine.catalog().revision.clone();

        std::fs::write(root.join("demo/index.ts"), "export async function broken(").unwrap();
        assert!(!reloader.once(&engine));
        assert_eq!(engine.catalog().revision, revision);

        // An empty reload is refused too.
        std::fs::remove_dir_all(&root).unwrap();
        assert!(!reloader.once(&engine));
        assert!(engine.catalog().connectors.contains_key("demo"));
    }
}

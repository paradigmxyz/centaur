use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::blocking::Client;
use serde::Deserialize;
use uuid::Uuid;

use crate::Result;

const ORGANIZATION_HEADING: &str = "[Organization instructions]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInstructionsUpdate {
    pub changed: bool,
    pub revision: Option<String>,
    pub content: Option<String>,
    pub sha256: Option<String>,
}

pub struct RuntimeInstructions {
    endpoint: String,
    bearer_token: Option<String>,
    baseline_path: PathBuf,
    target_path: PathBuf,
    applied_revision: Option<String>,
    client: Client,
}

#[derive(Deserialize)]
struct RuntimeInstructionsResponse {
    data: PublishedInstructions,
}

#[derive(Deserialize)]
struct PublishedInstructions {
    revision: Option<String>,
    content: String,
    sha256: String,
}

impl RuntimeInstructions {
    pub fn new(
        endpoint: String,
        bearer_token: Option<String>,
        baseline_path: PathBuf,
        target_path: PathBuf,
    ) -> Self {
        Self {
            endpoint,
            bearer_token,
            baseline_path,
            target_path,
            applied_revision: None,
            client: Client::new(),
        }
    }

    pub fn from_env() -> Option<Self> {
        let console_url = env::var("CENTAUR_CONSOLE_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_owned())
            .filter(|value| !value.is_empty())?;
        let baseline_path = env::var_os("CENTAUR_RUNTIME_INSTRUCTIONS_BASELINE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("AGENTS_RUNTIME_BASE.md"));
        let target_path = env::var_os("CENTAUR_RUNTIME_INSTRUCTIONS_TARGET")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("AGENTS.md"));
        let bearer_token = env::var("CENTAUR_RUNTIME_INSTRUCTIONS_TOKEN")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        Some(Self::new(
            format!("{console_url}/api/v1/sandbox/runtime_instructions"),
            bearer_token,
            baseline_path,
            target_path,
        ))
    }

    pub fn refresh(&mut self) -> Result<RuntimeInstructionsUpdate> {
        let mut request = self
            .client
            .get(&self.endpoint)
            .timeout(Duration::from_secs(5));
        if let Some(token) = &self.bearer_token {
            request = request.bearer_auth(token);
        }
        let published = request
            .send()?
            .error_for_status()?
            .json::<RuntimeInstructionsResponse>()?
            .data;
        let changed = self.applied_revision != published.revision;
        if changed {
            let baseline = fs::read_to_string(&self.baseline_path)?;
            let composed = compose(&baseline, &published.content);
            atomic_write(&self.target_path, composed.as_bytes())?;
            self.applied_revision = published.revision.clone();
        }
        Ok(RuntimeInstructionsUpdate {
            changed,
            revision: published.revision,
            content: Some(published.content),
            sha256: Some(published.sha256),
        })
    }
}

fn compose(baseline: &str, organization: &str) -> String {
    let baseline = baseline.trim_end();
    let organization = organization.trim();
    if organization.is_empty() {
        return format!("{baseline}\n");
    }
    format!("{baseline}\n\n---\n\n{ORGANIZATION_HEADING}\n{organization}\n")
}

fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("AGENTS.md");
    let temporary = parent.join(format!(".{name}.{}.tmp", Uuid::new_v4().simple()));
    fs::write(&temporary, content)?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoCacheAccess {
    None,
    Public,
    #[default]
    All,
}

impl RepoCacheAccess {
    pub fn enabled(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Public => "public",
            Self::All => "all",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SandboxCapabilities {
    #[serde(default)]
    pub repo_cache: RepoCacheAccess,
    pub observability_enabled: bool,
}

impl SandboxCapabilities {
    pub const fn default_enabled() -> Self {
        Self {
            repo_cache: RepoCacheAccess::All,
            observability_enabled: true,
        }
    }

    pub fn is_default_enabled(&self) -> bool {
        self.repo_cache.enabled() && self.observability_enabled
    }
}

impl Default for SandboxCapabilities {
    fn default() -> Self {
        Self::default_enabled()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SandboxSpec {
    pub image: String,
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    pub command: Option<Vec<String>>,
    pub args: Vec<String>,
    pub env: Vec<EnvVar>,
    /// Files materialized in the sandbox before the workload starts.
    /// Target paths are absolute paths inside the sandbox.
    #[serde(default)]
    pub files: Vec<SandboxFile>,
    pub working_dir: Option<String>,
    pub mounts: Vec<Mount>,
    pub resources: Option<ResourceRequirements>,
    /// iron-control principal OID (``prn_…``) this sandbox's egress proxy
    /// should act as. When set, the backend registers/binds an iron-control
    /// proxy for the sandbox instead of rendering a static proxy config.
    #[serde(default)]
    pub iron_control_principal: Option<String>,
    /// iron-control principal OID of the human requesting the turn that
    /// creates this sandbox, bound to the proxy alongside
    /// [`Self::iron_control_principal`].
    #[serde(default)]
    pub iron_control_requester_principal: Option<String>,
    /// Labels applied to the iron-control proxy registered for this sandbox.
    /// These are distinct from Kubernetes labels and are used by iron-control
    /// when rendering proxy-specific config.
    #[serde(default)]
    pub iron_control_proxy_labels: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub capabilities: SandboxCapabilities,
}

impl SandboxSpec {
    pub fn new(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
            labels: std::collections::BTreeMap::new(),
            command: None,
            args: Vec::new(),
            env: Vec::new(),
            files: Vec::new(),
            working_dir: None,
            mounts: Vec::new(),
            resources: None,
            iron_control_principal: None,
            iron_control_requester_principal: None,
            iron_control_proxy_labels: std::collections::BTreeMap::new(),
            capabilities: SandboxCapabilities::default_enabled(),
        }
    }

    pub fn iron_control_principal(mut self, principal_foreign_id: impl Into<String>) -> Self {
        self.iron_control_principal = Some(principal_foreign_id.into());
        self
    }

    pub fn capabilities(mut self, capabilities: SandboxCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn label(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(name.into(), value.into());
        self
    }

    pub fn command(mut self, command: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.command = Some(command.into_iter().map(Into::into).collect());
        self
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push(EnvVar::new(name, value));
        self
    }

    pub fn file(mut self, target_path: impl Into<String>, contents: impl Into<String>) -> Self {
        self.files.push(SandboxFile::new(target_path, contents));
        self
    }

    pub fn working_dir(mut self, working_dir: impl Into<String>) -> Self {
        self.working_dir = Some(working_dir.into());
        self
    }

    pub fn mount(mut self, mount: Mount) -> Self {
        self.mounts.push(mount);
        self
    }

    pub fn resources(mut self, resources: ResourceRequirements) -> Self {
        self.resources = Some(resources);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SandboxFile {
    /// Absolute destination path inside the sandbox.
    pub target_path: String,
    pub contents: String,
}

impl SandboxFile {
    pub fn new(target_path: impl Into<String>, contents: impl Into<String>) -> Self {
        Self {
            target_path: target_path.into(),
            contents: contents.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

impl EnvVar {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Mount {
    pub kind: MountKind,
    pub target_path: String,
    pub read_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_path: Option<String>,
}

impl Mount {
    pub fn new(kind: MountKind, target_path: impl Into<String>) -> Self {
        Self {
            kind,
            target_path: target_path.into(),
            read_only: false,
            sub_path: None,
        }
    }

    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    pub fn sub_path(mut self, sub_path: impl Into<String>) -> Self {
        self.sub_path = Some(sub_path.into());
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MountKind {
    EmptyDir,
    NamedVolume(String),
    Bind { source_path: String },
}

/// Container resources in the Kubernetes `ResourceRequirements` shape.
/// Quantity values are retained as strings and resource names are not limited
/// to CPU and memory, so extended and ephemeral-storage resources survive the
/// backend-neutral sandbox boundary.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRequirements {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<ResourceClaim>,
    #[serde(
        default,
        deserialize_with = "deserialize_quantity_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub limits: BTreeMap<String, String>,
    #[serde(
        default,
        deserialize_with = "deserialize_quantity_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub requests: BTreeMap<String, String>,
}

impl ResourceRequirements {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request(mut self, name: impl Into<String>, quantity: impl Into<String>) -> Self {
        self.requests.insert(name.into(), quantity.into());
        self
    }

    pub fn limit(mut self, name: impl Into<String>, quantity: impl Into<String>) -> Self {
        self.limits.insert(name.into(), quantity.into());
        self
    }

    pub fn claim(mut self, name: impl Into<String>, request: Option<String>) -> Self {
        self.claims.push(ResourceClaim {
            name: name.into(),
            request,
        });
        self
    }

    pub fn is_empty(&self) -> bool {
        self.claims.is_empty() && self.limits.is_empty() && self.requests.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceClaim {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ResourceQuantity {
    String(String),
    Signed(i64),
    Unsigned(u64),
    Float(f64),
}

impl ResourceQuantity {
    fn into_string(self) -> String {
        match self {
            Self::String(value) => value,
            Self::Signed(value) => value.to_string(),
            Self::Unsigned(value) => value.to_string(),
            Self::Float(value) => value.to_string(),
        }
    }
}

fn deserialize_quantity_map<'de, D>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    let quantities = Option::<BTreeMap<String, ResourceQuantity>>::deserialize(deserializer)?
        .unwrap_or_default();
    Ok(quantities
        .into_iter()
        .map(|(name, quantity)| (name, quantity.into_string()))
        .collect())
}

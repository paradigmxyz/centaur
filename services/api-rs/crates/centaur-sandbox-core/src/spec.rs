use serde::{Deserialize, Serialize};

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
    pub api_server_enabled: bool,
}

impl SandboxCapabilities {
    pub const fn default_enabled() -> Self {
        Self {
            repo_cache: RepoCacheAccess::All,
            observability_enabled: true,
            api_server_enabled: true,
        }
    }

    pub fn is_default_enabled(&self) -> bool {
        self.repo_cache.enabled() && self.observability_enabled && self.api_server_enabled
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
    pub working_dir: Option<String>,
    pub mounts: Vec<Mount>,
    pub resources: Option<ResourceLimits>,
    /// iron-control principal OID (``prn_…``) this sandbox's egress proxy
    /// should act as. When set, the backend registers/binds an iron-control
    /// proxy for the sandbox instead of rendering a static proxy config.
    #[serde(default)]
    pub iron_control_principal: Option<String>,
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
            working_dir: None,
            mounts: Vec::new(),
            resources: None,
            iron_control_principal: None,
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

    pub fn working_dir(mut self, working_dir: impl Into<String>) -> Self {
        self.working_dir = Some(working_dir.into());
        self
    }

    pub fn mount(mut self, mount: Mount) -> Self {
        self.mounts.push(mount);
        self
    }

    pub fn resources(mut self, resources: ResourceLimits) -> Self {
        self.resources = Some(resources);
        self
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub cpu_millis: Option<u32>,
    pub memory_bytes: Option<u64>,
}

impl ResourceLimits {
    pub fn new() -> Self {
        Self {
            cpu_millis: None,
            memory_bytes: None,
        }
    }

    pub fn cpu_millis(mut self, cpu_millis: u32) -> Self {
        self.cpu_millis = Some(cpu_millis);
        self
    }

    pub fn memory_bytes(mut self, memory_bytes: u64) -> Self {
        self.memory_bytes = Some(memory_bytes);
        self
    }
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self::new()
    }
}

use std::pin::Pin;

use tokio::io::{AsyncRead, AsyncWrite};

pub type SandboxRead = Pin<Box<dyn AsyncRead + Send>>;
pub type SandboxWrite = Pin<Box<dyn AsyncWrite + Send>>;

pub struct SandboxIo {
    stdin: SandboxWrite,
    stdout: SandboxRead,
    stderr: SandboxRead,
    guard: SandboxIoGuard,
    instance_id: Option<String>,
}

pub struct SandboxIoParts {
    pub stdin: SandboxWrite,
    pub stdout: SandboxRead,
    pub stderr: SandboxRead,
    pub guard: SandboxIoGuard,
    pub instance_id: Option<String>,
}

pub struct SandboxIoGuard {
    _inner: Box<dyn Send>,
}

impl SandboxIo {
    pub fn new(stdin: SandboxWrite, stdout: SandboxRead, stderr: SandboxRead) -> Self {
        Self::with_guard(stdin, stdout, stderr, ())
    }

    pub fn with_guard(
        stdin: SandboxWrite,
        stdout: SandboxRead,
        stderr: SandboxRead,
        guard: impl Send + 'static,
    ) -> Self {
        Self {
            stdin,
            stdout,
            stderr,
            guard: SandboxIoGuard::new(guard),
            instance_id: None,
        }
    }

    pub fn with_instance_id(mut self, instance_id: Option<String>) -> Self {
        self.instance_id = instance_id;
        self
    }

    pub fn into_parts(self) -> SandboxIoParts {
        SandboxIoParts {
            stdin: self.stdin,
            stdout: self.stdout,
            stderr: self.stderr,
            guard: self.guard,
            instance_id: self.instance_id,
        }
    }
}

impl SandboxIoGuard {
    pub fn new(guard: impl Send + 'static) -> Self {
        Self {
            _inner: Box::new(guard),
        }
    }
}

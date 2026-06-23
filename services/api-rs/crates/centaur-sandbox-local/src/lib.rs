//! Local process sandbox backend.
//!
//! This backend is for development and manager validation. It runs one local
//! child process per sandbox and wires byte-oriented stdin/stdout/stderr through
//! the shared sandbox trait.

use std::{
    collections::HashMap,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use centaur_sandbox_core::{
    ObservedSandbox, SandboxBackend, SandboxError, SandboxHandle, SandboxId, SandboxIo,
    SandboxRead, SandboxResult, SandboxSpec, SandboxStatus, SandboxWrite,
};
use tokio::{
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

#[derive(Clone, Default)]
pub struct LocalSandboxBackend {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    next_id: AtomicU64,
    sandboxes: Mutex<HashMap<SandboxId, Arc<Mutex<LocalSandbox>>>>,
}

struct LocalSandbox {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    status: SandboxStatus,
}

impl LocalSandboxBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_id(&self) -> SandboxId {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        SandboxId::new(format!("local-{id}"))
    }

    async fn sandbox(&self, id: &SandboxId) -> SandboxResult<Arc<Mutex<LocalSandbox>>> {
        self.inner
            .sandboxes
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| SandboxError::NotFound(id.as_str().to_owned()))
    }
}

#[async_trait]
impl SandboxBackend for LocalSandboxBackend {
    fn name(&self) -> &'static str {
        "local"
    }

    async fn create(&self, spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
        let (program, args) = command_parts(&spec)?;
        let mut command = Command::new(program);
        command.args(args);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        if let Some(working_dir) = &spec.working_dir {
            command.current_dir(working_dir);
        }
        for env in &spec.env {
            command.env(&env.name, &env.value);
        }

        let mut child = command
            .spawn()
            .map_err(|err| SandboxError::backend_source("failed to spawn local sandbox", err))?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let id = self.next_id();

        self.inner.sandboxes.lock().await.insert(
            id.clone(),
            Arc::new(Mutex::new(LocalSandbox {
                child,
                stdin,
                stdout,
                stderr,
                status: SandboxStatus::Running,
            })),
        );

        Ok(SandboxHandle::new(id, self.name()))
    }

    async fn open_io(&self, id: &SandboxId) -> SandboxResult<SandboxIo> {
        let sandbox = self.sandbox(id).await?;
        let mut sandbox = sandbox.lock().await;
        let status = refresh_status(&mut sandbox).await?;

        if !status.can_open_io() {
            return Err(SandboxError::NotReady(format!(
                "local sandbox {} is {:?}",
                id.as_str(),
                status
            )));
        }

        let stdin = sandbox
            .stdin
            .take()
            .ok_or_else(|| SandboxError::io("stdin is already open or closed"))?;
        let stdout = sandbox
            .stdout
            .take()
            .ok_or_else(|| SandboxError::io("stdout is already open or closed"))?;
        let stderr = sandbox
            .stderr
            .take()
            .ok_or_else(|| SandboxError::io("stderr is already open or closed"))?;
        Ok(SandboxIo::new(
            Box::pin(stdin) as SandboxWrite,
            Box::pin(stdout) as SandboxRead,
            Box::pin(stderr) as SandboxRead,
        ))
    }

    async fn status(&self, id: &SandboxId) -> SandboxResult<SandboxStatus> {
        let sandbox = self.sandbox(id).await?;
        let mut sandbox = sandbox.lock().await;
        refresh_status(&mut sandbox).await
    }

    async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
        Ok(ObservedSandbox::new(
            id.clone(),
            self.name(),
            self.status(id).await?,
        ))
    }

    async fn list_observed(&self) -> SandboxResult<Vec<ObservedSandbox>> {
        let ids = self
            .inner
            .sandboxes
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut observed = Vec::with_capacity(ids.len());
        for id in ids {
            observed.push(self.observe(&id).await?);
        }
        Ok(observed)
    }

    async fn stop(&self, id: &SandboxId) -> SandboxResult<()> {
        let Some(sandbox) = self.inner.sandboxes.lock().await.remove(id) else {
            return Ok(());
        };
        let mut sandbox = sandbox.lock().await;

        if !sandbox.status.is_terminal() {
            let _ = sandbox.child.kill().await;
            let _ = sandbox.child.wait().await;
        }
        Ok(())
    }

    async fn pause(&self, id: &SandboxId) -> SandboxResult<()> {
        let sandbox = self.sandbox(id).await?;
        let mut sandbox = sandbox.lock().await;
        send_signal(&sandbox.child, "STOP").await?;
        sandbox.status = SandboxStatus::Suspended;
        Ok(())
    }

    async fn resume(&self, id: &SandboxId) -> SandboxResult<()> {
        let sandbox = self.sandbox(id).await?;
        let mut sandbox = sandbox.lock().await;
        send_signal(&sandbox.child, "CONT").await?;
        sandbox.status = SandboxStatus::Running;
        Ok(())
    }
}

fn command_parts(spec: &SandboxSpec) -> SandboxResult<(&str, Vec<&str>)> {
    if let Some(command) = &spec.command {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| SandboxError::InvalidSpec("command is empty".to_owned()))?;
        let mut combined_args = args.iter().map(String::as_str).collect::<Vec<_>>();
        combined_args.extend(spec.args.iter().map(String::as_str));
        return Ok((program.as_str(), combined_args));
    }

    Ok((
        spec.image.as_str(),
        spec.args.iter().map(String::as_str).collect(),
    ))
}

async fn refresh_status(sandbox: &mut LocalSandbox) -> SandboxResult<SandboxStatus> {
    match sandbox
        .child
        .try_wait()
        .map_err(|err| SandboxError::backend_source("failed to poll local sandbox", err))?
    {
        Some(_) => {
            sandbox.status = SandboxStatus::Stopped;
            Ok(SandboxStatus::Stopped)
        }
        None => {
            if matches!(sandbox.status, SandboxStatus::Suspended) {
                Ok(SandboxStatus::Suspended)
            } else {
                sandbox.status = SandboxStatus::Running;
                Ok(SandboxStatus::Running)
            }
        }
    }
}

async fn send_signal(child: &Child, signal: &str) -> SandboxResult<()> {
    let Some(pid) = child.id() else {
        return Err(SandboxError::NotReady(
            "local process has no pid".to_owned(),
        ));
    };

    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .await
        .map_err(|err| SandboxError::backend_source(format!("failed to send SIG{signal}"), err))?;

    if status.success() {
        Ok(())
    } else {
        Err(SandboxError::backend(format!(
            "kill -{signal} {pid} exited with {status}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use centaur_sandbox_core::DesiredSandboxState;
    use centaur_sandbox_manager::{DriftReason, SandboxManager};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        time::{Duration, Instant, sleep, timeout},
    };

    use super::*;

    #[tokio::test]
    async fn local_backend_round_trips_bytes_through_manager() {
        let backend = Arc::new(LocalSandboxBackend::new());
        let manager = SandboxManager::new(backend);
        let handle = manager.create_running(cat_spec()).await.unwrap();
        let mut io = manager.open_io(&handle.id).await.unwrap().into_parts();

        io.stdin.write_all(b"ping\n").await.unwrap();
        io.stdin.flush().await.unwrap();
        let mut read = vec![0; b"ping\n".len()];
        timeout(Duration::from_secs(1), io.stdout.read_exact(&mut read))
            .await
            .expect("stdout read timed out")
            .unwrap();

        assert_eq!(read, b"ping\n");
        manager.stop(&handle.id).await.unwrap();
    }

    #[tokio::test]
    async fn local_backend_open_io_write_is_not_blocked_by_pending_stdout_read() {
        let backend = Arc::new(LocalSandboxBackend::new());
        let manager = SandboxManager::new(backend);
        let handle = manager.create_running(cat_spec()).await.unwrap();
        let io = manager.open_io(&handle.id).await.unwrap().into_parts();
        let mut stdin = io.stdin;
        let mut stdout = io.stdout;
        let _guard = io.guard;

        let pending_read = tokio::spawn(async move {
            let mut read = vec![0; b"ping\n".len()];
            stdout.read_exact(&mut read).await.unwrap();
            read
        });
        sleep(Duration::from_millis(50)).await;

        timeout(Duration::from_millis(100), async {
            stdin.write_all(b"ping\n").await.unwrap();
            stdin.flush().await.unwrap();
        })
        .await
        .expect("stdin write should not wait for a stdout read timeout");

        assert_eq!(pending_read.await.unwrap(), b"ping\n");
        manager.stop(&handle.id).await.unwrap();
    }

    #[tokio::test]
    async fn local_backend_pause_resume_updates_runtime_and_desired_state() {
        let backend = Arc::new(LocalSandboxBackend::new());
        let manager = SandboxManager::new(backend);
        let handle = manager.create_running(cat_spec()).await.unwrap();

        manager.pause(&handle.id).await.unwrap();
        assert_eq!(
            manager.status(&handle.id).await.unwrap(),
            SandboxStatus::Suspended
        );
        assert!(matches!(
            manager.desired_state(&handle.id),
            Some(DesiredSandboxState::Suspended(_))
        ));

        manager.resume(&handle.id).await.unwrap();
        assert_eq!(
            manager.status(&handle.id).await.unwrap(),
            SandboxStatus::Running
        );
        assert!(matches!(
            manager.desired_state(&handle.id),
            Some(DesiredSandboxState::Running(_))
        ));

        manager.stop(&handle.id).await.unwrap();
    }

    #[tokio::test]
    async fn local_backend_reports_unexpected_process_exit_to_manager() {
        let backend = Arc::new(LocalSandboxBackend::new());
        let manager = SandboxManager::new(backend);
        let handle = manager.create_running(short_lived_spec()).await.unwrap();

        wait_for_status(&manager, &handle.id, SandboxStatus::Stopped).await;
        assert_eq!(
            manager.reconcile_one(&handle.id).await.unwrap(),
            centaur_sandbox_manager::ReconcileOutcome::Drift(DriftReason::MissingWhileRunning)
        );
        manager.stop(&handle.id).await.unwrap();
    }

    fn cat_spec() -> SandboxSpec {
        SandboxSpec::new("/bin/cat")
    }

    fn short_lived_spec() -> SandboxSpec {
        SandboxSpec::new("/bin/sh")
            .command(["/bin/sh", "-lc"])
            .args(["sleep 0.02"])
    }

    async fn wait_for_status(manager: &SandboxManager, id: &SandboxId, expected: SandboxStatus) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let actual = manager.status(id).await.unwrap();
            if actual == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {id:?} to become {expected:?}; latest status: {actual:?}"
            );
            sleep(Duration::from_millis(25)).await;
        }
    }
}

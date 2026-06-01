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
use bytes::Bytes;
use centaur_sandbox_core::{
    ExecCommand, ExecResult, ObservedSandbox, OutputStream, ReadOptions, ReadResult,
    SandboxBackend, SandboxError, SandboxHandle, SandboxId, SandboxResult, SandboxSpec,
    SandboxStatus, WriteAck,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::{Duration, timeout},
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

        let mut child = command.spawn().map_err(|err| {
            SandboxError::Backend(format!("failed to spawn local sandbox: {err}"))
        })?;

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

    async fn read_bytes(&self, id: &SandboxId, opts: ReadOptions) -> SandboxResult<ReadResult> {
        let sandbox = self.sandbox(id).await?;
        let mut sandbox = sandbox.lock().await;

        if !sandbox.status.can_read_write() {
            return Err(SandboxError::NotReady(format!(
                "local sandbox {} is {:?}",
                id.as_str(),
                sandbox.status
            )));
        }

        let mut buf = vec![0; opts.max_bytes];
        let read = match opts.stream {
            OutputStream::Stdout => {
                let stdout = sandbox
                    .stdout
                    .as_mut()
                    .ok_or_else(|| SandboxError::Io("stdout is closed".to_owned()))?;
                read_with_timeout(stdout, &mut buf, opts.timeout_ms).await?
            }
            OutputStream::Stderr => {
                let stderr = sandbox
                    .stderr
                    .as_mut()
                    .ok_or_else(|| SandboxError::Io("stderr is closed".to_owned()))?;
                read_with_timeout(stderr, &mut buf, opts.timeout_ms).await?
            }
        };

        match read {
            Some(0) => Ok(ReadResult::Eof),
            Some(n) => {
                buf.truncate(n);
                Ok(ReadResult::Bytes {
                    bytes: Bytes::from(buf),
                    stream: opts.stream,
                    start_offset: None,
                    next_offset: None,
                })
            }
            None => Ok(ReadResult::TimedOut),
        }
    }

    async fn write_bytes(&self, id: &SandboxId, bytes: Bytes) -> SandboxResult<WriteAck> {
        let sandbox = self.sandbox(id).await?;
        let mut sandbox = sandbox.lock().await;

        if !sandbox.status.can_read_write() {
            return Err(SandboxError::NotReady(format!(
                "local sandbox {} is {:?}",
                id.as_str(),
                sandbox.status
            )));
        }

        let stdin = sandbox
            .stdin
            .as_mut()
            .ok_or_else(|| SandboxError::Io("stdin is closed".to_owned()))?;
        stdin
            .write_all(&bytes)
            .await
            .map_err(|err| SandboxError::Io(format!("failed to write stdin: {err}")))?;
        stdin
            .flush()
            .await
            .map_err(|err| SandboxError::Io(format!("failed to flush stdin: {err}")))?;

        Ok(WriteAck::new(bytes.len()))
    }

    async fn close_stdin(&self, id: &SandboxId) -> SandboxResult<()> {
        let sandbox = self.sandbox(id).await?;
        let mut sandbox = sandbox.lock().await;
        sandbox.stdin.take();
        Ok(())
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

    async fn exec(&self, id: &SandboxId, command: ExecCommand) -> SandboxResult<ExecResult> {
        let sandbox = self.sandbox(id).await?;
        {
            let mut sandbox = sandbox.lock().await;
            let status = refresh_status(&mut sandbox).await?;
            if !status.can_read_write() {
                return Err(SandboxError::NotReady(format!(
                    "local sandbox {} is {status:?}",
                    id.as_str()
                )));
            }
        }

        let (program, args) = command
            .argv
            .split_first()
            .ok_or_else(|| SandboxError::InvalidSpec("exec argv is empty".to_owned()))?;
        let mut cmd = Command::new(program);
        cmd.args(args);
        for env in command.env {
            cmd.env(env.name, env.value);
        }
        if let Some(working_dir) = command.working_dir {
            cmd.current_dir(working_dir);
        }
        let output = cmd
            .output()
            .await
            .map_err(|err| SandboxError::Backend(format!("failed to run local exec: {err}")))?;
        Ok(ExecResult::new(
            output.status.code().unwrap_or(-1),
            output.stdout,
            output.stderr,
        ))
    }

    async fn interrupt(&self, id: &SandboxId) -> SandboxResult<()> {
        let sandbox = self.sandbox(id).await?;
        let sandbox = sandbox.lock().await;
        send_signal(&sandbox.child, "INT").await
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

async fn read_with_timeout<R>(
    reader: &mut R,
    buf: &mut [u8],
    timeout_ms: Option<u64>,
) -> SandboxResult<Option<usize>>
where
    R: AsyncReadExt + Unpin,
{
    if let Some(timeout_ms) = timeout_ms {
        match timeout(Duration::from_millis(timeout_ms), reader.read(buf)).await {
            Ok(result) => result
                .map(Some)
                .map_err(|err| SandboxError::Io(format!("failed to read output: {err}"))),
            Err(_) => Ok(None),
        }
    } else {
        reader
            .read(buf)
            .await
            .map(Some)
            .map_err(|err| SandboxError::Io(format!("failed to read output: {err}")))
    }
}

async fn refresh_status(sandbox: &mut LocalSandbox) -> SandboxResult<SandboxStatus> {
    match sandbox
        .child
        .try_wait()
        .map_err(|err| SandboxError::Backend(format!("failed to poll local sandbox: {err}")))?
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
        .map_err(|err| SandboxError::Backend(format!("failed to send SIG{signal}: {err}")))?;

    if status.success() {
        Ok(())
    } else {
        Err(SandboxError::Backend(format!(
            "kill -{signal} {pid} exited with {status}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use centaur_sandbox_core::{DesiredSandboxState, OutputStream};
    use centaur_sandbox_manager::{DriftReason, SandboxManager};
    use tokio::time::{Duration, Instant, sleep};

    use super::*;

    #[tokio::test]
    async fn local_backend_round_trips_bytes_through_manager() {
        let backend = Arc::new(LocalSandboxBackend::new());
        let manager = SandboxManager::new(backend);
        let handle = manager.create_running(cat_spec()).await.unwrap();

        manager
            .write_bytes(&handle.id, Bytes::from_static(b"ping\n"))
            .await
            .unwrap();
        let read = manager
            .read_bytes(
                &handle.id,
                ReadOptions {
                    stream: OutputStream::Stdout,
                    after_offset: None,
                    max_bytes: 64,
                    timeout_ms: Some(1_000),
                },
            )
            .await
            .unwrap();

        assert_eq!(read, ReadResult::stdout(Bytes::from_static(b"ping\n")));
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

    #[tokio::test]
    async fn local_backend_exec_requires_existing_running_sandbox() {
        let backend = LocalSandboxBackend::new();
        let missing = SandboxId::new("missing-local");
        assert!(matches!(
            backend
                .exec(&missing, ExecCommand::new(["/bin/true"]))
                .await,
            Err(SandboxError::NotFound(_))
        ));

        let handle = backend.create(cat_spec()).await.unwrap();
        backend.pause(&handle.id).await.unwrap();
        assert!(matches!(
            backend
                .exec(&handle.id, ExecCommand::new(["/bin/true"]))
                .await,
            Err(SandboxError::NotReady(_))
        ));

        backend.resume(&handle.id).await.unwrap();
        let result = backend
            .exec(
                &handle.id,
                ExecCommand::new(["/bin/sh", "-lc", "printf ok"]),
            )
            .await
            .unwrap();
        assert_eq!(result.stdout, b"ok");

        backend.stop(&handle.id).await.unwrap();
        assert!(matches!(
            backend
                .exec(&handle.id, ExecCommand::new(["/bin/true"]))
                .await,
            Err(SandboxError::NotFound(_))
        ));
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

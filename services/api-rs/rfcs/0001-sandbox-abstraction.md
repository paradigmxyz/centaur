# RFC 0001: Sandbox Abstraction for the Rust API Control Plane

Status: Draft
Owner: TBD
Target: `services/api-rs`

## Summary

Build the first Rust slice around a narrow sandbox runtime abstraction. The
goal is to get one isolated workload up, open byte-oriented stdio handles, stop
it, pause it, resume it, and reconcile its observed state against what the
control plane thinks should exist.

This RFC intentionally does not expose sandbox spawning over an HTTP API. The
sandbox layer should be an internal crate boundary. Higher-level concepts like
thread keys, personas, harness choice, model selection, assignment generation,
and durable execution rows belong in a later data model that can call into this
runtime layer.

## Goals

- Define a backend-neutral sandbox trait for lifecycle, owned byte I/O handles,
  and status.
- Keep `SandboxSpec` focused on the workload primitive: image/process/env/mounts,
  not Centaur agent metadata.
- Treat I/O as bytes. Protocol framing, NDJSON, Anthropic events, and harness
  translation live above the sandbox abstraction.
- Model pause and resume as first-class lifecycle operations.
- Add reconciliation as a first-class concern because the Agent Sandbox
  controller, Kubernetes, kubelet, garbage collectors, node failures, or manual
  operators may change state outside this process.
- Keep crate boundaries crisp enough for a local test backend and the production
  Agent Sandbox CRD backend.

## Non-goals

- Exposing sandbox create/stop/pause/resume through public API endpoints.
- Porting durable execution, workflow engine, chat delivery, API key issuance,
  tool discovery, or assignment management.
- Encoding Centaur-specific agent concepts in `SandboxSpec`.
- Designing the final database schema for sandbox ownership.
- Replacing the current Python API immediately.

## Existing Behavior to Preserve

The Python API currently has a sandbox backend with create, attach, stdin,
stdout, stop, status, pause, and resume operations. The useful parts to preserve
in Rust are the runtime semantics, not the app-level arguments. Auxiliary exec
and interrupt behavior should live above or beside the portable sandbox
abstraction, not on `SandboxBackend`.

The Rust abstraction should not expose attach as a public operation. Kubernetes
attach is a transport detail owned by the Agent Sandbox backend. The portable
contract should expose a single `open_io` operation that returns owned Tokio
stdin/stdout/stderr handles.

The Agent Sandbox CRD backend implements pause/resume by patching:

- pause: `spec.replicas = 0`
- resume: `spec.replicas = 1`, then wait for the workload to become ready
- stop: delete the Sandbox CRD, state PVC, prompt Secret, and proxy resources

The Rust version should keep those lifecycle semantics while stripping away
thread/persona/harness details from the sandbox contract.

The Rust control plane should not recreate the legacy raw Kubernetes Pod
backend. Kubernetes support should go through Agent Sandbox CRDs only.

## Proposed Workspace Layout

```text
services/api-rs/
  Cargo.toml
  crates/
    centaur-sandbox-core/
    centaur-sandbox-local/
    centaur-sandbox-agent-k8s/
    centaur-sandbox-manager/
    centaur-session-runtime/
  rfcs/
    0001-sandbox-abstraction.md
```

### `centaur-sandbox-core`

Owns backend-neutral runtime types:

- `SandboxBackend` trait
- `SandboxSpec`
- `SandboxId`
- `SandboxHandle`
- `SandboxStatus`
- `SandboxIo`
- `SandboxRead`
- `SandboxWrite`
- `ObservedSandbox`
- `DesiredSandboxState`
- common errors

This crate should have no Kubernetes, HTTP server, database, harness, or Centaur
agent dependencies.

### `centaur-sandbox-local`

Development and test backend. It runs a local child process, exposes owned
stdin/stdout/stderr handles, and implements the same lifecycle shape where the
host process allows it.

This backend is the fastest way to prove the abstraction without Kubernetes.

### `centaur-sandbox-agent-k8s`

Agent Sandbox CRD backend:

- translate `SandboxSpec` into `agents.x-k8s.io/v1alpha1` Sandbox resources
- configure state volume claim templates
- implement pause/resume through replica patches
- delete CRD-owned state on stop
- resolve the backing Pod for private I/O transport and status
- list observed Sandbox CRD objects and backing Pods for reconciliation

This crate owns the complete Kubernetes runtime path. It may contain private
helpers for backing-Pod I/O/status, but those helpers should not become a public
raw Pod backend.

### `centaur-sandbox-manager`

Internal orchestration over `SandboxBackend`:

- create a sandbox from a runtime-only `SandboxSpec`
- open stdio handles
- stop/pause/resume by `SandboxId`
- reconcile desired sandbox state with observed backend state
- expose an internal library API for future control-plane code

This crate should not expose HTTP. A future API layer can call this crate after
the higher-level Centaur data model is settled.

### `centaur-session-runtime`

Internal session control-plane runtime over `centaur-sandbox-manager` and the
durable session store:

- create or reuse thread sessions
- start executions and ensure the backing sandbox exists
- own sandbox workload modes that translate session intent into `SandboxSpec`
- own live sandbox stdin/stdout/stderr pumps
- frame sandbox byte streams into session events
- expose replayable session event streams for thin HTTP adapters

This crate owns protocol framing such as NDJSON lines. It may use
`tokio_util::codec::FramedRead` and `FramedWrite` over `SandboxIo`, but those
details should not live in the HTTP server crate. It should consume sandboxes
through the generic manager interface; backend selection remains startup
configuration, and workload selection remains a runtime mode, not API-server
business logic.

## Core Types

Sketch only. Supporting types such as `EnvVar`, `Mount`, `ResourceLimits`, and
`SandboxError` would live in `centaur-sandbox-core`.

```rust
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct SandboxId(String);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SandboxSpec {
    pub image: String,
    pub command: Option<Vec<String>>,
    pub args: Vec<String>,
    pub env: Vec<EnvVar>,
    pub working_dir: Option<String>,
    pub mounts: Vec<Mount>,
    pub resources: Option<ResourceLimits>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SandboxHandle {
    pub id: SandboxId,
    pub backend: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SandboxStatus {
    Created,
    Running,
    Suspended,
    Stopped,
    Gone,
    Unknown(String),
}

pub type SandboxRead = Pin<Box<dyn AsyncRead + Send>>;
pub type SandboxWrite = Pin<Box<dyn AsyncWrite + Send>>;

pub struct SandboxIo {
    stdin: SandboxWrite,
    stdout: SandboxRead,
    stderr: SandboxRead,
    guard: SandboxIoGuard,
}

pub struct SandboxIoGuard {
    _inner: Box<dyn Send>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObservedSandbox {
    pub id: SandboxId,
    pub backend: String,
    pub status: SandboxStatus,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DesiredSandboxState {
    Running(SandboxSpec),
    Suspended(SandboxSpec),
    Stopped,
}

#[async_trait]
pub trait SandboxBackend: Send + Sync {
    fn name(&self) -> &'static str;

    async fn create(&self, spec: SandboxSpec) -> Result<SandboxHandle, SandboxError>;
    async fn open_io(&self, id: &SandboxId) -> Result<SandboxIo, SandboxError>;
    async fn status(&self, id: &SandboxId) -> Result<SandboxStatus, SandboxError>;
    async fn observe(&self, id: &SandboxId) -> Result<ObservedSandbox, SandboxError>;
    async fn list_observed(&self) -> Result<Vec<ObservedSandbox>, SandboxError>;

    async fn stop(&self, id: &SandboxId) -> Result<(), SandboxError>;
    async fn pause(&self, id: &SandboxId) -> Result<(), SandboxError>;
    async fn resume(&self, id: &SandboxId) -> Result<(), SandboxError>;
}
```

## I/O Contract

The sandbox abstraction moves bytes. It does not know whether those bytes are:

- NDJSON harness events
- an interactive shell stream
- a future binary protocol
- framed messages produced by another layer

Transport rules:

- `open_io` opens one owned stdio session for a running workload.
- The returned `stdin`, `stdout`, and `stderr` handles are plain Tokio
  `AsyncWrite` / `AsyncRead` objects.
- Dropping the returned `stdin` handle closes the workload's stdin without
  stopping the sandbox.
- The sandbox layer does not append newlines, JSON-encode values, or parse
  stdout.
- Protocol framing belongs above this layer. The session runtime can wrap stdout
  with `tokio_util::codec::FramedRead` and stdin with `FramedWrite`; an HTTP
  server should only adapt those runtime events to HTTP/SSE.
- Backends may reject a second `open_io` call for the same live workload if the
  first stdio session still owns the transport.
- EOF on `stdout` or `stderr` means that stream is closed. Call `observe` or
  `status` before deciding whether the sandbox stopped.
- Pause must close or quiesce private live I/O transport before scaling or
  freezing the workload down.
- Resume must wait until the workload can accept `open_io` again.
- Kubernetes attach, stream ownership, reconnects, and TTY details are private
  backend concerns, not public API concepts.

Protocol-specific code belongs above this layer. For example, a future agent
runner can turn `turn.start` structs into NDJSON lines using `LinesCodec` and
parse stdout lines back into agent events without changing the sandbox backend.

## Output Recovery Model

Durable output recovery is not part of the first implementation milestone. The
session runtime should persist framed output above the sandbox boundary so a
future HTTP process can recover output after a crash without owning live sandbox
transport.

Kubernetes attach is not durable. If an in-process session runtime owns a live
attach stream and then crashes, bytes written by the sandbox while that runtime
is down may be lost from the live stream. Kubernetes Pod logs may recover some
output, but they are not a strong enough source of truth because log access is
timestamp/tail based, rotation can drop old bytes, and stream framing is weaker
than the sandbox API needs.

The long-term design should make output durable inside the sandbox runtime. For
Agent Sandboxes, the likely shape is a stdout/stderr spool on the state volume:

```text
/state/stdio/stdout.log
/state/stdio/stderr.log
/state/stdio/events.jsonl
```

The backend can then offer a separate recovery primitive:

```rust
async fn replay_output(
    id: &SandboxId,
    stream_name: &str,
    after_offset: u64,
    max_bytes: usize,
) -> Result<OutputChunk, SandboxError>;
```

That recovery API should prefer durable bytes. A live Kubernetes attach pump can
still exist underneath for low-latency reads, but it should be treated as an
optimization rather than the durable source of truth.

Input recovery is a higher-level problem. If the API crashes after accepting a
write but before sending it to the sandbox, or after sending it but before
persisting that fact, the sandbox layer alone cannot prove whether the workload
observed the input. A future control-plane data model should persist intended
input frames before writing them to `SandboxIo.stdin` if it needs retryable input
delivery.

## Lifecycle State Machine

```text
          create
   None ---------> Created ---------> Running
                      | ready            |
                      |                  | pause
                      |                  v
                      |              Suspended
                      |                  |
                      |                  | resume
                      v                  v
                    Stopped <--------- Running
                      ^
                      |
                    stop
```

State notes:

- `Created` means the workload resource exists but is not ready for I/O.
- `Running` means `open_io` should succeed.
- `Suspended` means durable runtime state may exist but no live process is
  serving I/O.
- `Stopped` means the control plane intentionally cleaned up the sandbox.
- `Gone` means the backend cannot find the workload and the manager did not
  observe an intentional stop.
- `stop` should be valid from `Created`, `Running`, or `Suspended`.
- `pause` should return `Unsupported` for backends that cannot retain state.

## Reconciliation Model

The sandbox manager should not assume it is the only actor touching lifecycle.
The Agent Sandbox controller, Kubernetes controllers, kubelet, garbage
collectors, node failures, or manual operators can all change the observed
world.

The clean split is:

- desired state: owned by a future Centaur data model
- observed state: read from the runtime backend
- reconciliation: a small manager loop that compares desired and observed state
  and issues backend operations

This RFC does not design the final desired-state database. It only requires the
sandbox layer to expose enough primitives for reconciliation.

### Backend Responsibilities

Each backend should be able to:

- create a workload for a `SandboxSpec`
- observe one workload by ID
- list workloads owned by this control plane
- map native runtime state into `SandboxStatus`
- make lifecycle operations idempotent where possible
- surface backend-owned diagnostic reasons for ambiguous observations

### Manager Responsibilities

The manager should:

- never treat in-memory state as authoritative
- persist desired state somewhere above this RFC before production use
- call `observe` after create/pause/resume/stop instead of trusting the API call
  response
- periodically call `list_observed` and classify drift
- repair drift when there is an unambiguous desired action
- surface ambiguous drift for the higher-level control plane to decide

Example drift handling:

| Desired | Observed | Manager action |
|---------|----------|----------------|
| Running | Gone | recreate or report lost, depending on owner policy |
| Running | Suspended | resume |
| Suspended | Running | pause |
| Stopped | Running | stop |
| Stopped | Gone | no-op |
| Running | Unknown | report unhealthy and retry observe |

The important design constraint is that reconciliation should be backend-neutral
at the manager layer. Kubernetes-specific details stay in the backend's
`observe` and `list_observed` implementations.

## Agent Sandbox Backend Design

Use the `kube` crate and generated or dynamic Kubernetes API types inside
`centaur-sandbox-agent-k8s`:

- `kube` for clients, watches, API calls, and private attach transport
- `k8s-openapi` for backing Pod, Secret, PVC, and NetworkPolicy types
- `serde_json` for dynamic Agent Sandbox CRD payloads unless we add typed CRDs

The Agent Sandbox backend should own:

- translating `SandboxSpec` into a Sandbox CRD
- state volume claim templates
- deterministic sandbox names or caller-supplied IDs
- companion resource construction when needed
- readiness wait through the controller-owned backing Pod
- private attach transport for `open_io`
- pause/resume patches
- deletion of the Sandbox CRD, state PVC, and companion resources
- observation of both the Sandbox object and backing Pod
- list/watch support for reconciliation

The backing Pod remains an implementation detail. For the current controller
shape, the Pod name may match the sandbox ID, but that assumption should live
behind a resolver inside `centaur-sandbox-agent-k8s`.

The backend should use non-TTY stdio transport by default. TTY mode merges
streams and introduces terminal behavior that should not leak into the sandbox
byte API.

## First Milestone

The first usable slice should prove:

1. `centaur-sandbox-core` compiles with lean workload types and owned byte I/O.
2. `centaur-sandbox-local` can run a scripted child process and round-trip raw
   bytes through `SandboxIo`.
3. `centaur-sandbox-manager` can create, open I/O, and stop using the local
   backend.
4. The manager can reconcile a small in-memory desired-state map against the
   local backend's observed state.
5. `centaur-sandbox-agent-k8s` can create a real Agent Sandbox CRD and open
   stdio handles through the backing Pod.
6. `centaur-sandbox-agent-k8s` can pause and resume by patching replicas, then
   observe the resulting state.

Suggested local proof shape:

```rust
let spec = SandboxSpec {
    image: "local-scripted".to_string(),
    command: Some(vec!["./scripted-wrapper".to_string()]),
    args: vec![],
    env: vec![],
    working_dir: None,
    mounts: vec![],
    resources: None,
};

let handle = manager.create(spec).await?;
let mut io = manager.open_io(&handle.id).await?.into_parts();
io.stdin.write_all(b"ping\n").await?;
io.stdin.flush().await?;
let mut chunk = vec![0; b"ping\n".len()];
io.stdout.read_exact(&mut chunk).await?;
assert_eq!(chunk, b"ping\n");
manager.stop(&handle.id).await?;
```

## Testing Strategy

- Unit test owned I/O handle behavior in `centaur-sandbox-core`.
- Use `centaur-sandbox-local` for deterministic manager and reconciliation tests.
- Mock Kubernetes at the crate boundary for Sandbox CRD spec, replica patch,
  backing Pod private attach/status, and observed state mapping tests.
- Add ignored or feature-gated local Kubernetes integration tests that create a
  Sandbox CRD in the `centaur` namespace and round-trip bytes.
- Keep durable output spool tests out of the first milestone.
- Keep pause/resume tests focused on externally visible behavior:
  - pause closes I/O and patches/scales/freezes the workload
  - status reports `Suspended`
  - resume restores the workload and waits for readiness
  - `open_io` works again after resume
- Add reconciliation tests for stale state:
  - observed `Gone` while desired `Running`
  - observed `Running` while desired `Stopped`
  - observed `Suspended` while desired `Running`

## Open Questions

- Should `SandboxId` be generated by the manager or supplied by the caller so a
  higher-level data model can make IDs stable?
- Should `SandboxSpec` include labels/annotations, or should those be backend
  options outside the portable spec?
- Should stderr be exposed as a separate byte stream for all backends, or folded
  into backend diagnostics?
- Should reconciliation be poll-first, watch-first, or both?

## Recommendation

Start with `centaur-sandbox-core`, `centaur-sandbox-local`, and
`centaur-sandbox-manager`. Prove the byte I/O and reconciliation boundary before
touching Kubernetes. Then implement Agent Sandbox create/open I/O/stop, followed
by Agent Sandbox pause/resume and reconciliation.

The core rule should be: the sandbox crate manages isolated runtime workloads,
not Centaur agent semantics. Higher-level control-plane code can decide what the
bytes mean and why the sandbox exists.

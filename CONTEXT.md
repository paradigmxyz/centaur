# Centaur Domain Language

This file records product and architecture terms whose meaning spans services.
It is deliberately short. Detailed behavior belongs in the linked design
documents and service contracts.

## Conversation And Execution

- **Channel Binding** maps a platform conversation identity to one durable
  Centaur Session. The platform key is transport-specific; the Session is not.
- **Session** is the durable conversation and task history owned by `api-rs`.
- **Execution** is one serialized agent run within a Session. An Execution may
  wait for an external prerequisite, such as repository selection or Workspace
  provisioning, before a sandbox is started.

## Development Workspace

- **Repository** is an opaque, provider-backed source repository that the
  configured Centaur credential can resolve. Callers use a Centaur
  `repository_id`; they do not supply clone URLs.
- **Workspace** is the persistent development environment owned by exactly one
  Session. A sandbox is temporary compute attached to it.
- **Workspace Repository** binds a Repository to a Workspace and records its
  local path, base revision, working branch, and provisioning state.
- **ChangeSet** is the immutable review record for the ordinary Git commits
  produced by one completed Execution across one or more Workspace
  Repositories. It records exact base and head SHAs, diff metadata, and test
  evidence; it does not create a second hidden Git snapshot.
- **Publish Batch** is one approved attempt to publish a ChangeSet.
- **Publish Item** is the per-repository result inside a Publish Batch. A retry
  creates work only for failed items and preserves successful merge requests.

## Canonical Design

The first-release Feishu and Web Chat development workflow is specified in
[`docs/superpowers/specs/2026-08-13-feishu-web-multi-repository-development-flow-design.md`](docs/superpowers/specs/2026-08-13-feishu-web-multi-repository-development-flow-design.md).

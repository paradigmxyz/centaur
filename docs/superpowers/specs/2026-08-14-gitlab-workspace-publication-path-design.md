# GitLab Workspace Publication Path Design

## Problem

Development sandboxes start in `/workspace`, but their generic runtime prompt still
instructs agents to create writable clones under `~/branches`. The provisioner also
runs as UID 1000 while the sandbox agent runs as UID 1001, leaving prepared
repositories unreadable for writes by the agent. As a result, commits can land in an
ephemeral clone that ChangeSet collection never sees.

The local deployment has the GitLab Secret configured, but GitLab publication is
disabled. Direct `git push` from a sandbox cannot replace publication: sandboxes
intentionally have no GitLab credential and their proxy policy excludes the private
GitLab endpoint.

## Selected Design

Keep GitLab credentials confined to provisioner and publisher jobs.

1. Run provisioner, collector, and publisher containers as UID/GID 1001, matching the
   default `centaur-agent` image. New Workspace files, repositories, and manifests are
   therefore directly writable or readable by the sandbox agent.
2. Append a development-Workspace-specific block after the generic and overlay system
   prompts. It makes `/workspace/repos/*` authoritative, forbids `git-branch` and
   `~/branches` for selected repositories, and tells the agent to commit locally and
   let Centaur collect the change for publication approval.
3. Enable `gitlabPublishing` in the ignored local values file using the existing
   GitLab Secret reference. Keep the shared Chart default disabled because enabling a
   write-capable integration without an explicit deployment Secret is unsafe and
   makes the default Chart invalid when workspaces are disabled.
4. Preserve the current sandbox commit by moving it into the durable Workspace before
   restarting or deleting that sandbox.

The Feishu flow remains: agent commits in the Workspace, Collector emits a ChangeSet,
Feishu renders the `创建 MR` action, and an approved short-lived Publisher job pushes
the exact reviewed SHA and creates the merge request.

## Rejected Alternatives

- Injecting the GitLab token into every sandbox would expand credential exposure and
  bypass the existing approval boundary.
- Allowing direct private-network GitLab egress from every sandbox would still leave
  authentication and publication auditing unresolved.
- Copying or symlinking Workspace repositories into `~/branches` would create two
  competing working trees and preserve the path ambiguity.

## Validation

- Unit tests prove development prompts override the generic branch workflow.
- Rust tests prove every Workspace lifecycle job uses UID/GID 1001.
- Sandbox and workspace-manager tests pass with the current prompt and auth changes.
- Helm rendering with the local values includes the publisher ServiceAccount and
  `GITLAB_PUBLISHING_ENABLED=true`.
- A fresh local sandbox can write the selected repository, its ChangeSet is non-empty,
  and approval reaches GitLab through the Publisher job.

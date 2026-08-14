# GitLab Workspace Authentication Fix

## Goal

Allow Centaur to clone repositories from the configured GitLab using only the
existing token, including older GitLab installations that reject `oauth2` as
the HTTP Basic username. Surface workspace preparation failures as terminal
execution failures instead of leaving Feishu on "工作区准备中".

## Design

The workspace provisioner already receives the GitLab token as a mounted
Secret and receives clone URLs for one configured GitLab deployment. Before
cloning, it derives that deployment's origin from the first clone URL, calls
the GitLab v3 `/user` endpoint with the mounted token, validates the returned
username, and uses it through `GIT_ASKPASS`. The username is never added to
configuration, persisted, logged, or embedded in a clone URL.

The short-lived publication push Job resolves the username from the same v3
endpoint before its `ls-remote` and `push` operations. Merge-request API calls
continue to authenticate directly with the token header and need no username.

When workspace preparation reports any failed repository, the session store
marks the blocked execution as failed, clears its blocking reason, records a
bounded error, and appends the standard execution failure event. Feishubot
treats that event as terminal and replaces the preparation card with a failed
card. The workspace reconciliation loop also repairs legacy rows already left
in the failed-workspace/blocked-execution state, idempotently emitting the same
terminal event once.

## Error Handling

Identity lookup, malformed identity responses, clone authentication errors,
and clone failures remain bounded as repository preparation failures. Tokens,
usernames, upstream bodies, and Git stderr are not persisted or logged.

## Validation

- Unit-test the generated provisioner job script and its username lookup.
- SQLx-test that a failed workspace preparation fails and unblocks execution.
- Test that Feishubot renders the standard execution failure as terminal.
- Run the relevant Rust and Feishubot test suites.
- Build and deploy the affected local images.
- Re-run the real Feishu selection flow and verify workspace, execution,
  delivery, and user-visible card state.

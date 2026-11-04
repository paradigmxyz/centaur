---
name: gcloud
description: "Use the gcloud CLI from a Centaur sandbox, where iron-proxy injects Google Cloud credentials on the wire. Use before any gcloud command, Google Cloud deploy, Cloud Run, Cloud Build, Artifact Registry, Cloud Storage, or IAM task, and whenever gcloud reports no credentialed account, asks you to run `gcloud auth login`, or a Google API returns 401, 403, or SERVICE_DISABLED."
---

# gcloud in a Centaur sandbox

The sandbox has no local Google login. When the current principal holds a GCP credential grant, iron-proxy adds a real `Authorization` header to each Google API request as it leaves the sandbox. So `gcloud auth list` shows no credentialed account even when access works. **Never report "no GCP credentials" or "no authenticated account" because of `gcloud auth list` or `gcloud config get-value account`.** Make a real call first.

## Setup

gcloud refuses to send requests without a local credential. Give it a placeholder token, which the proxy replaces:

```bash
printf 'placeholder' > /tmp/gcp-placeholder-token
export CLOUDSDK_AUTH_ACCESS_TOKEN_FILE=/tmp/gcp-placeholder-token
gcloud run services list --project=<project> --region=<region>
```

Set the variable in the same shell command as the gcloud call, because shell state does not persist between tool calls. `--access-token-file=/tmp/gcp-placeholder-token` on a single command works too. With the variable set, `gcloud auth print-access-token` prints `placeholder`. Scripts that pass that value in an `Authorization` header still work, because the proxy overwrites the header.

## Rules

- Always pass `--project`. There is no default project.
- If a call fails with `SERVICE_DISABLED` for an API in a project you did not name (often Cloud Resource Manager), gcloud billed the request to the wrong quota project. Retry with `--billing-project=<a project where that API is enabled>`.
- Before acting, find out which identity you have. It depends on the surface:
  - DMs usually use the requester's personal Google OAuth.
  - Shared channels use service accounts granted to the channel.
  - A principal can hold several GCP credentials, scoped to different Google API hosts, so Cloud Run and Cloud Storage may act as different accounts.
- To see the principal for a host, make a harmless read that is likely to be denied. Storage errors name the caller: `gcloud storage ls gs://<bucket-you-cannot-read>` → `<principal> does not have storage.objects.list access`.
- Personal credentials act as a real person. Stay within the project the user named, read before you write, and ask before any destructive or IAM-changing command unless the user already asked for that exact change.
- Never print, log, or store real tokens. You never hold one anyway, so do not try to extract one.

## Known limits

- **ID tokens cannot be minted.** `gcloud auth print-identity-token` fails with `No identity token can be obtained from the current credentials`. Calls to private Cloud Run services work only for hosts where the deployment configured an ID-token credential. Anything else, including smoke tests that call `print-identity-token`, has to be run by a human from an authenticated shell. Give them the exact command.
- **Scripts that read `gcloud config get-value account` get an empty value.** Read the script before running it, and pass the account explicitly or hand that step to a human.
- **`bq` fails TLS verification through the proxy.** Use `gcloud` or a Google client library instead.
- **Client libraries** start from the sandbox's mock `GOOGLE_APPLICATION_CREDENTIALS`. They work when the proxy handles the token exchange for that credential. If you see `invalid_grant: account not found` or signing errors for `mock@creds.com`, switch to gcloud. Signed URLs and `signBlob` never work.

## Failures

| Symptom | Meaning | Action |
|---|---|---|
| `You do not currently have an active account selected` | No placeholder token was set. | Set `CLOUDSDK_AUTH_ACCESS_TOKEN_FILE` and retry. |
| `401 ACCESS_TOKEN_TYPE_UNSUPPORTED`, `Invalid Credentials` | The proxy did not inject a usable token for that host, usually because the principal has no GCP grant covering it. | Report the missing grant. Follow the `auth-failure-log-triage` skill for proxy evidence. |
| `403 PERMISSION_DENIED` naming a permission | The injected identity lacks IAM for that action. | Report the principal, the permission, and the resource. Offer the `gcloud ... add-iam-policy-binding` command for an owner to run. Do not retry with another identity. |
| `SERVICE_DISABLED` for the target project | The API is off in that project. | Ask an owner to run `gcloud services enable <api> --project=<project>`, unless you were asked to enable it and have `serviceusage.services.enable`. |

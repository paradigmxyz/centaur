# RFC 0005: Transparent MPP Egress

Status: Draft
Owner: TBD
Target: `services/iron-proxy`, `services/api-rs`, `services/console`

## Summary

Add Machine Payments Protocol (MPP) handling to Centaur's existing per-sandbox
egress boundary. A sandbox makes an ordinary HTTP request. When the upstream
returns a valid `402 Payment Required` challenge, the proxy checks policy,
obtains a payment credential from a host-side provider, and replays the exact
request. Neither the harness nor the tool needs to understand MPP or hold a
wallet.

This follows the composable egress design demonstrated by
[`gakonst/nanocodex#70`](https://github.com/gakonst/nanocodex/pull/70), but MPP
must be implemented inside iron-proxy's upstream round-trip path. Chaining a
second TLS-intercepting proxy would duplicate certificate authorities, request
buffering, destination policy, and audit boundaries.

## Goals

- Make MPP payment transparent to harnesses and ordinary HTTP clients.
- Keep wallet keys, payment credentials, budgets, and policy outside the
  sandbox.
- Compose with existing iron-proxy secret replacement and request policy.
- Replay the transformed request exactly once within strict size, time, retry,
  destination, and price bounds.
- Attribute every attempted payment to a Centaur principal, session,
  execution, destination, and challenge without logging credentials.
- Allow a deployment to start with wallet-backed services and progressively
  prefer native credentialed tools as credentials are added.

## Non-goals

- Teaching the model, harness, or every tool client the MPP protocol.
- Treating the MPP service registry as an authorization source.
- Paying arbitrary `402` responses or following challenge-directed URLs.
- Supporting subscriptions, arbitrary currencies, or multiple automatic
  retries in the first release.
- Moving service discovery into the egress proxy.

## Current Centaur Boundary

Each Kubernetes sandbox receives `HTTP_PROXY`, `HTTPS_PROXY`, and the firewall
CA for its per-sandbox iron-proxy. iron-proxy terminates TLS, applies the
principal's request transforms and credential grants, sends the upstream
request, applies response transforms, and emits an audit result.

This is the correct trust boundary: the sandbox already cannot reach external
services directly, while iron-proxy already knows the authenticated principal
and the final transformed request.

However, iron-proxy 0.49.0 cannot implement paid replay as a transform:

- `Transformer.TransformResponse` can inspect, modify, reject, or replace a
  response, but it cannot perform another upstream round trip.
- the proxy's `http.Transport` is private to `internal/proxy`;
- response transforms receive the original client request rather than a
  replay factory for the final transformed upstream request; and
- request and response bodies are buffered for transforms, but ownership is
  not exposed as a bounded replay contract.

Configuration alone is therefore insufficient.

## Proposed Request Flow

```text
sandbox HTTP client
  -> per-sandbox iron-proxy
     -> request transforms (allowlist, secret replacement, signing)
     -> buffer final upstream request within replay limit
     -> upstream request
     <- 402 + WWW-Authenticate: Payment ...
     -> MPP policy (principal, host/path/method, intent, amount, budget)
     -> host-side payment provider
     <- short-lived payment credential
     -> replay exact final upstream request with Authorization: Payment ...
     <- response + Payment-Receipt
     -> response transforms
  <- final response
```

Secret replacement runs once. The replay is built from the already-transformed
upstream request, so it cannot re-resolve or accidentally expose a secret to a
different destination.

## Required iron-proxy Change

Add a transport-level response handler after the initial upstream round trip
and before the existing response-transform pipeline. This is deliberately not
part of `Transformer`, whose contract remains deterministic request/response
mutation.

The upstream API should be generic rather than MPP-specific:

```go
type ResponseHandler interface {
    Name() string
    HandleResponse(
        ctx context.Context,
        meta RequestMetadata,
        replay ReplayRequest,
        response *http.Response,
    ) (*http.Response, error)
}

type ReplayRequest interface {
    NewRequest(ctx context.Context) (*http.Request, error)
    MaxBodyBytes() int64
}
```

`ReplayRequest` is created only after request transforms and upstream routing
finish. It snapshots method, final URL, sanitized headers, content length, and
bounded body bytes. It must not expose iron-proxy's transport, accept a new
destination, or permit unbounded/multiple replays.

The MPP handler then:

1. passes through non-`402` responses;
2. accepts only a syntactically valid MPP challenge from
   `WWW-Authenticate`;
3. validates the challenge against configured method, intent, currency,
   amount, recipient, and original request origin;
4. asks a `PaymentProvider` for a credential under a budget reservation;
5. creates one replay, adds only the MPP `Authorization` credential, and uses
   the same guarded transport;
6. commits the reservation only after the configured receipt condition, or
   records an indeterminate outcome when safe rollback cannot be proven; and
7. returns the replay response to the normal response-transform pipeline.

The handler must disable redirects for both attempts. A redirect response is
returned to the sandbox and never paid automatically.

## Centaur Control-plane Changes

### Payment provider

Add an MPP provider interface owned outside the sandbox. The initial Tempo
implementation should use a deployment wallet or delegated account, but
iron-proxy receives only the ability to request a bounded credential. Long-lived
wallet material must not enter the proxy configuration or sandbox pod.

The preferred production shape is an authenticated control-plane endpoint:

```text
iron-proxy -> POST /internal/mpp/authorize -> policy + budget reservation
iron-proxy -> POST /internal/mpp/settle    -> receipt/outcome/audit completion
```

Requests bind the proxy identity, principal, session/execution attribution,
challenge hash, destination, method, path policy key, and amount. Returned
credentials are single-challenge and short-lived.

### Policy

MPP grants are distinct from credential grants. A safe initial schema is:

```yaml
default: deny
allow:
  services: [parallel]
  providers: ["*.approved.example"]
  categories: [search, market-data]
  effects: [read]
deny:
  categories: [gambling]
  effects: [write, send-message, transaction, subscription]
limits:
  max_charge_atomic: 100000
  max_session_atomic: 1000000
  max_principal_daily_atomic: 10000000
  max_request_body_bytes: 1048576
  max_challenge_body_bytes: 1048576
  max_replays: 1
```

Hard deny overrides exact service, provider, category, and default rules.
Unclassified services default to denied. HTTP method is evidence, not an
effect classification: a `GET` can still purchase or trigger an external
effect.

Registry metadata may propose a category, effect, and expected price, but the
live challenge and Centaur policy are authoritative. A challenge may never
expand the request's original host, method, or path authorization.

### Audit and durability

Persist a payment attempt before signing so process or pod failure cannot make
the spend invisible. Store:

- attempt ID, principal, session, execution, proxy, and request correlation;
- service, origin, method, normalized path policy key, challenge hash, payment
  method/intent/currency, recipient, and amount;
- reservation, credential-created, replayed, committed, rolled-back, or
  indeterminate state; and
- receipt hash and safe non-secret receipt metadata.

Never store or log the payment credential, proxy authorization, wallet key,
secret-replaced headers, or request/response bodies by default.

## Discovery Is Separate

Transparent payment does not tell an agent that a service exists or how to
construct its request. Centaur's capability resolver should independently
choose:

```text
authorized healthy native tool
  -> otherwise compatible authorized MPP registry service
  -> otherwise unavailable
```

The resolver provides endpoint and schema information. The normal HTTP client
makes the request, and iron-proxy handles any payment challenge invisibly.
This separation also allows a native API-key-backed tool to replace an MPP
fallback without changing payment code.

## Failure Semantics

- Invalid, unsupported, over-budget, or denied challenges are returned as
  `402` with a sanitized machine-readable denial header; no payment occurs.
- Oversized request or challenge bodies are not replayable and fail closed.
- A provider timeout before credential creation releases the reservation.
- Failure after credential creation is recorded as indeterminate unless the
  payment method proves rollback is safe.
- A second `402` is returned without another automatic payment.
- Client cancellation cancels unpaid work, but cannot erase a payment attempt
  that may already have been submitted.
- Proxy or control-plane restarts reconcile durable reservations before
  allowing the same challenge to be paid again.

## Delivery Plan

1. Add the bounded `ResponseHandler`/`ReplayRequest` hook and concurrency tests
   upstream in iron-proxy.
2. Add an MPP handler with a fake provider and integration tests proving exact
   transformed-body replay, one retry, redirects disabled, and fail-closed
   limits.
3. Add durable authorization/reservation endpoints and policy storage to the
   Centaur control plane and Console.
4. Pin Centaur's wrapper image to the reviewed iron-proxy release and wire
   provider identity/configuration into each per-sandbox proxy.
5. Run an end-to-end sandbox test against a controlled paid origin, verifying
   the sandbox contains neither wallet material nor a payment credential.
6. Add registry-backed capability fallback only after egress payment policy and
   accounting are proven.

## Rejected Alternatives

### Chain nanocodex's proxy in front of iron-proxy

Two MITM proxies create ambiguous ordering, two CAs, duplicate buffering, and
split audit/policy. If MPP runs first it sees secrets/placeholders incorrectly;
if it runs second, wallet policy lacks Centaur principal context. It also makes
CONNECT and `NO_PROXY` behavior harder to reason about.

### Implement MPP in each tool

This leaks protocol and wallet concerns into every integration and does not
cover arbitrary shell or harness HTTP clients.

### Extend response transforms with the raw transport

Giving arbitrary transforms transport access permits destination changes,
unbounded retries, and bypass of proxy routing and audit invariants. A bounded
single-use replay capability is the narrower contract.

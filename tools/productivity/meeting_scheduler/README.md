# Meeting Scheduler

This is a narrow, workflow-owned Calendar + Zoom capability for Centaur. It
should not be exposed directly to user-facing principals; an application
workflow should provide confirmation and authorization before invoking writes.
It returns free/busy-derived slots only and writes to managed organizer
aliases. Manual meetings are always created on Centaur's managed calendar, with
the verified proposer included as an attendee. Centaur-created events allow
guests to modify event details and invite others, giving participants
event-level collaboration without requiring write access to an employee
calendar. Every write is keyed by a stable
occurrence identity and is recorded in `centaur_meeting_occurrences` before
provider work begins.

Production Zoom access is a `brokered_token` minted from the dedicated
User-managed General OAuth app. Centaur Console serializes refreshes and stores
each rotated refresh token; the scheduler receives only the current bearer.
All meetings are owned by the configured Centaur Zoom user. Centaur never uses
`schedule_for` or another user's identity to create a room. Zoom requests use
the OAuth identity-bound `/users/me` endpoint, so the authorizing Centaur account
is always the owner. When Zoom rejects a request, the raised error keeps the
HTTP status and a bounded, redacted copy of Zoom's `code`, `message`, and
field-level `errors`; headers, URLs, tokens, and email addresses are never
retained. Booking failures persist that reason in the occurrence's
`last_error`. For crash-recovery discovery, each room carries an opaque hash of
the occurrence identity in Zoom's free-form agenda. The scheduler intentionally
does not send Zoom `tracking_fields`, because those fields must first be
configured account-wide by a Zoom administrator and otherwise make meeting
creation fail with HTTP 400. Legacy tracking-field discovery remains supported
for rooms created before this marker was introduced. Every created room
requests cloud recording; `get_recording`
returns bounded transcript content and `get_summary`
returns the AI Companion summary after Zoom has finished processing, without
exposing signed provider URLs.

Zoom ownership remains with Centaur. For confirmed ad-hoc bookings, the
authenticated proposer is also assigned as the Zoom alternative host, which
gives them host controls (including ending the room for everyone) without
delegating API ownership or Calendar identity. The confirmation-gated
`end_meeting` operation remains available as a fallback and lets an authorized
requester ask Centaur to end its own Zoom meeting through the provider status API.
It only accepts a recorded Centaur occurrence and deliberately keeps the Calendar
event intact so recording processing and post-meeting follow-up retain their
source metadata.

Post-meeting operations expose processed summaries, bounded transcript content,
lease state, and delivery markers. A durable workflow can compose these with
Notion, Slack, or another destination while preserving idempotency. Missing or
still-processing Zoom artifacts remain retryable.

Client credentials and token material must not appear in a cadence, skill,
Slack message, repository, Terraform state, or runtime environment variable.
Live activation also requires Calendar read/event scopes and explicit organizer
aliases for managed meetings.

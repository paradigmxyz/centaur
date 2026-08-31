-- Memory lifecycle writes stay behind the typed workflow API.  In particular,
-- the run role does not need to delete facts or their immutable audit events;
-- forget is implemented as a scrubbed tombstone plus evidence deletion.
revoke delete on memory_facts, memory_fact_events from centaur_heartbeat_run;
revoke delete on heartbeat_runs, heartbeat_source_checkpoints,
    heartbeat_observations, heartbeat_items, heartbeat_item_observations,
    heartbeat_item_events, heartbeat_run_artifacts, heartbeat_deliveries,
    heartbeat_action_tokens from centaur_heartbeat_run;

-- A delivery retry is keyed by client_message_id, but this second invariant
-- prevents a future caller from creating duplicate live buttons for one fact
-- revision/action inside the same delivery.
create unique index if not exists heartbeat_action_tokens_active_target_idx
    on heartbeat_action_tokens (delivery_id, item_id, item_version, action)
    where consumed_at is null;

comment on table memory_facts is
    'Canonical scoped semantic facts; lifecycle mutations require reviewer/admin authorization and optimistic revisions.';

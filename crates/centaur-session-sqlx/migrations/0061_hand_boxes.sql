-- Hands: single-use warm-pool pods leased to nanocode threads.
-- The lease row is authoritative; pods and iron-control proxies are reaped
-- when it is deleted. Lease and thread identities only exist while leased.
create table if not exists hand_boxes (
    id text primary key,
    size text not null,
    state text not null,
    pod_name text not null,
    pod_ip text,
    iron_proxy_id text,
    lease_id text,
    thread_key text,
    principal text,
    requester text,
    leased_at timestamptz,
    lease_expires_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint hand_boxes_size_supported
        check (size in ('default', 'large')),
    constraint hand_boxes_state_supported
        check (state in ('warming', 'ready', 'leased', 'deleting'))
);

-- One live lease per thread. The partial index is the concurrency backstop
-- behind the pool's find-then-claim path.
create unique index if not exists hand_boxes_live_lease_thread_idx
    on hand_boxes (thread_key)
    where state = 'leased';

create unique index if not exists hand_boxes_lease_id_idx
    on hand_boxes (lease_id)
    where lease_id is not null;

create index if not exists hand_boxes_claim_idx
    on hand_boxes (size, state, created_at)
    where state = 'ready';

create index if not exists hand_boxes_state_idx
    on hand_boxes (state, updated_at);

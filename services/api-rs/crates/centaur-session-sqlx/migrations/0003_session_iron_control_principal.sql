-- Persist the iron-control principal OID a session's egress proxy binds to.
-- Captured once at session registration so a resumed session can recreate its
-- sandbox (and per-sandbox proxy) after an api-rs restart without re-deriving
-- the principal — which needs the slack_user_id only available at creation.
alter table sessions
    add column if not exists iron_control_principal text;

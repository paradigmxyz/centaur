alter table sessions
    add column if not exists persona_id text;

alter table sessions
    add constraint sessions_persona_id_len
    check (persona_id is null or octet_length(persona_id) between 1 and 128);

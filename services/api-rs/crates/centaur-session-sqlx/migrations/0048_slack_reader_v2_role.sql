do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'centaur_slack_reader_v2') then
        create role centaur_slack_reader_v2 nologin;
    end if;
end
$$;

grant usage on schema public to centaur_slack_reader_v2;
grant centaur_slack_reader_v2 to current_user;

create or replace function centaur_current_slack_history_channel_ids()
returns text[]
language plpgsql
stable
as $$
declare
    raw text := nullif(current_setting('centaur.slack_history_channel_ids', true), '');
    parsed jsonb;
begin
    if raw is null then
        return array[]::text[];
    end if;

    begin
        parsed := raw::jsonb;
    exception when others then
        return array[]::text[];
    end;

    if jsonb_typeof(parsed) <> 'array' then
        return array[]::text[];
    end if;

    return array(select jsonb_array_elements_text(parsed));
end;
$$;

create or replace function centaur_slack_reader_v2_include_public()
returns boolean
language sql
stable
as $$
    select current_setting('centaur.slack_include_public', true) = 'true'
$$;

revoke all on function centaur_current_slack_history_channel_ids() from public;
revoke all on function centaur_slack_reader_v2_include_public() from public;
grant execute on function centaur_current_slack_history_channel_ids()
    to centaur_slack_reader_v2;
grant execute on function centaur_slack_reader_v2_include_public()
    to centaur_slack_reader_v2;

grant select on
    slack_sync_channels,
    company_context_documents
to centaur_slack_reader_v2;

drop policy if exists centaur_slack_reader_v2_channels_select
    on slack_sync_channels;
create policy centaur_slack_reader_v2_channels_select
    on slack_sync_channels
    for select
    to centaur_slack_reader_v2
    using (
        channel_id = any(centaur_current_slack_history_channel_ids())
        or (
            centaur_slack_reader_v2_include_public()
            and not is_private
        )
    );

drop policy if exists centaur_slack_reader_v2_documents_select
    on company_context_documents;
create policy centaur_slack_reader_v2_documents_select
    on company_context_documents
    for select
    to centaur_slack_reader_v2
    using (
        source = 'slack'
        and (
            metadata ->> 'channel_id' = any(centaur_current_slack_history_channel_ids())
            or (
                centaur_slack_reader_v2_include_public()
                and exists (
                    select 1
                    from slack_sync_channels channels
                    where channels.channel_id = metadata ->> 'channel_id'
                      and not channels.is_private
                )
            )
        )
    );

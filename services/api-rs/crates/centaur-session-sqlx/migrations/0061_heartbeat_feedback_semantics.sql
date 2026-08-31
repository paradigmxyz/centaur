create or replace function heartbeat_consume_action(
    p_token_hash text, p_actor_ref text, p_provider_event_key text
) returns jsonb language plpgsql security definer
set search_path = pg_catalog, public
as $$
declare token_row record; item_row record; allowed boolean; to_status text;
    disposition_value text; snooze_value timestamptz; new_version integer;
    event_key text; result_value jsonb; draft_token text; draft_hash text;
begin
    select t.*, d.run_id, r.profile_id into token_row
      from public.heartbeat_action_tokens t
      join public.heartbeat_deliveries d on d.delivery_id=t.delivery_id
      join public.heartbeat_runs r on r.run_id=d.run_id
     where t.token_hash=p_token_hash for update of t;
    if not found then raise exception 'heartbeat action token is invalid' using errcode='42501'; end if;
    if token_row.consumed_at is not null then
        if token_row.provider_event_key=p_provider_event_key and token_row.consumed_by_principal=p_actor_ref and token_row.result is not null then return token_row.result; end if;
        raise exception 'heartbeat action token is already used' using errcode='42501';
    end if;
    if token_row.expires_at<=now() then raise exception 'heartbeat action token is expired' using errcode='42501'; end if;
    select exists(select 1 from public.heartbeat_profile_grants where profile_id=token_row.profile_id
      and permission in ('review','admin') and subject_kind='principal' and subject_ref=p_actor_ref) into allowed;
    if not allowed then raise exception 'actor is not a heartbeat reviewer' using errcode='42501'; end if;
    select * into item_row from public.heartbeat_items where item_id=token_row.item_id for update;
    if not found or item_row.version<>token_row.item_version then raise exception 'heartbeat item changed after this action was rendered' using errcode='40001'; end if;
    to_status:=item_row.status; disposition_value=item_row.disposition; snooze_value=item_row.snooze_until;
    if token_row.action in ('approve','park') then to_status:='resolved'; disposition_value:=token_row.action;
    elsif token_row.action='assign' then to_status:='open'; disposition_value:='assign';
    elsif token_row.action='snooze' then snooze_value:=coalesce((token_row.payload->>'until')::timestamptz,now()+interval '1 day'); to_status:='snoozed'; disposition_value:='snooze';
    elsif token_row.action='not_useful' then to_status:='dismissed'; disposition_value:='not_useful';
    elsif token_row.action<>'prepare_draft' then raise exception 'unsupported heartbeat action %',token_row.action using errcode='22023'; end if;
    new_version:=item_row.version+1;
    update public.heartbeat_items set status=to_status, disposition=disposition_value,
      owner_ref=case when token_row.action='assign' then p_actor_ref else owner_ref end,
      snooze_until=snooze_value, resolved_at=case when to_status in ('resolved','dismissed') then now() else null end,
      version=new_version where item_id=item_row.item_id;
    if token_row.action = 'prepare_draft' then
      draft_token:=gen_random_uuid()::text; draft_hash:=encode(sha256(convert_to(draft_token,'UTF8')),'hex');
      insert into public.heartbeat_draft_grants(grant_hash,delivery_id,item_id,item_version,profile_id,reviewer_ref,expires_at)
        values(draft_hash,token_row.delivery_id,item_row.item_id,new_version,token_row.profile_id,p_actor_ref,now()+interval '7 days');
    end if;
    event_key:='slack:'||p_provider_event_key;
    insert into public.heartbeat_item_events(event_id,item_id,run_id,event_type,from_status,to_status,item_version,actor_kind,actor_ref,payload,idempotency_key)
      values(gen_random_uuid(),item_row.item_id,token_row.run_id,token_row.action,item_row.status,to_status,new_version,'human',p_actor_ref,jsonb_build_object('delivery_id',token_row.delivery_id),event_key)
      on conflict (idempotency_key) do nothing;
    result_value:=jsonb_build_object('item_id',item_row.item_id,'action',token_row.action,'status',to_status,'version',new_version);
    if draft_token is not null then result_value:=result_value||jsonb_build_object('draft_grant',draft_token); end if;
    update public.heartbeat_action_tokens set consumed_at=now(),consumed_by_principal=p_actor_ref,provider_event_key=p_provider_event_key,result=result_value where token_hash=p_token_hash;
    return result_value;
end; $$;
alter function heartbeat_consume_action(text,text,text) owner to centaur_heartbeat_definer;

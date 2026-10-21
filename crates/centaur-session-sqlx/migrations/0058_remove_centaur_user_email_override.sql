create or replace function centaur_current_slack_user_email()
returns text
language sql
stable
security definer
set search_path = pg_catalog, public
as $$
    select (
        select lower(nullif(coalesce(
            users.raw_payload #>> '{profile,email}',
            users.raw_payload ->> 'email'
        ), ''))
        from public.slack_sync_users users
        where users.team_id = public.centaur_current_slack_team_id()
          and users.user_id = public.centaur_current_slack_user_id()
        limit 1
    )
$$;

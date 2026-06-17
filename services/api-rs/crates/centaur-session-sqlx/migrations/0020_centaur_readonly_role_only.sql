do $$
declare
    readonly_relation record;
begin
    for readonly_relation in
        select c.oid::regclass as relation_oid, c.relkind
        from pg_class c
        join pg_namespace n on n.oid = c.relnamespace
        where n.nspname = 'public'
            and c.relkind in ('v', 'm')
            and c.relname like 'centaur_readonly\_%' escape '\'
    loop
        if readonly_relation.relkind = 'm' then
            execute format(
                'drop materialized view if exists %s cascade',
                readonly_relation.relation_oid
            );
        else
            execute format('drop view if exists %s cascade', readonly_relation.relation_oid);
        end if;
    end loop;
end
$$;

do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'centaur_readonly') then
        create role centaur_readonly
            nologin
            nosuperuser
            nocreatedb
            nocreaterole
            noinherit
            noreplication;
    end if;
end
$$;

grant usage on schema public to centaur_readonly;
grant select on all tables in schema public to centaur_readonly;
grant usage, select on all sequences in schema public to centaur_readonly;

revoke insert, update, delete, truncate, references, trigger
    on all tables in schema public
    from centaur_readonly;
revoke update on all sequences in schema public from centaur_readonly;

alter default privileges in schema public
    grant select on tables to centaur_readonly;

alter default privileges in schema public
    grant usage, select on sequences to centaur_readonly;

grant centaur_readonly to current_user;

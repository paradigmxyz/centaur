-- centaur_readonly could read every table in the public schema. Keep anything
-- it owns, revoke all of its privileges, default privileges, and RLS policies
-- in this database, then drop the cluster-wide role when no other database
-- still references it.
do $$
begin
    if exists (select 1 from pg_roles where rolname = 'centaur_readonly') then
        reassign owned by centaur_readonly to current_user;
        drop owned by centaur_readonly;
        begin
            drop role centaur_readonly;
        exception
            when dependent_objects_still_exist or insufficient_privilege then
                raise notice 'centaur_readonly has no privileges here but was not dropped: %',
                    sqlerrm;
        end;
    end if;
end
$$;

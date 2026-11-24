-- nanocode sessions run in centaur-brain, which keeps their durable state in
-- its own schema so its role never needs the public schema. `centaur-brain
-- migrate` applies this with the database owner's credentials, then sets the
-- role's login password from the brain's DSN when it has one.
create schema if not exists nanocode;

do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'centaur_brain') then
        create role centaur_brain
            nologin
            nosuperuser
            nocreatedb
            nocreaterole
            noinherit
            noreplication;
    end if;
end
$$;

-- The brain's stores resolve unqualified names in nanocode. pg_catalog is
-- always searched implicitly, so it needs no entry here.
alter role centaur_brain set search_path = nanocode;

grant all privileges on schema nanocode to centaur_brain;

grant all privileges on all tables in schema nanocode to centaur_brain;
grant all privileges on all sequences in schema nanocode to centaur_brain;
grant all privileges on all functions in schema nanocode to centaur_brain;

alter default privileges in schema nanocode
    grant all privileges on tables to centaur_brain;
alter default privileges in schema nanocode
    grant all privileges on sequences to centaur_brain;
alter default privileges in schema nanocode
    grant all privileges on functions to centaur_brain;

-- No privileges in public, including any granted to it later by default
-- privileges.
revoke all privileges on schema public from centaur_brain;
revoke all privileges on all tables in schema public from centaur_brain;
revoke all privileges on all sequences in schema public from centaur_brain;
revoke all privileges on all functions in schema public from centaur_brain;

alter default privileges in schema public
    revoke all privileges on tables from centaur_brain;
alter default privileges in schema public
    revoke all privileges on sequences from centaur_brain;
alter default privileges in schema public
    revoke all privileges on functions from centaur_brain;

-- Lets the migration owner provision the login without superuser access.
grant centaur_brain to current_user;

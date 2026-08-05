alter table mpp_charge_attempts
    add column if not exists budget_reserved boolean not null default false;

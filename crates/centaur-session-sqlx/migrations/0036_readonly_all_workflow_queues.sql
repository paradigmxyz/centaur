select absurd.create_queue('centaur_workflows');
select absurd.create_queue('centaur_workflows_slack_live');
select absurd.create_queue('centaur_workflows_etl');
select absurd.create_queue('centaur_workflows_etl_backfill');

create or replace view centaur_readonly_workflow_runs as
select
    'centaur_workflows'::text as queue_name,
    r.run_id::text as run_id,
    t.task_id::text as task_id,
    t.task_name,
    t.params ->> 'workflow_name' as workflow_name,
    t.params ->> 'harness_type' as harness_type,
    t.state,
    t.attempts,
    t.max_attempts,
    t.enqueue_at as created_at,
    t.first_started_at,
    r.started_at,
    r.completed_at,
    r.failed_at,
    r.available_at,
    r.claimed_by is not null as claimed,
    t.cancelled_at
from absurd.t_centaur_workflows t
left join absurd.r_centaur_workflows r on r.run_id = t.last_attempt_run
union all
select
    'centaur_workflows_slack_live'::text as queue_name,
    r.run_id::text as run_id,
    t.task_id::text as task_id,
    t.task_name,
    t.params ->> 'workflow_name' as workflow_name,
    t.params ->> 'harness_type' as harness_type,
    t.state,
    t.attempts,
    t.max_attempts,
    t.enqueue_at as created_at,
    t.first_started_at,
    r.started_at,
    r.completed_at,
    r.failed_at,
    r.available_at,
    r.claimed_by is not null as claimed,
    t.cancelled_at
from absurd.t_centaur_workflows_slack_live t
left join absurd.r_centaur_workflows_slack_live r on r.run_id = t.last_attempt_run
union all
select
    'centaur_workflows_etl'::text as queue_name,
    r.run_id::text as run_id,
    t.task_id::text as task_id,
    t.task_name,
    t.params ->> 'workflow_name' as workflow_name,
    t.params ->> 'harness_type' as harness_type,
    t.state,
    t.attempts,
    t.max_attempts,
    t.enqueue_at as created_at,
    t.first_started_at,
    r.started_at,
    r.completed_at,
    r.failed_at,
    r.available_at,
    r.claimed_by is not null as claimed,
    t.cancelled_at
from absurd.t_centaur_workflows_etl t
left join absurd.r_centaur_workflows_etl r on r.run_id = t.last_attempt_run
union all
select
    'centaur_workflows_etl_backfill'::text as queue_name,
    r.run_id::text as run_id,
    t.task_id::text as task_id,
    t.task_name,
    t.params ->> 'workflow_name' as workflow_name,
    t.params ->> 'harness_type' as harness_type,
    t.state,
    t.attempts,
    t.max_attempts,
    t.enqueue_at as created_at,
    t.first_started_at,
    r.started_at,
    r.completed_at,
    r.failed_at,
    r.available_at,
    r.claimed_by is not null as claimed,
    t.cancelled_at
from absurd.t_centaur_workflows_etl_backfill t
left join absurd.r_centaur_workflows_etl_backfill r on r.run_id = t.last_attempt_run;

grant select on table centaur_readonly_workflow_runs to centaur_readonly;

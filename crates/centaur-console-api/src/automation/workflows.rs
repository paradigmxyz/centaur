//! The workflow dashboard (admin only): runs from the api-rs read-only
//! workflow view, schedules and run details from the workflows API (best
//! effort), and force-starting a run. Timestamps are returned as is; the
//! SPA formats durations.

use axum::{extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sqlx::PgPool;

use super::{
    centaur_api::CentaurApiClient,
    workflow_runs::{self as runs, WorkflowRun},
};
use crate::{
    ApiError, ApiResult, AppState,
    auth::ConsoleCaller,
    extract::{Path, Query, data},
    pagination::PageParams,
};

const STATUS_ORDER: [&str; 6] = [
    "running",
    "pending",
    "sleeping",
    "completed",
    "failed",
    "cancelled",
];

fn unavailable() -> ApiError {
    ApiError::upstream(
        StatusCode::SERVICE_UNAVAILABLE,
        "Workflow database is unavailable. Console needs the API workflow read-only views to show runs.",
        Some("workflow_database_unavailable"),
    )
}

fn db_unavailable(error: sqlx::Error) -> ApiError {
    tracing::warn!(%error, "console_workflows_load_failed");
    unavailable()
}

/// The api-rs pool, when the workflow views exist.
async fn workflow_db(state: &AppState) -> ApiResult<PgPool> {
    let pool = state.api_db.clone().ok_or_else(unavailable)?;
    match runs::available(&pool).await {
        Ok(true) => Ok(pool),
        Ok(false) => Err(unavailable()),
        Err(error) => Err(db_unavailable(error)),
    }
}

fn timing(run: &WorkflowRun) -> Map<String, Value> {
    let Value::Object(map) = json!({
        "status": run.display_status(),
        "queue_name": run.queue_name,
        "queue_label": run.queue_label(),
        "attempts": run.attempts.unwrap_or(0),
        "max_attempts": run.max_attempts,
        "started_at": run.started_or_created_at(),
        "created_at": run.created_at,
        "finished_at": run.terminal_at(),
    }) else {
        unreachable!("an object literal")
    };
    map
}

fn present(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.trim().is_empty())
}

pub async fn index(
    State(state): State<AppState>,
    _: ConsoleCaller,
    Query(page): Query<PageParams>,
) -> ApiResult {
    let page = page.resolve();
    let pool = workflow_db(&state).await?;
    let total = runs::workflow_count(&pool).await.map_err(db_unavailable)?;
    let latest = runs::latest_per_workflow(&pool, page.limit(), page.offset())
        .await
        .map_err(db_unavailable)?;
    let keys: Vec<String> = latest
        .iter()
        .filter_map(WorkflowRun::workflow_key)
        .collect();
    let by_queue = runs::latest_per_queue(&pool, &keys)
        .await
        .map_err(db_unavailable)?;
    let rows: Vec<Value> = latest
        .iter()
        .map(|run| {
            let key = run.workflow_key();
            let queues: Vec<Value> = by_queue
                .iter()
                .filter(|queue_run| queue_run.workflow_key() == key)
                .map(|queue_run| {
                    json!({
                        "queue_name": queue_run.queue_name,
                        "queue_label": queue_run.queue_label(),
                        "status": queue_run.display_status(),
                        "last_activity_at": queue_run.recency_at(),
                        "run_count": queue_run.queue_run_count.unwrap_or(0),
                    })
                })
                .collect();
            let mut row = timing(run);
            row.insert("workflow_key".into(), json!(key));
            row.insert("label".into(), json!(run.label()));
            row.insert("task_name".into(), json!(present(&run.task_name)));
            row.insert("queues".into(), json!(queues));
            Value::Object(row)
        })
        .collect();
    Ok(axum::Json(json!({"data": rows, "meta": page.meta(total)})).into_response())
}

/// The harness's display name.
pub fn engine_label(harness_type: Option<&str>) -> Option<String> {
    let value = harness_type.filter(|value| !value.is_empty())?;
    let known = match value {
        "codex" => Some("Codex"),
        "claudecode" => Some("Claude Code"),
        "amp" => Some("Amp"),
        "nanocodex" => Some("Nanocodex"),
        "pi" => Some("Pi"),
        "omp" => Some("OMP"),
        _ => None,
    };
    Some(known.map(str::to_owned).unwrap_or_else(|| {
        value
            .replace(['_', '-'], " ")
            .split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |first| {
                    first
                        .to_uppercase()
                        .chain(chars.flat_map(char::to_lowercase))
                        .collect()
                })
            })
            .collect::<Vec<_>>()
            .join(" ")
    }))
}

/// "cron 0 9 * * *", "every 5m", or "every 30s".
pub fn schedule_label(schedule: &Value) -> Option<String> {
    let kind = schedule.get("kind")?;
    match kind.get("type")?.as_str()? {
        "cron" => Some(format!(
            "cron {}",
            kind.get("cron").and_then(Value::as_str).unwrap_or_default()
        )),
        "interval" => {
            let seconds = kind
                .get("interval_seconds")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            Some(if seconds >= 60 && seconds % 60 == 0 {
                format!("every {}m", seconds / 60)
            } else {
                format!("every {seconds}s")
            })
        }
        _ => None,
    }
}

/// The source file's link: overlay paths map to the overlay repository.
pub fn source_url(source_path: Option<&str>) -> Option<String> {
    let path = source_path.filter(|path| !path.trim().is_empty())?;
    Some(match path.strip_prefix("centaur-tempo/") {
        Some(rest) => format!("https://github.com/tempoxyz/centaur-tempo/blob/main/{rest}"),
        None => format!("https://github.com/paradigmxyz/centaur/blob/main/{path}"),
    })
}

fn schedule_payload(schedule: Option<&Value>) -> Value {
    let Some(schedule) = schedule else {
        return Value::Null;
    };
    let label = schedule_label(schedule);
    let text = |key: &str| {
        schedule
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    };
    let timezone = text("timezone");
    let is_cron = schedule.pointer("/kind/type").and_then(Value::as_str) == Some("cron");
    let disabled = schedule.get("enabled") == Some(&Value::Bool(false));
    let mut parts: Vec<String> = label.iter().cloned().collect();
    if is_cron && let Some(timezone) = &timezone {
        parts.push(timezone.clone());
    }
    if disabled {
        parts.push("disabled".into());
    }
    let summary = parts.join(" · ");
    let source_path = text("source_path");
    json!({
        "label": label,
        "summary": (!summary.is_empty()).then_some(summary),
        "timezone": timezone,
        "enabled": !disabled,
        "source_url": source_url(source_path.as_deref()),
        "source_path": source_path,
    })
}

async fn schedule_for(api: &CentaurApiClient<'_>, workflow: &str) -> Option<Value> {
    match api.list_workflow_schedules().await {
        Ok(response) => response
            .get("schedules")
            .and_then(Value::as_array)?
            .iter()
            .find(|schedule| {
                schedule.get("workflow_name").and_then(Value::as_str) == Some(workflow)
            })
            .cloned(),
        Err(error) => {
            tracing::warn!(%error, "console_workflow_schedules_failed");
            None
        }
    }
}

/// A run's details from the workflows API, best effort.
async fn run_detail(
    api: &CentaurApiClient<'_>,
    run_id: Option<&str>,
) -> Option<Map<String, Value>> {
    let run_id = run_id.filter(|id| !id.trim().is_empty())?;
    match api.get_workflow_run(run_id).await {
        Ok(response) => response
            .get("run")
            .and_then(Value::as_object)
            .filter(|run| !run.is_empty())
            .cloned(),
        Err(error) => {
            tracing::warn!(run = run_id, %error, "console_workflow_run_detail_failed");
            None
        }
    }
}

fn debug_row(label: &str, value: &Value, run_id: Option<&Value>) -> Value {
    json!({"label": label, "value": value, "run_id": run_id.cloned().unwrap_or(Value::Null)})
}

fn blank(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

pub async fn show(
    State(state): State<AppState>,
    _: ConsoleCaller,
    Path(workflow): Path<String>,
) -> ApiResult {
    let pool = workflow_db(&state).await?;
    let latest = runs::for_workflow(&pool, &workflow, 1, 0, None, None)
        .await
        .map_err(db_unavailable)?
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::NotFound(format!("No workflow runs found for {workflow}.")))?;
    let counts = runs::status_counts(&pool, &workflow)
        .await
        .map_err(db_unavailable)?;
    let queues = runs::queue_names(&pool, &workflow)
        .await
        .map_err(db_unavailable)?;
    let api = CentaurApiClient::new(&state);
    let schedule = schedule_for(&api, &workflow).await;

    let mut statuses: Vec<&str> = STATUS_ORDER
        .iter()
        .copied()
        .filter(|status| counts.iter().any(|(s, _)| s == status))
        .collect();
    let mut others: Vec<&str> = counts
        .iter()
        .map(|(status, _)| status.as_str())
        .filter(|status| !STATUS_ORDER.contains(status))
        .collect();
    others.sort_unstable();
    statuses.extend(others);
    let count_of = |wanted: &str| {
        counts
            .iter()
            .find(|(status, _)| status == wanted)
            .map_or(0, |(_, count)| *count)
    };

    // Debug rows: the latest run's input, result, and failure; and the last
    // failure when the latest run did not fail.
    let mut debug = Vec::new();
    let detail = run_detail(&api, latest.run_id.as_deref()).await;
    if let Some(detail) = &detail {
        if let Some(input) = detail.get("input").filter(|input| !blank(input)) {
            debug.push(debug_row("Input", input, None));
        }
        if let Some(result) = detail.get("result").filter(|value| !value.is_null()) {
            debug.push(debug_row("Result", result, None));
        }
        if let Some(failure) = detail.get("failure").filter(|value| !value.is_null()) {
            debug.push(debug_row("Failure", failure, detail.get("run_id")));
        }
    }
    if (detail.is_some() || schedule.is_some())
        && latest.display_status() != "failed"
        && count_of("failed") > 0
    {
        let failed = runs::for_workflow(&pool, &workflow, 1, 0, Some("failed"), None)
            .await
            .map_err(db_unavailable)?
            .into_iter()
            .next();
        if let Some(failure) =
            run_detail(&api, failed.as_ref().and_then(|run| run.run_id.as_deref())).await
            && let Some(value) = failure.get("failure").filter(|value| !blank(value))
        {
            debug.push(debug_row("Last failure", value, failure.get("run_id")));
        }
    }

    let mut out = timing(&latest);
    out.insert("workflow_key".into(), json!(workflow));
    out.insert("label".into(), json!(latest.label()));
    out.insert("task_name".into(), json!(present(&latest.task_name)));
    out.insert("harness_type".into(), json!(present(&latest.harness_type)));
    out.insert(
        "engine_label".into(),
        json!(engine_label(latest.harness_type.as_deref())),
    );
    out.insert(
        "run_count".into(),
        json!(counts.iter().map(|(_, count)| count).sum::<i64>()),
    );
    out.insert(
        "status_counts".into(),
        json!(
            statuses
                .iter()
                .map(|status| json!({"status": status, "count": count_of(status)}))
                .collect::<Vec<_>>()
        ),
    );
    out.insert(
        "queues".into(),
        json!(
            queues
                .iter()
                .map(|name| json!({
                    "queue_name": name,
                    "queue_label": runs::queue_label(name.as_deref().unwrap_or_default()),
                }))
                .collect::<Vec<_>>()
        ),
    );
    out.insert("schedule".into(), schedule_payload(schedule.as_ref()));
    out.insert("debug".into(), json!(debug));
    Ok(data(Value::Object(out)).into_response())
}

#[derive(Deserialize)]
pub struct RunFilter {
    status: Option<String>,
    queue: Option<String>,
}

pub async fn list_runs(
    State(state): State<AppState>,
    _: ConsoleCaller,
    Path(workflow): Path<String>,
    Query(filter): Query<RunFilter>,
    Query(page): Query<PageParams>,
) -> ApiResult {
    let page = page.resolve();
    let pool = workflow_db(&state).await?;
    let nonblank = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let (status, queue) = (nonblank(&filter.status), nonblank(&filter.queue));
    let total = runs::run_count(&pool, &workflow, status.as_deref(), queue.as_deref())
        .await
        .map_err(db_unavailable)?;
    let rows = runs::for_workflow(
        &pool,
        &workflow,
        page.limit(),
        page.offset(),
        status.as_deref(),
        queue.as_deref(),
    )
    .await
    .map_err(db_unavailable)?;
    let rows: Vec<Value> = rows
        .iter()
        .map(|run| {
            let mut row = timing(run);
            row.insert("run_id".into(), json!(run.run_id));
            row.insert("task_id".into(), json!(run.task_id));
            Value::Object(row)
        })
        .collect();
    Ok(axum::Json(json!({"data": rows, "meta": page.meta(total)})).into_response())
}

/// Starts a run now. Scheduled workflows start with their schedule's input.
pub async fn create_run(
    State(state): State<AppState>,
    _: ConsoleCaller,
    Path(workflow): Path<String>,
) -> ApiResult {
    let api = CentaurApiClient::new(&state);
    let input = schedule_for(&api, &workflow)
        .await
        .and_then(|schedule| schedule.get("input").cloned());
    match api.create_workflow_run(&workflow, input.as_ref()).await {
        Ok(result) => {
            let created = result.get("created") != Some(&Value::Bool(false));
            let status = if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            Ok((
                status,
                data(json!({"run_id": result.get("run_id"), "created": created})),
            )
                .into_response())
        }
        Err(error) => {
            tracing::warn!(%workflow, %error, "console_workflow_force_start_failed");
            Err(ApiError::upstream(
                StatusCode::BAD_GATEWAY,
                error.message,
                Some("workflow_api_error"),
            ))
        }
    }
}

mod config;
mod credentials;
mod database;
mod drive;
mod embeddings;
mod errors;
mod extraction;
mod granola;
mod granola_tasks;
mod scheduler;
mod tasks;
mod telemetry;

use std::sync::Arc;

use absurd::{Client, ClientOptions, CreateQueueOptions, WorkerOptions};
use anyhow::{Context, Result};
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::sync::watch;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use crate::{
    config::{Config, QUEUE_NAME},
    credentials::ConsoleCredentials,
    drive::DriveClient,
    embeddings::EmbeddingsClient,
    granola::GranolaClient,
    tasks::TaskState,
};

#[derive(Clone)]
struct HttpState {
    pool: sqlx::PgPool,
    credentials: Arc<ConsoleCredentials>,
    metrics: PrometheusHandle,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Arc::new(Config::from_args());
    let pool = database::connect_and_migrate(&config.database_url).await?;
    let credentials = Arc::new(ConsoleCredentials::connect(&config).await?);
    let absurd = Client::from_pool_with_options(
        pool.clone(),
        ClientOptions {
            pool: Some(pool.clone()),
            queue_name: QUEUE_NAME.to_owned(),
            ..ClientOptions::default()
        },
    )?;
    absurd
        .create_queue(None, CreateQueueOptions::default())
        .await
        .context("create company context Absurd queue")?;

    let drive = DriveClient::new(&config, credentials.clone())?;
    let granola = GranolaClient::new(&config)?;
    let embeddings = EmbeddingsClient::new(&config)?;
    tasks::register(TaskState {
        config: config.clone(),
        pool: pool.clone(),
        absurd: absurd.clone(),
        credentials: credentials.clone(),
        drive,
        granola,
        embeddings,
    })?;

    let metrics = telemetry::init_metrics()?;
    let http_state = HttpState {
        pool: pool.clone(),
        credentials: credentials.clone(),
        metrics,
    };
    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/readyz", get(ready))
        .route("/metrics", get(render_metrics))
        .with_state(http_state);
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;

    let worker = absurd.start_worker(WorkerOptions {
        worker_id: Some(format!("company-context-{}", Uuid::new_v4())),
        concurrency: config.worker_concurrency,
        on_error: Some(Arc::new(
            |error| error!(event = "company_context_worker_error", error = %error),
        )),
        ..WorkerOptions::default()
    });
    let scheduler = tokio::spawn(scheduler::run(
        config.clone(),
        absurd.clone(),
        credentials.clone(),
    ));
    let granola_scheduler = tokio::spawn(scheduler::run_granola(
        config.clone(),
        absurd,
        credentials.clone(),
    ));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server_shutdown = shutdown_rx.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let mut shutdown = server_shutdown;
                let _ = shutdown.wait_for(|value| *value).await;
            })
            .await
    });

    info!(event = "company_context_started", bind = %config.bind_addr, queue = QUEUE_NAME);
    tokio::signal::ctrl_c().await?;
    info!(event = "company_context_shutdown_started");
    let _ = shutdown_tx.send(true);
    scheduler.abort();
    granola_scheduler.abort();
    worker.close().await?;
    server.await.context("join HTTP server")??;
    credentials.close().await;
    pool.close().await;
    Ok(())
}

async fn ready(State(state): State<HttpState>) -> StatusCode {
    if database::ready(&state.pool).await && state.credentials.ready().await {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn render_metrics(State(state): State<HttpState>) -> Response {
    (
        [("Content-Type", "text/plain; version=0.0.4; charset=utf-8")],
        state.metrics.render(),
    )
        .into_response()
}

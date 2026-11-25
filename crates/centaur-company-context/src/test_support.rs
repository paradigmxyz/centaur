use std::time::{SystemTime, UNIX_EPOCH};

use axum::{Json, Router, http::StatusCode, routing::post};
use clap::Parser;
use serde_json::{Value, json};
use sqlx::{Connection, Executor, PgConnection, PgPool};
use tokio::sync::Mutex;

use crate::{
    config::Config,
    database::{self, TextSearchBackend},
    embeddings::EmbeddingsClient,
};

/// Migrations create cluster-global roles with a check-then-create, so
/// concurrently migrating test databases would race on `pg_authid`.
pub(crate) static MIGRATIONS: Mutex<()> = Mutex::const_new(());

/// A freshly migrated database that is dropped at the end of a test.
pub struct TestDatabase {
    pub pool: PgPool,
    admin: PgConnection,
    name: String,
}

impl TestDatabase {
    pub async fn create(database_url: &str, label: &str) -> Self {
        let mut admin = PgConnection::connect(database_url).await.unwrap();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("company_context_{label}_{}_{nanos}", std::process::id());
        admin
            .execute(format!(r#"create database "{name}""#).as_str())
            .await
            .unwrap();
        let mut url = url::Url::parse(database_url).unwrap();
        url.set_path(&name);
        let backend = test_text_search(&mut admin).await;
        let pool = {
            let _guard = MIGRATIONS.lock().await;
            database::connect_and_migrate(url.as_str(), backend)
                .await
                .unwrap()
        };
        Self { pool, admin, name }
    }

    pub async fn drop(mut self) {
        self.pool.close().await;
        // pg_search starts background mergers connected to the database after
        // index writes, and a plain DROP DATABASE fails if one outlives its
        // five-second wait.
        self.admin
            .execute(format!(r#"drop database if exists "{}" with (force)"#, self.name).as_str())
            .await
            .unwrap();
    }
}

/// The text-search backend tests migrate with: `paradedb` where pg_search is
/// available, `postgres` elsewhere.
async fn test_text_search(admin: &mut PgConnection) -> TextSearchBackend {
    let pg_search: bool = sqlx::query_scalar(
        "select exists (select 1 from pg_available_extensions where name = 'pg_search')",
    )
    .fetch_one(admin)
    .await
    .unwrap();
    if pg_search {
        TextSearchBackend::Paradedb
    } else {
        TextSearchBackend::Postgres
    }
}

/// The text-search backend a test database was migrated with.
pub async fn text_search(pool: &PgPool) -> TextSearchBackend {
    let postgres: bool = sqlx::query_scalar(
        "select exists (
            select 1 from pg_attribute
            where attrelid = 'company_context_data.slack_documents'::regclass
              and attname = 'search_vector'
        )",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    if postgres {
        TextSearchBackend::Postgres
    } else {
        TextSearchBackend::Paradedb
    }
}

/// Serves embeddings, rejecting batches with an input containing "REJECT".
pub async fn fake_embeddings() -> (EmbeddingsClient, tokio::task::JoinHandle<()>) {
    async fn embed(Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        let inputs = body["input"].as_array().unwrap();
        if inputs
            .iter()
            .any(|input| input.as_str().unwrap().contains("REJECT"))
        {
            return (StatusCode::BAD_REQUEST, Json(json!({})));
        }
        let data: Vec<Value> = (0..inputs.len())
            .map(|index| json!({ "index": index, "embedding": vec![0.5; 1536] }))
            .collect();
        (StatusCode::OK, Json(json!({ "data": data })))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/embeddings", post(embed)))
            .await
            .unwrap();
    });
    let config = Config::try_parse_from([
        "centaur-company-context",
        "--database-url",
        "postgresql://context",
        "--console-database-url",
        "postgresql://console",
        "--active-record-primary-key",
        "primary",
        "--active-record-key-derivation-salt",
        "salt",
        "--openai-api-key",
        "test-key",
        "--slack-bot-token",
        "xoxb-test",
        "--jwt-signing-secret",
        "secret",
        "--openai-base-url",
        &base_url,
    ])
    .unwrap();
    (EmbeddingsClient::new(&config).unwrap(), server)
}

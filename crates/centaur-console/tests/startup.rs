//! Database preparation at startup (against the test Postgres server): create
//! the console database when it is missing, tolerating another process that
//! creates it first.

use std::str::FromStr;

use centaur_console::startup;
use centaur_console_api::env::Env;
use sqlx::{ConnectOptions, Connection, Executor, PgConnection, postgres::PgConnectOptions};

fn options(url: &str, database: &str) -> PgConnectOptions {
    PgConnectOptions::from_str(url)
        .expect("the test database URL")
        .database(database)
}

fn unique_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("console_startup_{}_{nanos}", std::process::id())
}

async fn connect(database: &str) -> Option<PgConnection> {
    let url = centaur_console_db::testing::database_url()?;
    Some(
        PgConnection::connect_with(&options(&url, database))
            .await
            .expect("connect to the test server"),
    )
}

async fn drop_database(name: &str) {
    let Some(mut admin) = connect("postgres").await else {
        return;
    };
    admin
        .execute(format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)").as_str())
        .await
        .expect("drop the test database");
}

/// `serve` and `worker` both find a fresh database missing and both create it;
/// the loser's `42P04` means the database exists, which is success.
#[tokio::test]
async fn creating_a_database_another_process_created_is_success() {
    let Some(mut admin) = connect("postgres").await else {
        return;
    };
    let name = unique_name();
    startup::create_database(&mut admin, &name)
        .await
        .expect("create the database");
    startup::create_database(&mut admin, &name)
        .await
        .expect("an existing database is success");
    drop_database(&name).await;
}

/// Two concurrent start-ups on a missing database both end up with a working
/// pool, whichever one wins the create race.
#[tokio::test]
async fn concurrent_startups_prepare_the_same_new_database() {
    let Some(url) = centaur_console_db::testing::database_url() else {
        return;
    };
    let name = unique_name();
    let database_url = options(&url, &name).to_url_lossy().to_string();
    let env = Env::from_pairs([("CENTAUR_CONSOLE_DATABASE_URL", database_url.as_str())]);
    let (first, second) = tokio::join!(
        startup::prepare_database(&env),
        startup::prepare_database(&env)
    );
    let (first, second) = (
        first.expect("first start-up"),
        second.expect("second start-up"),
    );
    for pool in [&first, &second] {
        let one: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(pool)
            .await
            .expect("the prepared pool answers");
        assert_eq!(one, 1);
    }
    first.close().await;
    second.close().await;
    drop_database(&name).await;
}

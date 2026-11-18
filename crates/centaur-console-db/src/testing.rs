//! Disposable databases for DB-backed tests (feature `testing`).
//!
//! Set `CONSOLE_DB_TEST_DATABASE_URL` to any database on a disposable
//! Postgres server whose user may `CREATE DATABASE` (the same variable the
//! read-side tests use). [`TestDb::migrated`] clones a per-process template
//! that `migrate` installed once, so each test gets its own schema in a few
//! milliseconds; [`TestDb::empty`] gives a database with no schema at all.
//! Without the variable the helpers return `None` and the test should
//! return early (it panics instead when `CI` is set, so CI never silently
//! skips).
//!
//! Databases are named `console_test_<run>_<n>`; databases left by runs more
//! than an hour old are dropped when a new run creates its template.

use std::{
    env,
    sync::atomic::{AtomicU32, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use sqlx::{
    ConnectOptions, Executor, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tokio::sync::OnceCell;

use crate::migrate::{MIGRATOR, migrate};

pub const DATABASE_URL_VAR: &str = "CONSOLE_DB_TEST_DATABASE_URL";

static RUN: OnceCell<Run> = OnceCell::const_new();
static NEXT: AtomicU32 = AtomicU32::new(0);
/// Caps the test databases alive at once in one test process, so a
/// many-core machine running every test in parallel stays under the
/// server's `max_connections` (each database's pool may open five).
/// Override with `CONSOLE_DB_TEST_MAX_DATABASES`.
static SLOTS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| {
        let slots = std::env::var("CONSOLE_DB_TEST_MAX_DATABASES")
            .ok()
            .and_then(|raw| raw.parse().ok())
            .filter(|slots: &usize| *slots > 0)
            .unwrap_or(12);
        std::sync::Arc::new(tokio::sync::Semaphore::new(slots))
    });

struct Run {
    id: u64,
    options: PgConnectOptions,
    template: String,
}

/// A database owned by one test.
pub struct TestDb {
    pub pool: PgPool,
    pub name: String,
    pub url: String,
    _slot: tokio::sync::OwnedSemaphorePermit,
}

impl TestDb {
    /// A database holding the full console schema (every SQLx migration).
    pub async fn migrated() -> Option<Self> {
        let run = run().await?;
        Some(create(run, Some(&run.template)).await)
    }

    /// A database with no schema, for migration tests.
    pub async fn empty() -> Option<Self> {
        let run = run().await?;
        Some(create(run, None).await)
    }

    /// Drops the database. Optional: stale databases are reaped by later runs.
    pub async fn drop_database(self) {
        let run = RUN.get().expect("run initialized");
        self.pool.close().await;
        let mut admin = run
            .options
            .connect()
            .await
            .expect("connect to the test server");
        admin
            .execute(format!("DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)", self.name).as_str())
            .await
            .expect("drop test database");
    }
}

/// Whether DB-backed tests can run; prints the skip notice when not.
pub fn database_url() -> Option<String> {
    match env::var(DATABASE_URL_VAR) {
        Ok(url) if !url.is_empty() => Some(url),
        _ => {
            assert!(
                env::var_os("CI").is_none(),
                "{DATABASE_URL_VAR} must be set in CI"
            );
            eprintln!("skipping: set {DATABASE_URL_VAR} to a disposable Postgres database");
            None
        }
    }
}

async fn run() -> Option<&'static Run> {
    let url = database_url()?;
    Some(
        RUN.get_or_init(|| async move {
            let options: PgConnectOptions = url.parse().expect("valid test database URL");
            let id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_secs();
            let template = template_name();
            let mut admin = options.connect().await.expect("connect to the test server");
            // One template per migration set, created once across processes.
            admin
                .execute("SELECT pg_advisory_lock(7419020001)")
                .await
                .expect("template lock");
            reap_stale(&mut admin, id).await;
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
                    .bind(&template)
                    .fetch_one(&mut admin)
                    .await
                    .expect("query templates");
            if !exists {
                let building = format!("{template}_building");
                admin
                    .execute(
                        format!("DROP DATABASE IF EXISTS \"{building}\" WITH (FORCE)").as_str(),
                    )
                    .await
                    .expect("drop partial template");
                admin
                    .execute(format!("CREATE DATABASE \"{building}\" TEMPLATE template0").as_str())
                    .await
                    .expect("create template");
                let pool = PgPoolOptions::new()
                    .max_connections(1)
                    .connect_with(options.clone().database(&building))
                    .await
                    .expect("connect to template");
                migrate(&pool).await.expect("migrate template");
                pool.close().await;
                admin
                    .execute(
                        format!("ALTER DATABASE \"{building}\" RENAME TO \"{template}\"").as_str(),
                    )
                    .await
                    .expect("publish template");
            }
            admin
                .execute("SELECT pg_advisory_unlock(7419020001)")
                .await
                .expect("template unlock");
            Run {
                id,
                options,
                template,
            }
        })
        .await,
    )
}

async fn create(run: &Run, template: Option<&str>) -> TestDb {
    let slot = SLOTS
        .clone()
        .acquire_owned()
        .await
        .expect("test database semaphore is never closed");
    let name = format!(
        "console_test_{}_{}_{}",
        run.id,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let mut admin = run
        .options
        .connect()
        .await
        .expect("connect to the test server");
    let template = template.unwrap_or("template0");
    // CREATE DATABASE ... TEMPLATE fails while another session is connected
    // to the template; nothing connects to it after it is built, but retry
    // briefly in case a concurrent creation is mid-copy.
    let mut attempts = 0;
    loop {
        match admin
            .execute(format!("CREATE DATABASE \"{name}\" TEMPLATE \"{template}\"").as_str())
            .await
        {
            Ok(_) => break,
            Err(error) if attempts < 50 => {
                attempts += 1;
                let _ = error;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err(error) => panic!("create test database: {error}"),
        }
    }
    let options = run.options.clone().database(&name);
    let url = options.to_url_lossy().to_string();
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .expect("connect to the test database");
    TestDb {
        pool,
        name,
        url,
        _slot: slot,
    }
}

/// The template's name changes with the embedded migrations.
fn template_name() -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for migration in MIGRATOR.iter() {
        digest.update(migration.version.to_be_bytes());
        digest.update(&*migration.checksum);
    }
    format!("console_template_{}", &hex::encode(digest.finalize())[..16])
}

async fn reap_stale(admin: &mut sqlx::PgConnection, now: u64) {
    let names: Vec<String> =
        sqlx::query_scalar("SELECT datname FROM pg_database WHERE datname LIKE 'console_test_%'")
            .fetch_all(&mut *admin)
            .await
            .unwrap_or_default();
    for name in names {
        let run: Option<u64> = name
            .strip_prefix("console_test_")
            .and_then(|rest| rest.split('_').next())
            .and_then(|run| run.parse().ok());
        if run.is_some_and(|run| run + 3600 < now) {
            let _ = admin
                .execute(format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)").as_str())
                .await;
        }
    }
}

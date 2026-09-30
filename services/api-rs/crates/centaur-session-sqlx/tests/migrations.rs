//! Migration ordering across text-search backends.
//!
//! A fresh database applies every migration in version order. An existing
//! database applied the same list in batches, one per release. Both must end
//! with the same schema for every backend.

use std::{
    env,
    error::Error,
    future::Future,
    pin::Pin,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use centaur_session_sqlx::{SessionStoreError, TextSearchBackend, migrate, migration_list};
use sqlx::{
    Connection, Executor, PgConnection,
    error::BoxDynError,
    migrate::{Migration, MigrationSource, Migrator},
    postgres::PgConnectOptions,
};

/// Keyword-searchable tables; each backend must index every one of them.
const SEARCHABLE_TABLES: [&str; 5] = [
    "company_context_documents",
    "google_docs_context_documents",
    "granola_context_documents",
    "slack_private_context_documents",
    "slack_private_conversation_context_documents",
];

/// Last core version released while core migrations created the BM25 indexes.
const LAST_LEGACY_CORE_VERSION: i64 = 55;

/// BM25 indexes as core migrations created them before the backends split.
const LEGACY_BM25_INDEXES_SQL: &str = include_str!("fixtures/legacy_bm25_indexes.sql");

static MIGRATION_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn fresh_databases_migrate_and_index_every_searchable_table() -> Result<(), Box<dyn Error>> {
    let Some(server) = TestServer::connect().await? else {
        return Ok(());
    };
    for backend in server.backends.clone() {
        let mut database = server.create_database().await?;
        let result = async {
            migrate(&mut database.conn, backend).await?;
            // A second run must find nothing to apply.
            migrate(&mut database.conn, backend).await?;
            for table in SEARCHABLE_TABLES {
                assert!(
                    has_keyword_index(&mut database.conn, backend, table).await?,
                    "{backend} migrations do not index {table}"
                );
            }
            Ok::<_, Box<dyn Error>>(())
        }
        .await;
        server.drop_database(database, result).await?;
    }
    Ok(())
}

#[tokio::test]
async fn every_upgrade_path_matches_a_fresh_database() -> Result<(), Box<dyn Error>> {
    let Some(server) = TestServer::connect().await? else {
        return Ok(());
    };
    for backend in server.backends.clone() {
        let reference = server.fresh_schema(backend).await?;
        let migrations = migration_list(backend);
        // Roles are cluster-wide and later migrations drop some, so each
        // upgrade path gets the server to itself.
        for applied in 1..migrations.len() {
            let stopped_after = migrations[applied - 1].version;
            let mut database = server.create_database().await?;
            let result = async {
                run_list(&mut database.conn, migrations[..applied].to_vec()).await?;
                migrate(&mut database.conn, backend).await?;
                let schema = schema_snapshot(&mut database.conn).await?;
                assert_same_schema(
                    &reference,
                    &schema,
                    &format!("{backend} database upgraded after version {stopped_after}"),
                );
                Ok::<_, Box<dyn Error>>(())
            }
            .await;
            server.drop_database(database, result).await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn legacy_bm25_databases_adopt_either_backend() -> Result<(), Box<dyn Error>> {
    let Some(server) = TestServer::connect().await? else {
        return Ok(());
    };
    if !server.backends.contains(&TextSearchBackend::Paradedb) {
        eprintln!("skipping legacy BM25 upgrade test: pg_search is unavailable");
        return Ok(());
    }
    for backend in TextSearchBackend::ALL {
        let mut reference = server.fresh_schema(backend).await?;
        let mut database = server.create_database().await?;
        let result = async {
            let legacy = migration_list(backend)
                .into_iter()
                .take_while(|migration| migration.version <= LAST_LEGACY_CORE_VERSION)
                .collect();
            run_list(&mut database.conn, legacy).await?;
            sqlx::raw_sql(LEGACY_BM25_INDEXES_SQL)
                .execute(&mut database.conn)
                .await?;
            migrate(&mut database.conn, backend).await?;

            let mut schema = schema_snapshot(&mut database.conn).await?;
            // The postgres backend leaves an installed pg_search in place, and
            // ParadeDB images may preinstall it in new databases.
            if backend == TextSearchBackend::Postgres {
                for snapshot in [&mut reference, &mut schema] {
                    snapshot.retain(|line| line != "extension pg_search");
                }
            }
            assert_same_schema(
                &reference,
                &schema,
                &format!("legacy BM25 database adopting {backend}"),
            );
            Ok::<_, Box<dyn Error>>(())
        }
        .await;
        server.drop_database(database, result).await?;
    }
    Ok(())
}

#[tokio::test]
async fn databases_keep_their_text_search_backend() -> Result<(), Box<dyn Error>> {
    let Some(server) = TestServer::connect().await? else {
        return Ok(());
    };
    let mut database = server.create_database().await?;
    let result = async {
        migrate(&mut database.conn, TextSearchBackend::Postgres).await?;
        let before = schema_snapshot(&mut database.conn).await?;

        let error = migrate(&mut database.conn, TextSearchBackend::Paradedb)
            .await
            .expect_err("switching backends must fail");
        assert!(
            matches!(
                error,
                SessionStoreError::TextSearchBackendMismatch {
                    configured: TextSearchBackend::Paradedb,
                    applied: TextSearchBackend::Postgres,
                }
            ),
            "unexpected error: {error}"
        );
        assert_eq!(schema_snapshot(&mut database.conn).await?, before);
        Ok::<_, Box<dyn Error>>(())
    }
    .await;
    server.drop_database(database, result).await
}

async fn has_keyword_index(
    conn: &mut PgConnection,
    backend: TextSearchBackend,
    table: &str,
) -> Result<bool, sqlx::Error> {
    let pattern = match backend {
        TextSearchBackend::Paradedb => "% USING bm25 %",
        TextSearchBackend::Postgres => "% USING gin (search_vector)",
    };
    sqlx::query_scalar(
        "select exists (
            select 1 from pg_indexes
            where schemaname = 'public' and tablename = $1 and indexdef like $2
        )",
    )
    .bind(table)
    .bind(pattern)
    .fetch_one(conn)
    .await
}

/// Every user-visible schema object, one normalized line each, sorted.
async fn schema_snapshot(conn: &mut PgConnection) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        r#"
        with excluded_namespaces as (
            select oid from pg_namespace
            where nspname in ('pg_catalog', 'information_schema')
               or nspname like 'pg_toast%'
               or nspname like 'pg_temp%'
               or oid in (
                   select objid from pg_depend
                   where classid = 'pg_namespace'::regclass and deptype = 'e'
               )
        ),
        relations as (
            select c.* from pg_class c
            where c.relnamespace not in (select oid from excluded_namespaces)
              and c.oid not in (
                  select objid from pg_depend
                  where classid = 'pg_class'::regclass and deptype = 'e'
              )
        )
        select line from (
            select format(
                'schema %s acl=%s', n.nspname, coalesce(n.nspacl::text, '')
            ) as line
            from pg_namespace n
            where n.oid not in (select oid from excluded_namespaces)
            union all
            select format(
                'relation %s kind=%s rls=%s acl=%s',
                c.oid::regclass, c.relkind, c.relrowsecurity, coalesce(c.relacl::text, '')
            )
            from relations c
            union all
            select format(
                'column %s.%s #%s %s not_null=%s default=%s generated=%s acl=%s',
                a.attrelid::regclass, a.attname, a.attnum,
                format_type(a.atttypid, a.atttypmod), a.attnotnull,
                coalesce(pg_get_expr(d.adbin, d.adrelid), ''), a.attgenerated,
                coalesce(a.attacl::text, '')
            )
            from pg_attribute a
            join relations c on c.oid = a.attrelid
            left join pg_attrdef d on d.adrelid = a.attrelid and d.adnum = a.attnum
            where a.attnum > 0 and not a.attisdropped
            union all
            select format('constraint %s %s %s', r.conrelid::regclass, r.conname, pg_get_constraintdef(r.oid))
            from pg_constraint r
            where r.connamespace not in (select oid from excluded_namespaces)
            union all
            select format('index %s', pg_get_indexdef(i.indexrelid))
            from pg_index i
            join relations c on c.oid = i.indexrelid
            union all
            select format(
                'policy %s.%s %s %s %s using=%s check=%s',
                p.schemaname, p.tablename, p.policyname, p.roles, p.cmd,
                coalesce(p.qual, ''), coalesce(p.with_check, '')
            )
            from pg_policies p
            union all
            select format('trigger %s', pg_get_triggerdef(t.oid))
            from pg_trigger t
            join relations c on c.oid = t.tgrelid
            where not t.tgisinternal
            union all
            select format('view %s %s', c.oid::regclass, pg_get_viewdef(c.oid))
            from relations c
            where c.relkind in ('v', 'm')
            union all
            select format(
                'function %s acl=%s %s',
                p.oid::regprocedure, coalesce(p.proacl::text, ''),
                case when p.prokind in ('f', 'p') then pg_get_functiondef(p.oid) else '' end
            )
            from pg_proc p
            where p.pronamespace not in (select oid from excluded_namespaces)
              and p.oid not in (
                  select objid from pg_depend
                  where classid = 'pg_proc'::regclass and deptype = 'e'
              )
            union all
            select format('extension %s', e.extname)
            from pg_extension e
            union all
            select format('migration %s success=%s %s', m.version, m.success, encode(m.checksum, 'hex'))
            from _sqlx_migrations m
        ) objects
        order by line
        "#,
    )
    .fetch_all(conn)
    .await
}

fn assert_same_schema(expected: &[String], actual: &[String], context: &str) {
    let missing: Vec<_> = expected
        .iter()
        .filter(|line| !actual.contains(line))
        .collect();
    let unexpected: Vec<_> = actual
        .iter()
        .filter(|line| !expected.contains(line))
        .collect();
    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "{context} differs from a fresh database\nmissing: {missing:#?}\nunexpected: {unexpected:#?}"
    );
}

async fn run_list(
    conn: &mut PgConnection,
    migrations: Vec<Migration>,
) -> Result<(), sqlx::migrate::MigrateError> {
    Migrator::new(MigrationList(migrations))
        .await?
        .run(conn)
        .await?;
    Ok(())
}

#[derive(Debug)]
struct MigrationList(Vec<Migration>);

impl<'s> MigrationSource<'s> for MigrationList {
    fn resolve(
        self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Migration>, BoxDynError>> + Send + 's>> {
        Box::pin(async move { Ok(self.0) })
    }
}

struct TestServer {
    _guard: tokio::sync::MutexGuard<'static, ()>,
    database_url: String,
    backends: Vec<TextSearchBackend>,
}

struct TestDatabase {
    name: String,
    conn: PgConnection,
}

impl TestServer {
    async fn connect() -> Result<Option<Self>, Box<dyn Error>> {
        let Ok(database_url) = env::var("SESSION_SQLX_TEST_DATABASE_URL")
            .or_else(|_| env::var("SESSION_RUNTIME_TEST_DATABASE_URL"))
        else {
            eprintln!(
                "skipping migration tests: set SESSION_SQLX_TEST_DATABASE_URL to a Postgres URL"
            );
            return Ok(None);
        };
        let guard = MIGRATION_TEST_LOCK.lock().await;
        let mut conn = PgConnection::connect(&database_url).await?;
        let pg_search: bool = sqlx::query_scalar(
            "select exists (select 1 from pg_available_extensions where name = 'pg_search')",
        )
        .fetch_one(&mut conn)
        .await?;
        conn.close().await?;
        let backends = TextSearchBackend::ALL
            .into_iter()
            .filter(|backend| pg_search || *backend != TextSearchBackend::Paradedb)
            .collect();
        Ok(Some(Self {
            _guard: guard,
            database_url,
            backends,
        }))
    }

    async fn create_database(&self) -> Result<TestDatabase, Box<dyn Error>> {
        let name = self.database_name()?;
        let mut admin = PgConnection::connect(&self.database_url).await?;
        admin
            .execute(format!(r#"create database "{name}""#).as_str())
            .await?;
        admin.close().await?;
        let options = PgConnectOptions::from_str(&self.database_url)?.database(&name);
        let conn = PgConnection::connect_with(&options).await?;
        Ok(TestDatabase { name, conn })
    }

    async fn drop_named(&self, name: &str) -> Result<(), Box<dyn Error>> {
        let mut admin = PgConnection::connect(&self.database_url).await?;
        admin
            .execute(format!(r#"drop database if exists "{name}""#).as_str())
            .await?;
        admin.close().await?;
        Ok(())
    }

    fn database_name(&self) -> Result<String, Box<dyn Error>> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        Ok(format!(
            "centaur_migrations_{}_{}",
            std::process::id(),
            nanos
        ))
    }

    async fn drop_database<T>(
        &self,
        database: TestDatabase,
        result: Result<T, Box<dyn Error>>,
    ) -> Result<T, Box<dyn Error>> {
        let TestDatabase { name, conn } = database;
        let close_result = conn.close().await;
        self.drop_named(&name).await?;
        let value = result?;
        close_result?;
        Ok(value)
    }

    async fn fresh_schema(
        &self,
        backend: TextSearchBackend,
    ) -> Result<Vec<String>, Box<dyn Error>> {
        let mut database = self.create_database().await?;
        let snapshot = async {
            migrate(&mut database.conn, backend).await?;
            Ok::<_, Box<dyn Error>>(schema_snapshot(&mut database.conn).await?)
        }
        .await;
        self.drop_database(database, snapshot).await
    }
}

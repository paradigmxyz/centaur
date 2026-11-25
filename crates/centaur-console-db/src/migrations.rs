//! Console's schema migrations, which api-rs owns.
//!
//! The first migration is a baseline: the schema the last migration of the
//! retired Rails Console produced. A database that Console already migrated to
//! that point adopts the baseline without running it; one left behind is
//! refused, because the data migrations it is missing no longer exist here.
//! Every later migration runs normally.

use std::collections::BTreeSet;

use sqlx::{
    PgConnection, PgPool,
    migrate::{Migrate, Migrator},
};

use crate::SetupError as Error;

type Result<T> = std::result::Result<T, Error>;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Version of the baseline migration.
const BASELINE_VERSION: i64 = 1;

/// Session advisory lock that serializes adoption and migration across
/// processes (several api-rs replicas start together). sqlx takes its own lock
/// inside [`Migrator::run`], but only after the adoption check, which must not
/// race either.
const LOCK_KEY: i64 = 0x636f_6e73_6f6c_6501; // "console" + 1

/// Apply pending Console migrations to the database behind `pool`, adopting a
/// schema the retired Rails Console migrated. The pool must allow writes.
pub async fn migrate(pool: &PgPool) -> Result<()> {
    let mut conn = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(LOCK_KEY)
        .execute(&mut *conn)
        .await?;
    let result = async {
        adopt_rails_schema(&mut conn).await?;
        MIGRATOR.run(&mut *conn).await?;
        Ok::<_, Error>(())
    }
    .await;
    let unlocked = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(LOCK_KEY)
        .execute(&mut *conn)
        .await;
    result?;
    unlocked?;
    Ok(())
}

/// Migration versions the baseline inherits from the retired Rails Console,
/// read from the baseline's own `schema_migrations` insert so the two cannot
/// disagree.
pub fn rails_versions() -> BTreeSet<&'static str> {
    let baseline = MIGRATOR
        .iter()
        .find(|migration| migration.version == BASELINE_VERSION)
        .expect("the baseline migration is embedded");
    let inserts = baseline
        .sql
        .split_once("INSERT INTO public.schema_migrations (version) VALUES")
        .expect("the baseline records the Rails versions")
        .1;
    inserts
        .split(['(', ')', ',', ';', '\n', ' '])
        .map(|token| token.trim_matches('\''))
        .filter(|token| !token.is_empty())
        .collect()
}

/// Record the baseline as applied when the retired Rails Console already built
/// this schema.
async fn adopt_rails_schema(conn: &mut PgConnection) -> Result<()> {
    let tracked: bool = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
        .fetch_one(&mut *conn)
        .await?;
    if tracked {
        let adopted: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM _sqlx_migrations WHERE version = $1)")
                .bind(BASELINE_VERSION)
                .fetch_one(&mut *conn)
                .await?;
        if adopted {
            return Ok(());
        }
    }
    let (rails, users): (bool, bool) = sqlx::query_as(
        "SELECT to_regclass('schema_migrations') IS NOT NULL, to_regclass('users') IS NOT NULL",
    )
    .fetch_one(&mut *conn)
    .await?;
    if !rails {
        if users {
            return Err(Error::Schema(
                "the Console database has tables but no migration history; \
                 refusing to migrate it"
                    .to_owned(),
            ));
        }
        // An empty database: the baseline creates everything.
        return Ok(());
    }

    let applied: BTreeSet<String> = sqlx::query_scalar("SELECT version FROM schema_migrations")
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .collect();
    let expected = rails_versions();
    let missing: Vec<&str> = expected
        .iter()
        .copied()
        .filter(|version| !applied.contains(*version))
        .collect();
    if !missing.is_empty() {
        return Err(Error::Schema(format!(
            "the Console database is missing {} Rails migration(s), the first {}; \
             it was left behind by a Console release older than the api-rs baseline. \
             Upgrade through a chart release that still ran the Rails Console worker \
             (before the Console moved to api-rs), then upgrade again",
            missing.len(),
            missing[0],
        )));
    }
    let unknown: Vec<&str> = applied
        .iter()
        .map(String::as_str)
        .filter(|version| !expected.contains(version))
        .collect();
    if !unknown.is_empty() {
        return Err(Error::Schema(format!(
            "the Console database has Rails migration(s) the api-rs baseline does not \
             include: {}",
            unknown.join(", "),
        )));
    }

    let baseline = MIGRATOR
        .iter()
        .find(|migration| migration.version == BASELINE_VERSION)
        .expect("the baseline migration is embedded");
    conn.ensure_migrations_table().await?;
    // The row `Migrator::run` writes when it applies a migration itself.
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES ($1, $2, TRUE, $3, 0)",
    )
    .bind(baseline.version)
    .bind(&*baseline.description)
    .bind(&*baseline.checksum)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_is_the_first_migration() {
        assert_eq!(
            MIGRATOR.iter().next().map(|migration| migration.version),
            Some(BASELINE_VERSION)
        );
    }
}

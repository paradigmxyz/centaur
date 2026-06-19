//! Overlay database migrations.
//!
//! api-rs applies only its own embedded core migrations (see
//! [`PgSessionStore::run_migrations`](crate::PgSessionStore::run_migrations)).
//! The legacy Python `api` additionally applied an overlay repository's
//! `services/api/db/migrations/*.sql` (dbmate format) when `CENTAUR_OVERLAY_DIR`
//! was set; the RS rewrite dropped that, so overlay-owned tables were never
//! created.
//!
//! This restores it, applied AFTER the core migrations so overlay schema may
//! depend on core. Each pending `-- migrate:up` section is applied in filename
//! order and recorded in `schema_migrations_overlay` — a ledger independent of
//! the core `schema_migrations`, so an applied version is never re-run (an
//! upgrade from the legacy dbmate path therefore skips versions it already
//! recorded). dbmate's `transaction:false` directive is honored.

use std::path::{Path, PathBuf};

use sqlx::PgPool;

use crate::SessionStoreError;

/// Ledger table for overlay migrations, kept separate from core `schema_migrations`.
const OVERLAY_MIGRATIONS_TABLE: &str = "schema_migrations_overlay";

/// Advisory-lock key that serializes overlay-migration application across api-rs
/// replicas (the core sqlx migrator takes its own lock for the same reason). An
/// arbitrary fixed value in the application range.
const OVERLAY_MIGRATIONS_LOCK_KEY: i64 = 0x4f56_4c41_5900;

/// Apply every pending overlay migration found in `dirs`, in filename order,
/// recording each in [`OVERLAY_MIGRATIONS_TABLE`]. A directory that does not
/// exist is skipped (an opted-in source may legitimately carry no migrations).
/// Returns the number of migrations newly applied.
///
/// Serialized across processes with a Postgres advisory lock, so a scaled
/// deployment (or an overlapping rolling deploy where two api-rs pods run with
/// migrations enabled) applies the set exactly once.
pub async fn apply_overlay_migrations(
    pool: &PgPool,
    dirs: &[PathBuf],
) -> Result<usize, SessionStoreError> {
    let mut lock = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(OVERLAY_MIGRATIONS_LOCK_KEY)
        .execute(&mut *lock)
        .await?;
    let result = apply_locked(pool, dirs).await;
    // Release the session lock on every path. (A crashed process releases it on
    // disconnect, but here the connection returns to the pool still open.)
    if let Err(error) = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(OVERLAY_MIGRATIONS_LOCK_KEY)
        .execute(&mut *lock)
        .await
    {
        tracing::warn!(%error, "failed to release overlay-migrations advisory lock");
    }
    result
}

async fn apply_locked(pool: &PgPool, dirs: &[PathBuf]) -> Result<usize, SessionStoreError> {
    let create_sql = format!(
        "CREATE TABLE IF NOT EXISTS {OVERLAY_MIGRATIONS_TABLE} \
         (version varchar(255) PRIMARY KEY, applied_at timestamptz NOT NULL DEFAULT now())"
    );
    sqlx::query(&create_sql).execute(pool).await?;

    let select_sql = format!("SELECT 1 FROM {OVERLAY_MIGRATIONS_TABLE} WHERE version = $1");
    let insert_sql = format!(
        "INSERT INTO {OVERLAY_MIGRATIONS_TABLE} (version) VALUES ($1) ON CONFLICT DO NOTHING"
    );

    let mut applied = 0usize;
    for dir in dirs {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if !dir.is_dir() {
            tracing::warn!(dir = %dir.display(), "overlay migrations dir absent; skipping");
            continue;
        }
        for path in sql_files_sorted(dir)? {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_owned();
            let Some(version) = migration_version(&name) else {
                tracing::info!(file = %name, "overlay migration has no numeric version prefix; skipping");
                continue;
            };
            let already: Option<i32> = sqlx::query_scalar(&select_sql)
                .bind(version)
                .fetch_optional(pool)
                .await?;
            if already.is_some() {
                continue;
            }
            let content = std::fs::read_to_string(&path).map_err(|error| {
                SessionStoreError::OverlayMigration(format!("read {}: {error}", path.display()))
            })?;
            let up = extract_up_section(&content).ok_or_else(|| {
                SessionStoreError::OverlayMigration(format!("{name}: no '-- migrate:up' section"))
            })?;

            // dbmate's `transaction:false` (e.g. CREATE INDEX CONCURRENTLY) cannot
            // run inside a transaction. Postgres' simple protocol autocommits a
            // lone statement but wraps a multi-statement string in an implicit
            // transaction, so such migrations must be a SINGLE statement (see
            // MIGRATING_TO_RS.md); the ledger row is then written separately.
            if is_no_transaction(&content) {
                sqlx::raw_sql(&up).execute(pool).await.map_err(|error| {
                    SessionStoreError::OverlayMigration(format!("{name}: {error}"))
                })?;
                sqlx::query(&insert_sql).bind(version).execute(pool).await?;
            } else {
                // The body and the ledger insert commit together, so a failure
                // rolls back and the version is retried on the next run.
                let mut tx = pool.begin().await?;
                sqlx::raw_sql(&up)
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| {
                        SessionStoreError::OverlayMigration(format!("{name}: {error}"))
                    })?;
                sqlx::query(&insert_sql)
                    .bind(version)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
            }
            tracing::info!(version = %version, file = %name, "applied overlay migration");
            applied += 1;
        }
    }
    Ok(applied)
}

/// `*.sql` files in `dir`, sorted by filename (the `NNN_` prefix gives order).
fn sql_files_sorted(dir: &Path) -> Result<Vec<PathBuf>, SessionStoreError> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|error| {
            SessionStoreError::OverlayMigration(format!("read_dir {}: {error}", dir.display()))
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("sql"))
        .collect();
    files.sort();
    Ok(files)
}

/// The leading run of ASCII digits in `filename` (the dbmate version), or `None`
/// for a file without a numeric prefix (not a migration).
fn migration_version(filename: &str) -> Option<&str> {
    let len = filename.bytes().take_while(u8::is_ascii_digit).count();
    (len > 0).then(|| &filename[..len])
}

/// If `line` is a dbmate directive marker for `token` ("migrate:up"/"migrate:down"),
/// returns the text AFTER the token (for option parsing like `transaction:false`).
/// The `--` must be at the START of the line (column 0, as dbmate anchors markers,
/// so an indented `--` comment inside a body is NOT a marker), and the token must
/// be a whole token — followed by whitespace or end of line — so `migrate:upgrade`
/// does not match `migrate:up`.
fn marker_rest<'a>(line: &'a str, token: &str) -> Option<&'a str> {
    let after = line.strip_prefix("--")?.trim_start().strip_prefix(token)?;
    (after.is_empty() || after.starts_with(char::is_whitespace)).then_some(after)
}

fn marker_kind(line: &str) -> Option<&'static str> {
    if marker_rest(line, "migrate:up").is_some() {
        Some("up")
    } else if marker_rest(line, "migrate:down").is_some() {
        Some("down")
    } else {
        None
    }
}

/// Extract the `-- migrate:up` section. Returns `None` if there is no up marker
/// or the section is empty/whitespace (a malformed migration — fail loudly
/// rather than record an empty no-op as applied).
fn extract_up_section(content: &str) -> Option<String> {
    let mut out = String::new();
    let mut in_up = false;
    let mut saw_up = false;
    for line in content.lines() {
        match marker_kind(line) {
            Some("up") => {
                in_up = true;
                saw_up = true;
            }
            Some("down") => in_up = false,
            _ if in_up => {
                out.push_str(line);
                out.push('\n');
            }
            _ => {}
        }
    }
    // Require at least one non-blank, non-comment line so a comment-only up
    // section is treated as malformed (None) rather than recorded as an applied
    // no-op.
    let has_sql = out.lines().any(|line| {
        let t = line.trim();
        !t.is_empty() && !t.starts_with("--")
    });
    (saw_up && has_sql).then_some(out)
}

/// True when the `-- migrate:up` marker carries dbmate's `transaction:false`.
fn is_no_transaction(content: &str) -> bool {
    content
        .lines()
        .find_map(|line| marker_rest(line, "migrate:up"))
        .is_some_and(|after| after.contains("transaction:false"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_leading_digits_only() {
        assert_eq!(migration_version("001_agent_memory.sql"), Some("001"));
        assert_eq!(migration_version("010_x.sql"), Some("010"));
        assert_eq!(
            migration_version("20240101120000_x.sql"),
            Some("20240101120000")
        );
        assert_eq!(migration_version("seed.sql"), None);
        assert_eq!(migration_version("v1_x.sql"), None);
        // Quotes/semicolons can never reach SQL: only a numeric prefix is taken.
        assert_eq!(migration_version("1';drop.sql"), Some("1"));
    }

    #[test]
    fn extracts_up_section_between_anchored_markers() {
        let sql = "-- migrate:up\nCREATE TABLE a (id int);\n-- migrate:down\nDROP TABLE a;\n";
        assert_eq!(
            extract_up_section(sql).unwrap().trim(),
            "CREATE TABLE a (id int);"
        );
    }

    #[test]
    fn down_marker_inside_body_does_not_truncate() {
        // A non-line-start mention of the down marker must not flip capture off.
        let sql = "-- migrate:up\nCREATE TABLE a (id int); -- see migrate:down for rollback\nCREATE TABLE b (id int);\n-- migrate:down\nDROP TABLE a;\n";
        let up = extract_up_section(sql).unwrap();
        assert!(up.contains("CREATE TABLE a"));
        assert!(
            up.contains("CREATE TABLE b"),
            "body after the false marker must survive"
        );
        assert!(
            !up.contains("DROP TABLE a"),
            "real down section must be excluded"
        );
    }

    #[test]
    fn marker_less_or_empty_up_is_none() {
        assert_eq!(extract_up_section("CREATE TABLE a (id int);\n"), None);
        assert_eq!(
            extract_up_section("-- migrate:up\n\n-- migrate:down\nDROP TABLE a;\n"),
            None
        );
    }

    #[test]
    fn transaction_false_detected_only_on_up_marker() {
        assert!(is_no_transaction(
            "-- migrate:up transaction:false\nCREATE INDEX CONCURRENTLY i ON a (id);\n"
        ));
        assert!(!is_no_transaction(
            "-- migrate:up\nCREATE TABLE a (id int);\n"
        ));
        // A mention in the body is not the directive.
        assert!(!is_no_transaction(
            "-- migrate:up\nINSERT INTO a VALUES ('transaction:false');\n"
        ));
    }

    #[test]
    fn markers_must_be_at_column_zero() {
        // An indented `--` is a SQL comment in the body, not a directive, so it
        // must not toggle capture (matches dbmate, which anchors markers at ^).
        let sql = "-- migrate:up\nDO $$ BEGIN\n  -- migrate:down would drop it\n  PERFORM 1;\nEND $$;\nCREATE TABLE keep (id int);\n-- migrate:down\nDROP TABLE keep;\n";
        let up = extract_up_section(sql).unwrap();
        assert!(
            up.contains("PERFORM 1"),
            "indented marker must not truncate"
        );
        assert!(up.contains("CREATE TABLE keep"));
        assert!(!up.contains("DROP TABLE keep"));
    }

    #[test]
    fn marker_requires_whole_token() {
        // `migrate:upgrade` / `migrate:downstream` are not the up/down markers.
        assert_eq!(marker_kind("-- migrate:upgrade the schema"), None);
        assert_eq!(marker_kind("-- migrate:downstream"), None);
        assert_eq!(marker_kind("-- migrate:up"), Some("up"));
        assert_eq!(marker_kind("--migrate:up"), Some("up")); // no space after --
        assert_eq!(marker_kind("-- migrate:down"), Some("down"));
        let sql = "-- migrate:up\n-- migrate:upgrade note\nCREATE TABLE a (id int);\n";
        assert!(extract_up_section(sql).unwrap().contains("CREATE TABLE a"));
    }

    #[test]
    fn comment_only_up_section_is_none() {
        // No real SQL — only comments — must be treated as malformed, not a no-op.
        assert_eq!(
            extract_up_section("-- migrate:up\n-- just a note\n-- migrate:down\nDROP TABLE a;\n"),
            None
        );
    }
}

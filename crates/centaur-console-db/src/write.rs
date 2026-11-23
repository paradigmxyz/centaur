//! Timestamped writes for Console's tables.
//!
//! Every Console table carries `created_at`/`updated_at`. Both are set on
//! insert, and `updated_at` is bumped on update **only when a column actually
//! changes**: an unchanged `update` is a no-op, so `updated_at` keeps its value.
//! The SPA sorts on `updated_at`, so the difference is observable.
//!
//! These helpers build the SQL; the caller binds the values in column order:
//!
//! ```ignore
//! use centaur_console_db::write;
//!
//! let sql = write::insert_returning_sql("api_keys", &["user_id", "name", "token_hash"], "id, name");
//! let row: ApiKeyRow = sqlx::query_as(&sql)
//!     .bind(user_id)
//!     .bind(name)
//!     .bind(token_hash)
//!     .fetch_one(pool)
//!     .await?;
//!
//! let sql = write::update_changed_sql("api_clients", &["name", "scopes"]);
//! sqlx::query(&sql).bind(name).bind(scopes).bind(id).execute(pool).await?;
//! ```
//!
//! A write that intentionally keeps `updated_at` (a partial column write, a
//! soft delete setting only `deleted_at`) stays a plain statement in its area;
//! do not route it through these helpers.
//! [`update_where_sql`] is the same update addressed by a column other than the
//! primary key.

/// `INSERT INTO <table> (<columns>, created_at, updated_at) VALUES ($1, ...,
/// now(), now())`. Binds are positional in `columns` order.
pub fn insert_sql(table: &str, columns: &[&str]) -> String {
    let mut names: Vec<String> = columns.iter().map(|column| ident(column)).collect();
    names.push("created_at".to_owned());
    names.push("updated_at".to_owned());
    let mut values: Vec<String> = (1..=columns.len())
        .map(|index| format!("${index}"))
        .collect();
    values.push("now()".to_owned());
    values.push("now()".to_owned());
    format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        names.join(", "),
        values.join(", ")
    )
}

/// [`insert_sql`] plus `RETURNING <returning>`, for an insert that renders the
/// stored row.
pub fn insert_returning_sql(table: &str, columns: &[&str], returning: &str) -> String {
    format!("{} RETURNING {returning}", insert_sql(table, columns))
}

/// `UPDATE <table> SET <column> = $n, ..., updated_at = now() WHERE id = $k`.
/// Binds are the columns in order, then the id.
pub fn update_sql(table: &str, columns: &[&str]) -> String {
    update_with(table, columns, None)
}

/// Like [`update_sql`], but only when at least one of `columns` changes value:
/// `... WHERE id = $k AND (<columns>) IS DISTINCT FROM (<bindings>)`. An
/// unchanged write affects no rows and leaves `updated_at` alone.
pub fn update_changed_sql(table: &str, columns: &[&str]) -> String {
    update_with(table, columns, Some(columns))
}

fn update_with(table: &str, columns: &[&str], changed: Option<&[&str]>) -> String {
    let mut assignments: Vec<String> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| format!("{} = ${}", ident(column), index + 1))
        .collect();
    assignments.push("updated_at = now()".to_owned());
    let id = columns.len() + 1;
    let mut sql = format!(
        "UPDATE {table} SET {} WHERE id = ${id}",
        assignments.join(", ")
    );
    if let Some(changed) = changed {
        let bindings: Vec<String> = (1..=changed.len())
            .map(|index| format!("${index}"))
            .collect();
        sql.push_str(&format!(
            " AND ({}) IS DISTINCT FROM ({})",
            changed
                .iter()
                .map(|column| ident(column))
                .collect::<Vec<_>>()
                .join(", "),
            bindings.join(", ")
        ));
    }
    sql
}

/// Like [`update_sql`], but addressed by a `WHERE` clause instead of the
/// primary key. The assignments bind `$1..=$n`; `from_where` refers to the
/// parameters after them (`broker_credential_id = $2`, for example).
pub fn update_where_sql(table: &str, columns: &[&str], from_where: &str) -> String {
    let mut assignments: Vec<String> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| format!("{} = ${}", ident(column), index + 1))
        .collect();
    assignments.push("updated_at = now()".to_owned());
    format!(
        "UPDATE {table} SET {} WHERE {from_where}",
        assignments.join(", ")
    )
}

/// A column name as a quoted SQL identifier. Every builder quotes the columns it
/// is given, so callers pass plain names and reserved words (`grant`,
/// `default`) need no special handling.
fn ident(column: &str) -> String {
    assert!(
        !column.contains('"'),
        "pass plain column names; the SQL builders quote them: {column}"
    );
    format!("\"{column}\"")
}

/// `UPDATE <table> SET updated_at = now() WHERE id = $1`, a touch.
pub fn touch_sql(table: &str) -> String {
    format!("UPDATE {table} SET updated_at = now() WHERE id = $1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_always_set_both_timestamps() {
        assert_eq!(
            insert_sql("api_keys", &["user_id", "name", "token_hash"]),
            "INSERT INTO api_keys (\"user_id\", \"name\", \"token_hash\", created_at, updated_at) \
             VALUES ($1, $2, $3, now(), now())"
        );
        assert_eq!(
            insert_returning_sql("api_keys", &["user_id"], "id, created_at"),
            "INSERT INTO api_keys (\"user_id\", created_at, updated_at) VALUES ($1, now(), now()) \
             RETURNING id, created_at"
        );
    }

    #[test]
    fn updates_only_bump_updated_at_when_a_column_changes() {
        assert_eq!(
            update_changed_sql("api_clients", &["name", "scopes"]),
            "UPDATE api_clients SET \"name\" = $1, \"scopes\" = $2, updated_at = now() \
             WHERE id = $3 AND (\"name\", \"scopes\") IS DISTINCT FROM ($1, $2)"
        );
        assert_eq!(
            update_changed_sql("roles", &["name"]),
            "UPDATE roles SET \"name\" = $1, updated_at = now() \
             WHERE id = $2 AND (\"name\") IS DISTINCT FROM ($1)"
        );
        assert_eq!(
            update_sql("roles", &["name"]),
            "UPDATE roles SET \"name\" = $1, updated_at = now() WHERE id = $2"
        );
        assert_eq!(
            update_where_sql(
                "static_secrets",
                &["broker_credential_id"],
                "broker_credential_id = $2"
            ),
            "UPDATE static_secrets SET \"broker_credential_id\" = $1, updated_at = now() \
             WHERE broker_credential_id = $2"
        );
        assert_eq!(
            touch_sql("principals"),
            "UPDATE principals SET updated_at = now() WHERE id = $1"
        );
    }
}

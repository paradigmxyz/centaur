//! Console's singleton system settings and the roles assigned by default to new
//! principals. The default-role replacement runs in the same transaction as the
//! settings write.

use sqlx::PgPool;
use sqlx::postgres::PgConnection;
use time::PrimitiveDateTime;

use crate::Result;
use crate::model::{ConsoleModel, SystemSetting as SystemSettingModel};
use crate::write;

const SETTING_COLUMNS: &str = "\
    id, default_sandbox_repo_cache, default_sandbox_observability_enabled, \
    default_sandbox_sessions_read_enabled, default_sandbox_workflows_read_enabled, \
    default_sandbox_workflows_write_enabled, updated_at";

/// The columns a settings write assigns, in bind order.
const SETTING_VALUE_COLUMNS: [&str; 5] = [
    "default_sandbox_repo_cache",
    "default_sandbox_observability_enabled",
    "default_sandbox_sessions_read_enabled",
    "default_sandbox_workflows_read_enabled",
    "default_sandbox_workflows_write_enabled",
];

/// The values the System Settings page may change. This is also the row
/// projection for those columns and the `data` of the settings payload, so the
/// column list, the row, and the wire keys are declared once.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow, serde::Serialize)]
pub struct SystemSettingValues {
    pub default_sandbox_repo_cache: String,
    pub default_sandbox_observability_enabled: bool,
    pub default_sandbox_sessions_read_enabled: bool,
    pub default_sandbox_workflows_read_enabled: bool,
    pub default_sandbox_workflows_write_enabled: bool,
}

/// The stored singleton: its id, its values, and when they last changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemSettingRow {
    pub id: i64,
    pub values: SystemSettingValues,
    pub updated_at: PrimitiveDateTime,
}

impl sqlx::FromRow<'_, sqlx::postgres::PgRow> for SystemSettingRow {
    fn from_row(row: &sqlx::postgres::PgRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            values: SystemSettingValues::from_row(row)?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct RoleRow {
    pub id: i64,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
    pub assign_by_default: bool,
}

fn missing() -> crate::Error {
    crate::Error::InvalidData("system settings row is missing".to_owned())
}

/// Console's single settings row, created on first read.
pub async fn system_setting(pool: &PgPool) -> Result<SystemSettingRow> {
    let query = format!(
        "SELECT {SETTING_COLUMNS} FROM {} WHERE singleton ORDER BY id LIMIT 1",
        SystemSettingModel::TABLE
    );
    let record: Option<SystemSettingRow> = sqlx::query_as(&query).fetch_optional(pool).await?;
    if let Some(record) = record {
        return Ok(record);
    }
    // A concurrent request may win the insert; either way we then read it back.
    sqlx::query(&format!(
        "{} ON CONFLICT (singleton) DO NOTHING",
        write::insert_sql(SystemSettingModel::TABLE, &["singleton"])
    ))
    .bind(true)
    .execute(pool)
    .await?;
    let record: Option<SystemSettingRow> = sqlx::query_as(&query).fetch_optional(pool).await?;
    record.ok_or_else(missing)
}

/// Saves the settings, then replaces the default roles, in one transaction. An
/// unchanged settings write is skipped so `updated_at` keeps its value, and the
/// default roles are only replaced when the request sent `default_role_ids`.
pub async fn write_system_settings(
    pool: &PgPool,
    id: i64,
    values: &SystemSettingValues,
    replace_roles: Option<&[i64]>,
) -> Result<()> {
    let mut transaction = pool.begin().await?;
    update_in(&mut transaction, id, values).await?;
    if let Some(role_ids) = replace_roles {
        replace_roles_in(&mut transaction, role_ids).await?;
    }
    transaction.commit().await?;
    Ok(())
}

async fn update_in(
    connection: &mut PgConnection,
    id: i64,
    values: &SystemSettingValues,
) -> Result<()> {
    // The dirty guard makes the write a no-op when a concurrent request already
    // stored these values, so `updated_at` is not bumped needlessly.
    let sql = write::update_changed_sql(SystemSettingModel::TABLE, &SETTING_VALUE_COLUMNS);
    sqlx::query(&sql)
        .bind(&values.default_sandbox_repo_cache)
        .bind(values.default_sandbox_observability_enabled)
        .bind(values.default_sandbox_sessions_read_enabled)
        .bind(values.default_sandbox_workflows_read_enabled)
        .bind(values.default_sandbox_workflows_write_enabled)
        .bind(id)
        .execute(&mut *connection)
        .await?;
    Ok(())
}

/// Every role, ordered as `Role.order(:name, :foreign_id, :id)`.
pub async fn roles(pool: &PgPool) -> Result<Vec<RoleRow>> {
    let rows: Vec<RoleRow> = sqlx::query_as(
        "SELECT id, foreign_id, name, assign_by_default FROM roles \
         ORDER BY name ASC, foreign_id ASC, id ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// The subset of `role_ids` that exist, so unknown ids can be reported instead
/// of silently dropped.
pub async fn existing_role_ids(pool: &PgPool, role_ids: &[i64]) -> Result<Vec<i64>> {
    if role_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(
        sqlx::query_scalar("SELECT id FROM roles WHERE id = ANY($1) ORDER BY id ASC")
            .bind(role_ids)
            .fetch_all(pool)
            .await?,
    )
}

/// Make exactly `role_ids` the roles new principals receive.
pub async fn replace_default_roles(pool: &PgPool, role_ids: &[i64]) -> Result<()> {
    let mut transaction = pool.begin().await?;
    replace_roles_in(&mut transaction, role_ids).await?;
    transaction.commit().await?;
    Ok(())
}

/// Both statements set a set of rows rather than one id, which the shared
/// update helper does not address.
async fn replace_roles_in(connection: &mut PgConnection, role_ids: &[i64]) -> Result<()> {
    sqlx::query(
        "UPDATE roles SET assign_by_default = false, updated_at = now() \
         WHERE assign_by_default AND NOT (id = ANY($1))",
    )
    .bind(role_ids)
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "UPDATE roles SET assign_by_default = true, updated_at = now() \
         WHERE id = ANY($1) AND NOT assign_by_default",
    )
    .bind(role_ids)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

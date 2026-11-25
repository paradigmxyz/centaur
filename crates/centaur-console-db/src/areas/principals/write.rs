//! Writes for the principals area, including the side effects that go with them
//! (default roles, sync-config cache bumps, reconciliation).

use std::collections::BTreeSet;

use serde_json::Value;
use sqlx::{PgConnection, PgPool};

use super::read::{load_principal_view, load_role_view, principal_roles, role_payload};
use super::request::{PermissionWrite, PrincipalWrite, RoleWrite};
use super::{
    PERMISSION_ATTRIBUTES, PRINCIPAL_COLUMNS, PRINCIPAL_RESERVED_PREFIX, PermissionPayload,
    PrincipalError, PrincipalRow, ROLE_COLUMNS, ROLE_RESERVED_PREFIX, Result, RolePayload, RoleRow,
    grantee_principals, principal_defaults,
};
use crate::areas::reconciliation;
use crate::areas::secrets::Grantee;
use crate::invalidate::{self, Scope};
use crate::model::{self, ConsoleModel};
use crate::timestamp;
use crate::validation::{ValidateFields, Validation};
use crate::write;

#[derive(Clone, Debug)]
struct PrincipalDraft {
    id: Option<i64>,
    original: Option<PrincipalRow>,
    foreign_id: Option<String>,
    name: Option<String>,
    kind: Option<String>,
    labels: Value,
    slack_user_id: Option<String>,
    slack_channel_id: Option<String>,
    slack_team_id: Option<String>,
    slack_email: Option<String>,
    console_user_id: Option<i64>,
    sandbox_repo_cache: Option<String>,
    sandbox_observability_enabled: Option<bool>,
    sandbox_sessions_read_enabled: Option<bool>,
    sandbox_workflows_read_enabled: Option<bool>,
    sandbox_workflows_write_enabled: Option<bool>,
    created_by_id: i64,
    provided: BTreeSet<&'static str>,
}

impl PrincipalDraft {
    fn new(foreign_id: Option<String>, created_by_id: i64) -> Self {
        Self {
            id: None,
            original: None,
            foreign_id,
            name: None,
            kind: Some("unknown".to_owned()),
            labels: serde_json::json!({}),
            slack_user_id: None,
            slack_channel_id: None,
            slack_team_id: None,
            slack_email: None,
            console_user_id: None,
            sandbox_repo_cache: Some("all".to_owned()),
            sandbox_observability_enabled: Some(true),
            sandbox_sessions_read_enabled: Some(false),
            sandbox_workflows_read_enabled: Some(false),
            sandbox_workflows_write_enabled: Some(false),
            created_by_id,
            provided: BTreeSet::new(),
        }
    }

    fn from_row(row: &PrincipalRow) -> Self {
        Self {
            id: Some(row.id),
            original: Some(row.clone()),
            foreign_id: row.foreign_id.clone(),
            name: row.name.clone(),
            kind: Some(row.kind.clone()),
            labels: row.labels.clone(),
            slack_user_id: row.slack_user_id.clone(),
            slack_channel_id: row.slack_channel_id.clone(),
            slack_team_id: row.slack_team_id.clone(),
            slack_email: row.slack_email.clone(),
            console_user_id: row.console_user_id,
            sandbox_repo_cache: Some(row.sandbox_repo_cache.clone()),
            sandbox_observability_enabled: Some(row.sandbox_observability_enabled),
            sandbox_sessions_read_enabled: Some(row.sandbox_sessions_read_enabled),
            sandbox_workflows_read_enabled: Some(row.sandbox_workflows_read_enabled),
            sandbox_workflows_write_enabled: Some(row.sandbox_workflows_write_enabled),
            created_by_id: row.created_by_id,
            provided: BTreeSet::new(),
        }
    }

    fn new_record(&self) -> bool {
        self.id.is_none()
    }

    fn apply(&mut self, write: &PrincipalWrite) {
        if let Some(value) = &write.name {
            self.name = value.clone();
            self.provided.insert("name");
        }
        if let Some(value) = &write.kind {
            self.kind = value.clone();
            self.provided.insert("kind");
        }
        if let Some(value) = &write.labels {
            self.labels = Value::Object(value.clone());
            self.provided.insert("labels");
        }
        if let Some(value) = &write.slack_user_id {
            self.slack_user_id = value.clone();
            self.provided.insert("slack_user_id");
        }
        if let Some(value) = &write.slack_channel_id {
            self.slack_channel_id = value.clone();
            self.provided.insert("slack_channel_id");
        }
        if let Some(value) = &write.slack_team_id {
            self.slack_team_id = value.clone();
            self.provided.insert("slack_team_id");
        }
        if let Some(value) = &write.slack_email {
            self.slack_email = value.clone();
            self.provided.insert("slack_email");
        }
        if let Some(value) = &write.console_user_id {
            self.console_user_id = *value;
            self.provided.insert("console_user_id");
        }
        if let Some(value) = &write.sandbox_repo_cache {
            self.sandbox_repo_cache = value.clone();
            self.provided.insert("sandbox_repo_cache");
        }
        if let Some(value) = &write.sandbox_observability_enabled {
            self.sandbox_observability_enabled = *value;
            self.provided.insert("sandbox_observability_enabled");
        }
        if let Some(value) = &write.sandbox_sessions_read_enabled {
            self.sandbox_sessions_read_enabled = *value;
            self.provided.insert("sandbox_sessions_read_enabled");
        }
        if let Some(value) = &write.sandbox_workflows_read_enabled {
            self.sandbox_workflows_read_enabled = *value;
            self.provided.insert("sandbox_workflows_read_enabled");
        }
        if let Some(value) = &write.sandbox_workflows_write_enabled {
            self.sandbox_workflows_write_enabled = *value;
            self.provided.insert("sandbox_workflows_write_enabled");
        }
    }

    async fn apply_defaults(&mut self, conn: &mut PgConnection) -> Result<()> {
        if !self.new_record() {
            return Ok(());
        }
        let defaults = principal_defaults(conn).await?;
        if !self.provided.contains("sandbox_repo_cache") {
            self.sandbox_repo_cache = Some(defaults.sandbox_repo_cache);
        }
        if !self.provided.contains("sandbox_observability_enabled") {
            self.sandbox_observability_enabled = Some(defaults.sandbox_observability_enabled);
        }
        if !self.provided.contains("sandbox_sessions_read_enabled") {
            self.sandbox_sessions_read_enabled = Some(defaults.sandbox_sessions_read_enabled);
        }
        if !self.provided.contains("sandbox_workflows_read_enabled") {
            self.sandbox_workflows_read_enabled = Some(defaults.sandbox_workflows_read_enabled);
        }
        if !self.provided.contains("sandbox_workflows_write_enabled") {
            self.sandbox_workflows_write_enabled = Some(defaults.sandbox_workflows_write_enabled);
        }
        Ok(())
    }

    /// Links the console user by Slack email, called only when the body carried
    /// a `slack_email` key.
    async fn link_console_user(&mut self, conn: &mut PgConnection) -> Result<()> {
        if self.kind.as_deref() != Some("slack_dm") {
            return Ok(());
        }
        let Some(email) = self
            .slack_email
            .as_deref()
            .map(str::trim)
            .filter(|email| !email.is_empty())
        else {
            return Ok(());
        };
        self.console_user_id =
            sqlx::query_scalar("SELECT id FROM users WHERE email = $1 AND email <> $2")
                .bind(email.to_lowercase())
                .bind(crate::SYSTEM_USER_EMAIL)
                .fetch_optional(conn)
                .await?;
        Ok(())
    }

    fn apply_sandbox_repo_cache_label(&mut self) {
        let mut labels = self.labels.as_object().cloned().unwrap_or_default();
        labels.insert(
            super::SANDBOX_REPO_CACHE_LABEL.to_owned(),
            self.sandbox_repo_cache
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        self.labels = Value::Object(labels);
    }

    fn sync_config_changed(&self) -> bool {
        let Some(original) = &self.original else {
            return false;
        };
        self.name != original.name
            || self.labels != original.labels
            || self.kind.as_deref() != Some(original.kind.as_str())
            || self.slack_user_id != original.slack_user_id
            || self.slack_channel_id != original.slack_channel_id
            || self.slack_team_id != original.slack_team_id
            || self.slack_email != original.slack_email
            || self.console_user_id != original.console_user_id
            || self.sandbox_sessions_read_enabled != Some(original.sandbox_sessions_read_enabled)
            || self.sandbox_workflows_read_enabled != Some(original.sandbox_workflows_read_enabled)
            || self.sandbox_workflows_write_enabled
                != Some(original.sandbox_workflows_write_enabled)
    }

    /// The `foreign_id` a write validates: a blank one is no foreign id at all.
    fn non_blank_foreign_id(&self) -> Option<&str> {
        self.foreign_id
            .as_deref()
            .filter(|foreign_id| !foreign_id.trim().is_empty())
    }

    /// A format validation runs only on a value this write carried *and*
    /// changed, so the stored value of an untouched or re-sent field is not
    /// re-checked. `None` is both "no rule" and "cleared field".
    fn revalidated(&self, attribute: &str) -> Option<&str> {
        let original = self.original.as_ref();
        let (current, stored) = match attribute {
            "slack_user_id" => (
                self.slack_user_id.as_deref(),
                original.and_then(|row| row.slack_user_id.as_deref()),
            ),
            "slack_channel_id" => (
                self.slack_channel_id.as_deref(),
                original.and_then(|row| row.slack_channel_id.as_deref()),
            ),
            "slack_team_id" => (
                self.slack_team_id.as_deref(),
                original.and_then(|row| row.slack_team_id.as_deref()),
            ),
            "slack_email" => (
                self.slack_email.as_deref(),
                original.and_then(|row| row.slack_email.as_deref()),
            ),
            _ => return None,
        };
        (self.provided.contains(attribute) && current != stored)
            .then_some(current)
            .flatten()
    }
    /// The request the merged draft would have come from: every field a rule
    /// applies to is present with the value this write will store, except the
    /// `slack_*` fields, which stay absent unless this write changed them.
    fn validation_write(&self) -> PrincipalWrite {
        PrincipalWrite {
            foreign_id: self.non_blank_foreign_id().map(str::to_owned),
            kind: Some(self.kind.clone()),
            sandbox_repo_cache: Some(self.sandbox_repo_cache.clone()),
            slack_user_id: self
                .revalidated("slack_user_id")
                .map(|value| Some(value.to_owned())),
            slack_channel_id: self
                .revalidated("slack_channel_id")
                .map(|value| Some(value.to_owned())),
            slack_team_id: self
                .revalidated("slack_team_id")
                .map(|value| Some(value.to_owned())),
            slack_email: self
                .revalidated("slack_email")
                .map(|value| Some(value.to_owned())),
            ..PrincipalWrite::default()
        }
    }
}

/// The record's own validations. A taken `foreign_id` needs the database, so it
/// is merged with the field rules; a reserved prefix short-circuits the query as
/// it always did.
async fn validate_principal(conn: &mut PgConnection, draft: &PrincipalDraft) -> Result<()> {
    let mut validation = Validation::new();

    if let Some(foreign_id) = draft
        .non_blank_foreign_id()
        .filter(|foreign_id| !foreign_id.starts_with(PRINCIPAL_RESERVED_PREFIX))
    {
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM principals WHERE foreign_id = $1 AND id <> $2)",
        )
        .bind(foreign_id)
        .bind(draft.id.unwrap_or(0))
        .fetch_one(&mut *conn)
        .await?;
        if taken {
            validation.add("foreign_id", FOREIGN_ID_TAKEN);
        }
    }

    validation.check(&draft.validation_write());
    validation.into_result()?;
    Ok(())
}

/// The principal columns a write binds, in bind order.
const PRINCIPAL_WRITE_COLUMNS: [&str; 13] = [
    "name",
    "kind",
    "labels",
    "slack_user_id",
    "slack_channel_id",
    "slack_team_id",
    "slack_email",
    "console_user_id",
    "sandbox_repo_cache",
    "sandbox_observability_enabled",
    "sandbox_sessions_read_enabled",
    "sandbox_workflows_read_enabled",
    "sandbox_workflows_write_enabled",
];

/// Inserts a new principal or updates the stored one, from the same column
/// list: an insert is those columns plus `foreign_id`/`created_by_id`.
async fn persist_principal_row(
    conn: &mut PgConnection,
    draft: &PrincipalDraft,
) -> Result<PrincipalRow> {
    let new = draft.id.is_none();
    let sql = if new {
        let mut columns = vec!["foreign_id"];
        columns.extend(PRINCIPAL_WRITE_COLUMNS);
        columns.push("created_by_id");
        write::insert_returning_sql(model::Principal::TABLE, &columns, PRINCIPAL_COLUMNS)
    } else {
        format!(
            "{} RETURNING {PRINCIPAL_COLUMNS}",
            write::update_sql(model::Principal::TABLE, &PRINCIPAL_WRITE_COLUMNS)
        )
    };
    let mut query = sqlx::query_as(&sql);
    if new {
        query = query.bind(&draft.foreign_id);
    }
    query = query
        .bind(&draft.name)
        .bind(&draft.kind)
        .bind(&draft.labels)
        .bind(&draft.slack_user_id)
        .bind(&draft.slack_channel_id)
        .bind(&draft.slack_team_id)
        .bind(&draft.slack_email)
        .bind(draft.console_user_id)
        .bind(&draft.sandbox_repo_cache)
        .bind(draft.sandbox_observability_enabled)
        .bind(draft.sandbox_sessions_read_enabled)
        .bind(draft.sandbox_workflows_read_enabled)
        .bind(draft.sandbox_workflows_write_enabled)
        .bind(draft.id.unwrap_or(draft.created_by_id));
    Ok(query.fetch_one(&mut *conn).await?)
}

async fn assign_default_roles(conn: &mut PgConnection, principal_id: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO principal_roles (principal_id, role_id, created_at, updated_at) \
         SELECT $1, id, $2, $2 FROM roles WHERE assign_by_default",
    )
    .bind(principal_id)
    .bind(timestamp::now())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Applies a merged principal draft and runs the side effects a save fires:
/// default roles on insert, a sync-config bump on a changed update, the
/// submitted permission set, and credential reconciliation.
async fn persist_principal(
    conn: &mut PgConnection,
    mut draft: PrincipalDraft,
    write: &PrincipalWrite,
    was_new: bool,
) -> Result<PrincipalRow> {
    draft.apply(write);
    draft.apply_defaults(conn).await?;
    if write.slack_email.is_some() {
        draft.link_console_user(conn).await?;
    }
    draft.apply_sandbox_repo_cache_label();
    validate_principal(conn, &draft).await?;
    let row = if was_new {
        let row = persist_principal_row(conn, &draft).await?;
        assign_default_roles(conn, row.id).await?;
        row
    } else {
        let changed = draft.sync_config_changed();
        let row = persist_principal_row(conn, &draft).await?;
        if changed {
            invalidate::invalidate(conn, Scope::Principal(row.id)).await?;
        }
        row
    };
    if let Some(permissions) = &write.slack_channel_permissions {
        replace_permissions_in_tx(
            conn,
            Grantee::Principal(row.id),
            permissions.as_deref().unwrap_or_default(),
            false,
        )
        .await?;
    }
    reconciliation::apply_for_principal(conn, row.id).await?;
    Ok(row)
}

pub async fn create_principal(
    pool: &PgPool,
    write: &PrincipalWrite,
    created_by: i64,
) -> Result<super::PrincipalView> {
    let draft = PrincipalDraft::new(write.foreign_id.clone(), created_by);
    let mut tx = pool.begin().await?;
    let row = persist_principal(&mut tx, draft, write, true).await?;
    tx.commit().await?;
    load_principal_view(pool, row.id)
        .await?
        .ok_or_else(|| PrincipalError::Internal("principal vanished after create".to_owned()))
}

pub async fn upsert_principal(
    pool: &PgPool,
    identifier: &str,
    write: &PrincipalWrite,
    created_by: i64,
) -> Result<(super::PrincipalView, bool)> {
    let (row, was_new) = match upsert_principal_once(pool, identifier, write, created_by).await {
        // Two first-time upserts of one foreign id race to insert it; the loser
        // retries once and updates the row the winner created.
        Err(error) if lost_insert_race(&error, identifier, PRINCIPAL_RESERVED_PREFIX) => {
            upsert_principal_once(pool, identifier, write, created_by).await?
        }
        result => result?,
    };
    let view = load_principal_view(pool, row.id)
        .await?
        .ok_or_else(|| PrincipalError::Internal("principal vanished after upsert".to_owned()))?;
    Ok((view, was_new))
}

async fn upsert_principal_once(
    pool: &PgPool,
    identifier: &str,
    write: &PrincipalWrite,
    created_by: i64,
) -> Result<(PrincipalRow, bool)> {
    let mut tx = pool.begin().await?;
    let (draft, was_new) = if identifier.starts_with(PRINCIPAL_RESERVED_PREFIX) {
        let row = model::find_by_oid::<model::Principal, PrincipalRow, _>(&mut *tx, identifier)
            .await?
            .ok_or_else(model::Principal::not_found)?;
        (PrincipalDraft::from_row(&row), false)
    } else {
        match model::find_by_foreign_id::<model::Principal, PrincipalRow, _>(&mut *tx, identifier)
            .await?
        {
            Some(row) => (PrincipalDraft::from_row(&row), false),
            None => {
                let mut draft = PrincipalDraft::new(Some(identifier.to_owned()), created_by);
                draft.id = None;
                (draft, true)
            }
        }
    };

    let row = persist_principal(&mut tx, draft, write, was_new).await?;
    tx.commit().await?;
    Ok((row, was_new))
}

pub async fn delete_principal(pool: &PgPool, view: &super::PrincipalView) -> Result<()> {
    let mut tx = pool.begin().await?;
    let id = view.row.id;
    sqlx::query("DELETE FROM grants WHERE principal_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM principal_roles WHERE principal_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM slack_channel_permissions WHERE principal_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    model::delete::<model::Principal, _>(&mut *tx, id).await?;
    tx.commit().await?;
    Ok(())
}

// --- roles -----------------------------------------------------------------

#[derive(Clone, Debug)]
struct RoleDraft {
    id: Option<i64>,
    foreign_id: Option<String>,
    name: Option<String>,
    labels: Value,
    created_by_id: i64,
    provided: BTreeSet<&'static str>,
}

impl RoleDraft {
    fn new(foreign_id: Option<String>, created_by_id: i64) -> Self {
        Self {
            id: None,
            foreign_id,
            name: None,
            labels: serde_json::json!({}),
            created_by_id,
            provided: BTreeSet::new(),
        }
    }

    fn from_row(row: &RoleRow) -> Self {
        Self {
            id: Some(row.id),
            foreign_id: row.foreign_id.clone(),
            name: row.name.clone(),
            labels: row.labels.clone(),
            created_by_id: row.created_by_id,
            provided: BTreeSet::new(),
        }
    }

    fn apply(&mut self, write: &RoleWrite) {
        if let Some(value) = &write.name {
            self.name = value.clone();
            self.provided.insert("name");
        }
        if let Some(value) = &write.labels {
            self.labels = Value::Object(value.clone());
            self.provided.insert("labels");
        }
    }
}

/// The record's own validations. A taken `foreign_id` needs the database and a
/// stored `labels` value is not always the object the request type guarantees,
/// so both are checked here while the field rules run on the request built from
/// the draft.
async fn validate_role(conn: &mut PgConnection, draft: &RoleDraft) -> Result<()> {
    let mut validation = Validation::new();

    let foreign_id = draft
        .foreign_id
        .as_deref()
        .filter(|foreign_id| !foreign_id.trim().is_empty());
    if let Some(foreign_id) = foreign_id.filter(|id| !id.starts_with(ROLE_RESERVED_PREFIX)) {
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM roles WHERE foreign_id = $1 AND id <> $2)",
        )
        .bind(foreign_id)
        .bind(draft.id.unwrap_or(0))
        .fetch_one(&mut *conn)
        .await?;
        if taken {
            validation.add("foreign_id", FOREIGN_ID_TAKEN);
        }
    }

    validation.check(&RoleWrite {
        foreign_id: foreign_id.map(str::to_owned),
        ..RoleWrite::default()
    });
    if !draft.labels.is_object() {
        validation.add("labels", "must be a hash");
    }
    validation.into_result()?;
    Ok(())
}

async fn insert_role(conn: &mut PgConnection, draft: &RoleDraft) -> Result<RoleRow> {
    let sql = write::insert_returning_sql(
        model::Role::TABLE,
        &[
            "foreign_id",
            "name",
            "labels",
            "assign_by_default",
            "created_by_id",
        ],
        ROLE_COLUMNS,
    );
    Ok(sqlx::query_as(&sql)
        .bind(&draft.foreign_id)
        .bind(&draft.name)
        .bind(&draft.labels)
        .bind(false)
        .bind(draft.created_by_id)
        .fetch_one(&mut *conn)
        .await?)
}

async fn update_role(conn: &mut PgConnection, draft: &RoleDraft) -> Result<RoleRow> {
    let sql = format!(
        "{} RETURNING {ROLE_COLUMNS}",
        write::update_sql(model::Role::TABLE, &["name", "labels"])
    );
    Ok(sqlx::query_as(&sql)
        .bind(&draft.name)
        .bind(&draft.labels)
        .bind(draft.id.expect("persisted role"))
        .fetch_one(&mut *conn)
        .await?)
}

/// Applies a merged role draft, writes it, and stores the submitted permission
/// set.
async fn persist_role(
    conn: &mut PgConnection,
    mut draft: RoleDraft,
    write: &RoleWrite,
    was_new: bool,
) -> Result<RoleRow> {
    draft.apply(write);
    validate_role(conn, &draft).await?;
    let row = if was_new {
        insert_role(conn, &draft).await?
    } else {
        update_role(conn, &draft).await?
    };
    if let Some(permissions) = &write.slack_channel_permissions {
        replace_permissions_in_tx(
            conn,
            Grantee::Role(row.id),
            permissions.as_deref().unwrap_or_default(),
            false,
        )
        .await?;
    }
    Ok(row)
}

pub async fn create_role(
    pool: &PgPool,
    write: &RoleWrite,
    created_by: i64,
) -> Result<super::RoleView> {
    let draft = RoleDraft::new(write.foreign_id.clone(), created_by);
    let mut tx = pool.begin().await?;
    let row = persist_role(&mut tx, draft, write, true).await?;
    tx.commit().await?;
    load_role_view(pool, row.id)
        .await?
        .ok_or_else(|| PrincipalError::Internal("role vanished after create".to_owned()))
}

pub async fn upsert_role(
    pool: &PgPool,
    identifier: &str,
    write: &RoleWrite,
    created_by: i64,
) -> Result<(super::RoleView, bool)> {
    let (row, was_new) = match upsert_role_once(pool, identifier, write, created_by).await {
        // As for principals: the loser of a first-insert race retries once.
        Err(error) if lost_insert_race(&error, identifier, ROLE_RESERVED_PREFIX) => {
            upsert_role_once(pool, identifier, write, created_by).await?
        }
        result => result?,
    };
    let view = load_role_view(pool, row.id)
        .await?
        .ok_or_else(|| PrincipalError::Internal("role vanished after upsert".to_owned()))?;
    Ok((view, was_new))
}

const FOREIGN_ID_TAKEN: &str = "has already been taken";

/// An upsert addressed by foreign id (not by oid) that lost a first-insert race:
/// another request created the same record after our read. Depending on timing
/// that surfaces as the uniqueness check's error or as the unique index's.
fn lost_insert_race(error: &PrincipalError, identifier: &str, reserved_prefix: &str) -> bool {
    if identifier.starts_with(reserved_prefix) {
        return false;
    }
    match error {
        PrincipalError::Database(sqlx::Error::Database(database)) => {
            database.code().as_deref() == Some("23505")
        }
        PrincipalError::Validation(errors) => errors.entries().any(|(attribute, messages)| {
            attribute == "foreign_id" && messages.iter().any(|message| message == FOREIGN_ID_TAKEN)
        }),
        _ => false,
    }
}

async fn upsert_role_once(
    pool: &PgPool,
    identifier: &str,
    write: &RoleWrite,
    created_by: i64,
) -> Result<(RoleRow, bool)> {
    let mut tx = pool.begin().await?;
    let (draft, was_new) = if identifier.starts_with(ROLE_RESERVED_PREFIX) {
        let row = model::find_by_oid::<model::Role, RoleRow, _>(&mut *tx, identifier)
            .await?
            .ok_or_else(model::Role::not_found)?;
        (RoleDraft::from_row(&row), false)
    } else {
        match model::find_by_foreign_id::<model::Role, RoleRow, _>(&mut *tx, identifier).await? {
            Some(row) => (RoleDraft::from_row(&row), false),
            None => (
                RoleDraft::new(Some(identifier.to_owned()), created_by),
                true,
            ),
        }
    };
    let row = persist_role(&mut tx, draft, write, was_new).await?;
    tx.commit().await?;
    Ok((row, was_new))
}

pub async fn delete_role(pool: &PgPool, view: &super::RoleView) -> Result<()> {
    let mut tx = pool.begin().await?;
    let id = view.row.id;
    let affected = grantee_principals(&mut tx, None, Some(id)).await?;
    sqlx::query("DELETE FROM grants WHERE role_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM principal_roles WHERE role_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM slack_channel_permissions WHERE role_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    model::delete::<model::Role, _>(&mut *tx, id).await?;
    invalidate::invalidate(&mut tx, Scope::Principals(&affected)).await?;
    tx.commit().await?;
    Ok(())
}

// --- role assignments ------------------------------------------------------

pub async fn assign_role(
    pool: &PgPool,
    principal_id: i64,
    role_oid: &str,
) -> Result<(super::RoleView, bool)> {
    let role = super::read::find_role_by_oid(pool, role_oid).await?;
    let mut tx = pool.begin().await?;
    let existing: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM principal_roles WHERE principal_id = $1 AND role_id = $2)",
    )
    .bind(principal_id)
    .bind(role.row.id)
    .fetch_one(&mut *tx)
    .await?;
    if existing {
        tx.commit().await?;
        return Ok((role, false));
    }
    let inserted = sqlx::query(
        "INSERT INTO principal_roles (principal_id, role_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $3) ON CONFLICT (principal_id, role_id) DO NOTHING",
    )
    .bind(principal_id)
    .bind(role.row.id)
    .bind(timestamp::now())
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if inserted {
        invalidate::invalidate(&mut tx, Scope::Principal(principal_id)).await?;
    }
    tx.commit().await?;
    Ok((role, inserted))
}

pub async fn remove_role(pool: &PgPool, principal_id: i64, role_oid: &str) -> Result<()> {
    let role = super::read::find_role_by_oid(pool, role_oid).await?;
    let mut tx = pool.begin().await?;
    let deleted =
        sqlx::query("DELETE FROM principal_roles WHERE principal_id = $1 AND role_id = $2")
            .bind(principal_id)
            .bind(role.row.id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if deleted == 0 {
        return Err(model::PrincipalRole::not_found().into());
    }
    invalidate::invalidate(&mut tx, Scope::Principal(principal_id)).await?;
    tx.commit().await?;
    Ok(())
}

// --- Slack channel permissions --------------------------------------------

async fn owner_permissions(
    conn: &mut PgConnection,
    owner: Grantee,
) -> Result<Vec<super::PermissionRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM {} ORDER BY channel_id, id",
        super::PERMISSION_COLUMNS,
        owner.owner_sql("slack_channel_permissions")
    ))
    .bind(owner.id())
    .fetch_all(&mut *conn)
    .await?)
}

/// `SlackChannelPermission.normalized_permission_rows`: one row per channel,
/// with every flag OR-ed across repeated rows and channels upper-cased.
fn normalized_rows(rows: &[PermissionWrite]) -> Vec<PermissionDraft> {
    let mut by_channel: std::collections::BTreeMap<String, PermissionDraft> =
        std::collections::BTreeMap::new();
    for row in rows {
        let channel_id = row.channel_id();
        let draft = by_channel
            .entry(channel_id.clone())
            .or_insert_with(|| PermissionDraft::new(channel_id));
        for attribute in PERMISSION_ATTRIBUTES {
            draft.set(attribute, row.flag(attribute) == Some(true));
        }
    }
    by_channel.into_values().collect()
}

/// One channel's permission flags, normalized and ready to validate and write.
///
/// The record this becomes is validated after normalization, so the field rules
/// are attributes on this draft rather than on the request row.
#[derive(Clone, Debug, garde::Validate)]
#[garde(allow_unvalidated)]
#[garde(custom(at_least_one_permission))]
struct PermissionDraft {
    /// Already trimmed and upper-cased, before validation.
    #[garde(custom(super::rules::channel_id))]
    channel_id: String,
    upload_enabled: bool,
    download_enabled: bool,
    history_enabled: bool,
}

impl PermissionDraft {
    fn new(channel_id: String) -> Self {
        Self {
            channel_id,
            upload_enabled: false,
            download_enabled: false,
            history_enabled: false,
        }
    }

    fn from_row(row: &super::PermissionRow) -> Self {
        Self {
            channel_id: row.channel_id.clone(),
            upload_enabled: row.upload_enabled,
            download_enabled: row.download_enabled,
            history_enabled: row.history_enabled,
        }
    }

    fn set(&mut self, attribute: &str, enabled: bool) {
        if !enabled {
            return;
        }
        match attribute {
            "upload_enabled" => self.upload_enabled = true,
            "download_enabled" => self.download_enabled = true,
            "history_enabled" => self.history_enabled = true,
            _ => {}
        }
    }

    fn any_enabled(&self) -> bool {
        self.upload_enabled || self.download_enabled || self.history_enabled
    }

    fn payload(&self) -> PermissionPayload {
        PermissionPayload {
            channel_id: self.channel_id.clone(),
            upload_enabled: self.upload_enabled,
            download_enabled: self.download_enabled,
            history_enabled: self.history_enabled,
        }
    }
}

/// At least one of the permission flags is enabled.
fn at_least_one_permission(row: &PermissionDraft, _: &()) -> garde::Result {
    if row.any_enabled() {
        return Ok(());
    }
    Err(garde::Error::new("Select at least one Slack permission"))
}

fn validate_permission_row(row: &PermissionDraft) -> std::result::Result<(), crate::FieldErrors> {
    row.validate_fields()
}

async fn replace_permissions_in_tx(
    conn: &mut PgConnection,
    owner: Grantee,
    rows: &[PermissionWrite],
    keep_direct_messages: bool,
) -> Result<Vec<PermissionPayload>> {
    let mut rows = normalized_rows(rows);

    if keep_direct_messages {
        // Direct-message rows are API-managed: keep them as stored and ignore
        // any submitted D row.
        let existing = owner_permissions(&mut *conn, owner).await?;
        rows.retain(|row| !row.channel_id.starts_with('D'));
        rows.extend(
            existing
                .iter()
                .filter(|permission| permission.channel_id.starts_with('D'))
                .map(PermissionDraft::from_row),
        );
        rows.sort_by(|left, right| left.channel_id.cmp(&right.channel_id));
    }

    for row in &rows {
        validate_permission_row(row)?;
    }

    let current = owner_permissions(&mut *conn, owner).await?;
    let desired: Vec<PermissionPayload> = rows.iter().map(PermissionDraft::payload).collect();
    let current_payload: Vec<PermissionPayload> =
        current.iter().map(PermissionPayload::from).collect();
    let unchanged = keep_direct_messages && current_payload == desired;
    if unchanged {
        return Ok(current_payload);
    }

    let affected = grantee_principals(&mut *conn, owner.principal_id(), owner.role_id()).await?;
    sqlx::query(&format!(
        "DELETE FROM {}",
        owner.owner_sql("slack_channel_permissions")
    ))
    .bind(owner.id())
    .execute(&mut *conn)
    .await?;
    let sql = write::insert_sql(
        model::SlackChannelPermission::TABLE,
        &[
            "principal_id",
            "role_id",
            "channel_id",
            "upload_enabled",
            "download_enabled",
            "history_enabled",
        ],
    );
    for row in &rows {
        sqlx::query(&sql)
            .bind(owner.principal_id())
            .bind(owner.role_id())
            .bind(&row.channel_id)
            .bind(row.upload_enabled)
            .bind(row.download_enabled)
            .bind(row.history_enabled)
            .execute(&mut *conn)
            .await?;
    }
    invalidate::invalidate(&mut *conn, Scope::Principals(&affected)).await?;
    Ok(desired)
}

pub async fn replace_slack_channel_permissions(
    pool: &PgPool,
    owner: Grantee,
    rows: &[PermissionWrite],
) -> Result<Vec<PermissionPayload>> {
    let keep_direct_messages = matches!(owner, Grantee::Principal(_));
    let mut tx = pool.begin().await?;
    let payload = replace_permissions_in_tx(&mut tx, owner, rows, keep_direct_messages).await?;
    tx.commit().await?;
    Ok(payload)
}

async fn owner_permission(
    conn: &mut PgConnection,
    owner: Grantee,
    channel_id: &str,
) -> Result<Option<super::PermissionRow>> {
    let sql = format!(
        "SELECT {} FROM {} AND channel_id = $2",
        super::PERMISSION_COLUMNS,
        owner.owner_sql("slack_channel_permissions")
    );
    Ok(sqlx::query_as(&sql)
        .bind(owner.id())
        .bind(channel_id)
        .fetch_optional(&mut *conn)
        .await?)
}

pub async fn upsert_slack_channel_permission(
    pool: &PgPool,
    owner: Grantee,
    write: &PermissionWrite,
) -> Result<(PermissionPayload, bool)> {
    let channel_id = write.channel_id();
    let mut tx = pool.begin().await?;
    let current = owner_permission(&mut tx, owner, &channel_id).await?;
    let was_new = current.is_none();

    // A new permission defaults every flag on; an update keeps the stored value
    // of any flag the body omitted.
    let keep = |flag: fn(&super::PermissionRow) -> bool| {
        if was_new {
            true
        } else {
            current.as_ref().map(flag).unwrap_or(true)
        }
    };
    let row = PermissionDraft {
        channel_id: channel_id.clone(),
        upload_enabled: write
            .upload_enabled
            .unwrap_or_else(|| keep(|p| p.upload_enabled)),
        download_enabled: write
            .download_enabled
            .unwrap_or_else(|| keep(|p| p.download_enabled)),
        history_enabled: write
            .history_enabled
            .unwrap_or_else(|| keep(|p| p.history_enabled)),
    };

    validate_permission_row(&row)?;

    let changed = was_new
        || current.as_ref().is_none_or(|permission| {
            permission.upload_enabled != row.upload_enabled
                || permission.download_enabled != row.download_enabled
                || permission.history_enabled != row.history_enabled
        });
    let permission: super::PermissionRow = if was_new {
        let sql = write::insert_returning_sql(
            model::SlackChannelPermission::TABLE,
            &[
                "principal_id",
                "role_id",
                "channel_id",
                "upload_enabled",
                "download_enabled",
                "history_enabled",
            ],
            super::PERMISSION_COLUMNS,
        );
        sqlx::query_as(&sql)
            .bind(owner.principal_id())
            .bind(owner.role_id())
            .bind(&row.channel_id)
            .bind(row.upload_enabled)
            .bind(row.download_enabled)
            .bind(row.history_enabled)
            .fetch_one(&mut *tx)
            .await?
    } else {
        let current = current.expect("existing permission");
        let sql = format!(
            "{} RETURNING {}",
            write::update_sql(
                model::SlackChannelPermission::TABLE,
                &["upload_enabled", "download_enabled", "history_enabled"],
            ),
            super::PERMISSION_COLUMNS
        );
        sqlx::query_as(&sql)
            .bind(row.upload_enabled)
            .bind(row.download_enabled)
            .bind(row.history_enabled)
            .bind(current.id)
            .fetch_one(&mut *tx)
            .await?
    };
    if changed {
        let affected = grantee_principals(&mut tx, owner.principal_id(), owner.role_id()).await?;
        invalidate::invalidate(&mut tx, Scope::Principals(&affected)).await?;
    }
    tx.commit().await?;
    Ok((PermissionPayload::from(&permission), was_new))
}

pub async fn destroy_slack_channel_permission(
    pool: &PgPool,
    owner: Grantee,
    channel_id: &str,
) -> Result<()> {
    let channel_id = channel_id.trim().to_uppercase();
    let mut tx = pool.begin().await?;
    let Some(permission) = owner_permission(&mut tx, owner, &channel_id).await? else {
        return Err(model::SlackChannelPermission::not_found().into());
    };
    model::delete::<model::SlackChannelPermission, _>(&mut *tx, permission.id).await?;
    let affected = grantee_principals(&mut tx, owner.principal_id(), owner.role_id()).await?;
    invalidate::invalidate(&mut tx, Scope::Principals(&affected)).await?;
    tx.commit().await?;
    Ok(())
}

/// Kept for symmetry with the console payload helpers.
pub async fn principal_roles_payload(pool: &PgPool, principal_id: i64) -> Result<Vec<RolePayload>> {
    let roles = principal_roles(pool, principal_id).await?;
    let mut values = Vec::with_capacity(roles.len());
    for role in roles {
        if let Some(view) = super::read::load_role_view(pool, role.id).await? {
            values.push(role_payload(&view));
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entries(errors: crate::FieldErrors) -> Vec<String> {
        errors
            .entries()
            .map(|(attribute, _)| attribute.to_owned())
            .collect()
    }

    fn permission(
        channel_id: &str,
        upload: bool,
        download: bool,
        history: bool,
    ) -> PermissionDraft {
        PermissionDraft {
            channel_id: channel_id.to_owned(),
            upload_enabled: upload,
            download_enabled: download,
            history_enabled: history,
        }
    }

    #[test]
    fn a_permission_row_needs_a_valid_channel_and_one_enabled_flag() {
        let errors = validate_permission_row(&permission("", false, false, false)).unwrap_err();
        let mut keys = entries(errors);
        keys.sort();
        assert_eq!(keys, ["base", "channel_id"]);

        assert!(validate_permission_row(&permission("nope", true, false, false)).is_err());

        // One enabled flag is enough.
        assert!(validate_permission_row(&permission("C0DIFFROLE1", false, true, false)).is_ok());
    }

    #[test]
    fn permission_rows_merge_channels_and_or_their_flags() {
        let rows = normalized_rows(&[
            PermissionWrite {
                channel_id: Some(" c0diffone1 ".to_owned()),
                upload_enabled: Some(true),
                download_enabled: None,
                history_enabled: Some(false),
            },
            PermissionWrite {
                channel_id: Some("C0DIFFONE1".to_owned()),
                upload_enabled: Some(false),
                download_enabled: Some(true),
                history_enabled: None,
            },
            PermissionWrite {
                channel_id: None,
                upload_enabled: Some(true),
                download_enabled: None,
                history_enabled: None,
            },
        ]);
        assert_eq!(
            rows.iter()
                .map(|row| (
                    row.channel_id.clone(),
                    row.upload_enabled,
                    row.download_enabled,
                    row.history_enabled
                ))
                .collect::<Vec<_>>(),
            [
                (String::new(), true, false, false),
                ("C0DIFFONE1".to_owned(), true, true, false),
            ]
        );
    }

    fn principal_draft(foreign_id: Option<&str>) -> PrincipalDraft {
        PrincipalDraft::new(foreign_id.map(str::to_owned), 1)
    }

    #[test]
    fn only_a_value_the_write_carried_and_changed_is_revalidated() {
        let mut draft = principal_draft(Some("diff-channel"));
        assert_eq!(draft.revalidated("slack_user_id"), None);

        // A write that carries a new (bad) id is revalidated.
        draft.apply(&PrincipalWrite {
            slack_user_id: Some(Some("bogus".to_owned())),
            ..PrincipalWrite::default()
        });
        assert_eq!(draft.revalidated("slack_user_id"), Some("bogus"));

        // Once the row exists, re-sending the stored value is not a change.
        let mut stored = draft.clone();
        stored.original = Some(super::PrincipalRow {
            id: 1,
            foreign_id: Some("diff-channel".to_owned()),
            name: None,
            kind: "unknown".to_owned(),
            labels: json!({}),
            slack_user_id: Some("bogus".to_owned()),
            slack_channel_id: None,
            slack_team_id: None,
            slack_email: None,
            console_user_id: None,
            sandbox_repo_cache: "all".to_owned(),
            sandbox_observability_enabled: true,
            sandbox_sessions_read_enabled: false,
            sandbox_workflows_read_enabled: false,
            sandbox_workflows_write_enabled: false,
            sync_config_cache_version: 1,
            created_by_id: 1,
            created_at: time::PrimitiveDateTime::MIN,
            updated_at: time::PrimitiveDateTime::MIN,
        });
        assert_eq!(stored.revalidated("slack_user_id"), None);

        // An explicit null clears the column, which `allow_nil: true` allows.
        stored.apply(&PrincipalWrite {
            slack_user_id: Some(None),
            ..PrincipalWrite::default()
        });
        assert_eq!(stored.revalidated("slack_user_id"), None);
    }

    #[test]
    fn a_blank_foreign_id_is_not_a_rule() {
        let draft = principal_draft(Some("   "));
        assert_eq!(draft.non_blank_foreign_id(), None);
        assert_eq!(principal_draft(None).non_blank_foreign_id(), None);
        assert_eq!(
            principal_draft(Some("diff-channel")).non_blank_foreign_id(),
            Some("diff-channel")
        );
    }
}

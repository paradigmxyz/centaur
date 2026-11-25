//! Grants and the effective-grant view for the principals area.

use serde::Serialize;
use sqlx::{PgPool, Row};
use time::PrimitiveDateTime;

use super::{
    GRANT_COLUMNS, GrantRow, GrantWrite, Grantee, PrincipalError, Result, RoleSummaryPayload,
};
use crate::areas::reconciliation;
use crate::areas::secrets::SecretKind;
use crate::model::{self, ConsoleModel};
use crate::{FieldErrors, invalidate};
use invalidate::Scope;

const DEFAULT_DIRECT_PRIORITY: i32 = 100;
const DEFAULT_ROLE_PRIORITY: i32 = 0;

impl SecretKind {
    /// The id this kind's column holds in `grant`, if it is the grantable.
    pub fn id_in(self, grant: &GrantRow) -> Option<i64> {
        match self {
            Self::Static => grant.static_secret_id,
            Self::GcpAuth => grant.gcp_auth_secret_id,
            Self::GcpIdToken => grant.gcp_id_token_secret_id,
            Self::AwsAuth => grant.aws_auth_secret_id,
            Self::OauthToken => grant.oauth_token_secret_id,
            Self::PgDsn => grant.pg_dsn_secret_id,
            Self::Hmac => grant.hmac_secret_id,
            Self::SshKey => grant.ssh_key_secret_id,
        }
    }
}

#[derive(Clone, Debug)]
pub struct GrantableSummary {
    pub kind: SecretKind,
    pub id: i64,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
}

/// The `grantable` object the console grant payload adds.
#[derive(Clone, Debug, Serialize)]
pub struct GrantableSummaryPayload {
    pub kind: &'static str,
    pub kind_label: &'static str,
    pub id: String,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
}

impl GrantableSummary {
    pub fn payload(&self) -> GrantableSummaryPayload {
        GrantableSummaryPayload {
            kind: self.kind.slug(),
            kind_label: self.kind.label(),
            id: crate::oid(self.kind.oid_prefix(), self.id),
            foreign_id: self.foreign_id.clone(),
            name: self.name.clone(),
        }
    }
}

async fn grantable_summary(
    conn: &mut sqlx::PgConnection,
    kind: SecretKind,
    id: i64,
) -> Result<Option<GrantableSummary>> {
    let row: Option<(i64, Option<String>, Option<String>)> = sqlx::query_as(&format!(
        "SELECT id, foreign_id, name FROM {} WHERE id = $1",
        kind.table()
    ))
    .bind(id)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|(id, foreign_id, name)| GrantableSummary {
        kind,
        id,
        foreign_id,
        name,
    }))
}

async fn load_grant_by_oid(pool: &PgPool, oid: &str) -> Result<GrantRow> {
    Ok(model::find_by_oid::<model::Grant, GrantRow, _>(pool, oid)
        .await?
        .ok_or_else(model::Grant::not_found)?)
}

/// A grant payload, without the console `grantable` summary.
#[derive(Clone, Debug, Serialize)]
pub struct GrantPayload {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub static_secret_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gcp_auth_secret_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gcp_id_token_secret_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aws_auth_secret_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_token_secret_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pg_dsn_secret_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hmac_secret_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh_key_secret_id: Option<String>,
    #[serde(with = "crate::timestamp::primitive")]
    pub created_at: PrimitiveDateTime,
    #[serde(with = "crate::timestamp::primitive")]
    pub updated_at: PrimitiveDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grantable: Option<GrantableSummaryPayload>,
}

pub fn grant_payload(row: &GrantRow) -> Result<GrantPayload> {
    let (kind, secret_id) = row
        .grantable()
        .ok_or_else(|| PrincipalError::Internal("grant has no grantable".to_owned()))?;
    let secret = Some(crate::oid(kind.oid_prefix(), secret_id));
    let mut payload = GrantPayload {
        id: model::Grant::oid(row.id),
        principal_id: row.principal_id.map(model::Principal::oid),
        role_id: row.role_id.map(model::Role::oid),
        static_secret_id: None,
        gcp_auth_secret_id: None,
        gcp_id_token_secret_id: None,
        aws_auth_secret_id: None,
        oauth_token_secret_id: None,
        pg_dsn_secret_id: None,
        hmac_secret_id: None,
        ssh_key_secret_id: None,
        created_at: row.created_at,
        updated_at: row.updated_at,
        grantable: None,
    };
    match kind {
        SecretKind::Static => payload.static_secret_id = secret,
        SecretKind::GcpAuth => payload.gcp_auth_secret_id = secret,
        SecretKind::GcpIdToken => payload.gcp_id_token_secret_id = secret,
        SecretKind::AwsAuth => payload.aws_auth_secret_id = secret,
        SecretKind::OauthToken => payload.oauth_token_secret_id = secret,
        SecretKind::PgDsn => payload.pg_dsn_secret_id = secret,
        SecretKind::Hmac => payload.hmac_secret_id = secret,
        SecretKind::SshKey => payload.ssh_key_secret_id = secret,
    }
    Ok(payload)
}

/// The console grant payload: the base payload plus `grantable`.
pub async fn console_grant_payload(pool: &PgPool, row: &GrantRow) -> Result<GrantPayload> {
    let mut payload = grant_payload(row)?;
    let (kind, secret_id) = row
        .grantable()
        .ok_or_else(|| PrincipalError::Internal("grant has no grantable".to_owned()))?;
    let mut conn = pool.acquire().await?;
    if let Some(summary) = grantable_summary(&mut conn, kind, secret_id).await? {
        payload.grantable = Some(summary.payload());
    }
    Ok(payload)
}

pub async fn show_grant(pool: &PgPool, oid: &str) -> Result<GrantPayload> {
    let row = load_grant_by_oid(pool, oid).await?;
    grant_payload(&row)
}

/// The console grant payload, addressed by oid.
pub async fn console_show_grant(pool: &PgPool, oid: &str) -> Result<GrantPayload> {
    let row = load_grant_by_oid(pool, oid).await?;
    console_grant_payload(pool, &row).await
}

pub async fn destroy_grant(pool: &PgPool, oid: &str) -> Result<()> {
    let row = load_grant_by_oid(pool, oid).await?;
    let mut tx = pool.begin().await?;
    model::delete::<model::Grant, _>(&mut *tx, row.id).await?;
    invalidate::invalidate(
        &mut tx,
        Scope::Grantee {
            principal_id: row.principal_id,
            role_id: row.role_id,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn resolve_one(
    pool: &PgPool,
    write: &GrantWrite,
) -> Result<Option<(SecretKind, GrantableSummary)>> {
    for kind in SecretKind::ALL {
        let Some(value) = write.grantable(kind) else {
            continue;
        };
        let not_found = || PrincipalError::NotFound(format!("{} not found", kind.model_name()));
        let Some(id) = crate::decode_oid(kind.oid_prefix(), value) else {
            return Err(not_found());
        };
        let mut conn = pool.acquire().await?;
        let summary = grantable_summary(&mut conn, kind, id)
            .await?
            .ok_or_else(not_found)?;
        return Ok(Some((kind, summary)));
    }
    Ok(None)
}

async fn resolve_grantee(pool: &PgPool, write: &GrantWrite) -> Result<Option<Grantee>> {
    let Some((column, oid)) = write.grantee() else {
        return Ok(None);
    };
    Ok(Some(if column == "principal_id" {
        Grantee::Principal(super::find_principal_by_oid(pool, oid).await?.row.id)
    } else {
        Grantee::Role(super::find_role_by_oid(pool, oid).await?.row.id)
    }))
}

/// Every field of a grant body is a reference the database has to resolve, so
/// these rules are checked here rather than in garde attributes, and the grantee
/// is resolved before the grantable (so a body with neither reports the grantee
/// alone).
fn missing_reference(names: &[&str]) -> PrincipalError {
    let mut errors = FieldErrors::new();
    errors.add_base(format!("must reference one of {}", names.join(", ")));
    errors.into()
}

pub async fn create_grant(
    pool: &PgPool,
    write: &GrantWrite,
    created_by: i64,
) -> Result<(GrantRow, bool)> {
    const GRANTEE_TYPES: [&str; 2] = ["principal_id", "role_id"];

    let Some(grantee) = resolve_grantee(pool, write).await? else {
        return Err(missing_reference(&GRANTEE_TYPES));
    };
    let Some((kind, summary)) = resolve_one(pool, write).await? else {
        let names: Vec<&str> = SecretKind::ALL
            .iter()
            .map(|kind| kind.grant_column())
            .collect();
        return Err(missing_reference(&names));
    };

    // `Grant#owner_only_grantee`: an owner-only OAuth connection may only be
    // granted to the principal that owns it.
    if kind == SecretKind::Static {
        let credential: Option<(i64, bool)> = sqlx::query_as(
            "SELECT c.id, a.owner_only FROM static_secrets s \
             JOIN broker_credentials c ON c.id = s.broker_credential_id \
             JOIN oauth_apps a ON a.id = c.oauth_app_id WHERE s.id = $1",
        )
        .bind(summary.id)
        .fetch_optional(pool)
        .await?;
        if let Some((credential_id, true)) = credential {
            let owner = match grantee {
                Grantee::Principal(id) => {
                    let mut conn = pool.acquire().await?;
                    reconciliation::connection_owner(&mut conn, id, credential_id).await?
                }
                Grantee::Role(_) => false,
            };
            if !owner {
                let mut errors = FieldErrors::new();
                errors.add(
                    "principal",
                    "must be the owner of this owner-only connection",
                );
                return Err(errors.into());
            }
        }
    }

    let (column, id) = (grantee.column(), grantee.id());
    let secret_column = kind.grant_column();
    let priority = match grantee {
        Grantee::Principal(_) => DEFAULT_DIRECT_PRIORITY,
        Grantee::Role(_) => DEFAULT_ROLE_PRIORITY,
    };
    let existing: Option<GrantRow> = sqlx::query_as(&format!(
        "SELECT {GRANT_COLUMNS} FROM grants WHERE {column} = $1 AND {secret_column} = $2"
    ))
    .bind(id)
    .bind(summary.id)
    .fetch_optional(pool)
    .await?;
    if let Some(row) = existing {
        return Ok((row, false));
    }

    let sql = crate::write::insert_returning_sql(
        model::Grant::TABLE,
        &[column, secret_column, "priority", "created_by_id"],
        GRANT_COLUMNS,
    );
    let insert: std::result::Result<GrantRow, sqlx::Error> = sqlx::query_as(&sql)
        .bind(id)
        .bind(summary.id)
        .bind(priority)
        .bind(created_by)
        .fetch_one(pool)
        .await;
    let row = match insert {
        Ok(row) => row,
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            let row: GrantRow = sqlx::query_as(&format!(
                "SELECT {GRANT_COLUMNS} FROM grants WHERE {column} = $1 AND {secret_column} = $2"
            ))
            .bind(id)
            .bind(summary.id)
            .fetch_one(pool)
            .await?;
            return Ok((row, false));
        }
        Err(error) => return Err(error.into()),
    };
    let mut conn = pool.acquire().await?;
    invalidate::invalidate(
        &mut conn,
        Scope::Grantee {
            principal_id: row.principal_id,
            role_id: row.role_id,
        },
    )
    .await?;
    Ok((row, true))
}

/// The grant rows of one grantee, in insertion order.
pub async fn list_grants(pool: &PgPool, grantee: Grantee) -> Result<Vec<GrantRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {GRANT_COLUMNS} FROM grants WHERE {} = $1 ORDER BY created_at ASC, id ASC",
        grantee.column()
    ))
    .bind(grantee.id())
    .fetch_all(pool)
    .await?)
}

fn effective_condition() -> &'static str {
    "g.principal_id = $1 OR g.role_id IN (SELECT role_id FROM principal_roles WHERE principal_id = $1)"
}

/// One count per kind, in [`SecretKind::ALL`] order.
async fn effective_counts(pool: &PgPool, principal_id: i64) -> Result<Vec<i64>> {
    let selects: Vec<String> = SecretKind::ALL
        .iter()
        .map(|kind| {
            let column = kind.grant_column();
            format!("COUNT(DISTINCT g.{column}) AS {column}")
        })
        .collect();
    let row = sqlx::query(&format!(
        "SELECT {} FROM grants g WHERE {}",
        selects.join(", "),
        effective_condition()
    ))
    .bind(principal_id)
    .fetch_one(pool)
    .await?;
    Ok(SecretKind::ALL
        .iter()
        .map(|kind| row.get(kind.grant_column()))
        .collect())
}

async fn page_records(
    pool: &PgPool,
    principal_id: i64,
    kind: SecretKind,
    offset: i64,
    limit: i64,
) -> Result<Vec<(i64, Option<String>, Option<String>)>> {
    let column = kind.grant_column();
    Ok(sqlx::query_as(&format!(
        "SELECT s.id, s.foreign_id, s.name FROM {} s \
         INNER JOIN (SELECT g.{column} AS secret_id, MAX(g.priority) AS effective_priority \
                     FROM grants g WHERE {condition} AND g.{column} IS NOT NULL \
                     GROUP BY g.{column}) gp ON gp.secret_id = s.id \
         ORDER BY gp.effective_priority ASC, s.id ASC LIMIT $2 OFFSET $3",
        kind.table(),
        condition = effective_condition()
    ))
    .bind(principal_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?)
}

async fn sources(
    pool: &PgPool,
    principal_id: i64,
    kind: SecretKind,
    secret_ids: &[i64],
) -> Result<Vec<GrantRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {GRANT_COLUMNS} FROM grants g WHERE ({}) AND g.{} = ANY($2) ORDER BY g.id",
        effective_condition(),
        kind.grant_column()
    ))
    .bind(principal_id)
    .bind(secret_ids)
    .fetch_all(pool)
    .await?)
}

async fn role_summaries(pool: &PgPool, ids: &[i64]) -> Result<Vec<RoleSummaryPayload>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<(i64, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT id, foreign_id, name FROM roles WHERE id = ANY($1) ORDER BY id")
            .bind(ids)
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(id, foreign_id, name)| RoleSummaryPayload {
            id: model::Role::oid(id),
            foreign_id,
            name,
        })
        .collect())
}

/// One row of the principal's effective grants page.
#[derive(Clone, Debug, Serialize)]
pub struct EffectiveGrantPayload {
    pub id: String,
    pub grant_key: &'static str,
    pub kind: &'static str,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
    pub direct: bool,
    pub roles: Vec<RoleSummaryPayload>,
}

/// The effective grants page for a principal.
///
/// Returns `(data, page, total)`; the page is clamped to the last one.
pub async fn effective_grants(
    pool: &PgPool,
    principal_id: i64,
    page: i64,
    limit: i64,
) -> Result<(Vec<EffectiveGrantPayload>, i64, i64)> {
    let counts = effective_counts(pool, principal_id).await?;
    let total: i64 = counts.iter().sum();
    let total_pages = (((total as f64) / (limit as f64)).ceil() as i64).max(1);
    let page = page.clamp(1, total_pages);
    let mut offset = (page - 1).saturating_mul(limit);

    let mut data = Vec::new();
    for (index, kind) in SecretKind::ALL.into_iter().enumerate() {
        let count = counts[index];
        if offset >= count {
            offset -= count;
            continue;
        }
        let take = limit.min(count - offset);
        // `remaining` is shared across kinds; the loop breaks once the page is
        // full, so `take` is bounded by the page size as well.
        let records = page_records(pool, principal_id, kind, offset, take).await?;
        if !records.is_empty() {
            let ids: Vec<i64> = records.iter().map(|(id, _, _)| *id).collect();
            let grants = sources(pool, principal_id, kind, &ids).await?;
            for (secret_id, foreign_id, name) in records {
                let mut direct = false;
                let mut role_ids: Vec<i64> = Vec::new();
                for grant in grants
                    .iter()
                    .filter(|grant| grant.grantable() == Some((kind, secret_id)))
                {
                    if grant.principal_id.is_some() {
                        direct = true;
                    }
                    if let Some(role_id) = grant.role_id
                        && !role_ids.contains(&role_id)
                    {
                        role_ids.push(role_id);
                    }
                }
                data.push(EffectiveGrantPayload {
                    id: crate::oid(kind.oid_prefix(), secret_id),
                    grant_key: kind.grant_column(),
                    kind: kind.slug(),
                    foreign_id,
                    name,
                    direct,
                    roles: role_summaries(pool, &role_ids).await?,
                });
            }
        }
        offset = 0;
        if data.len() as i64 >= limit {
            break;
        }
    }
    Ok((data, page, total))
}

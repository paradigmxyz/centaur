//! Reads Console's database directly in place of the equivalent admin API
//! calls. Console remains the only writer.

use std::collections::BTreeMap;

use centaur_console_db::{decode_oid, find_principal, find_principal_by_foreign_id};
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::error::{IronControlError, Result};
use crate::models::{EffectiveConfig, Principal, PrincipalInput, SlackChannelPermissionInput};

const PRINCIPAL_OID_PREFIX: &str = "prn";
/// Console stores the principal's repo-cache access as this label too.
const SANDBOX_REPO_CACHE_LABEL: &str = "centaur.sandbox_repo_cache";
const SLACK_DM_KIND: &str = "slack_dm";

/// The principal named by an OID (`prn_…`) or `foreign_id`, as the admin
/// API's principal lookup returns it.
pub(crate) async fn principal(pool: &PgPool, ident: &str) -> Result<Principal> {
    principal_record(find(pool, ident).await?.ok_or_else(|| not_found(ident))?)
}

/// The principal's effective config, as the admin API's `effective_config`
/// returns it for api-rs.
pub(crate) async fn effective_config(pool: &PgPool, ident: &str) -> Result<EffectiveConfig> {
    let row = find(pool, ident).await?.ok_or_else(|| not_found(ident))?;
    let config = centaur_console_db::effective_config(pool, row.id)
        .await?
        .ok_or_else(|| not_found(ident))?;
    serde_json::from_value(json!({ "secrets": config.secrets, "postgres": config.postgres }))
        .map_err(|error| invalid_data(format!("effective config for {ident}: {error}")))
}

/// The stored principal when upserting `input` would change nothing Console
/// stores for it, otherwise `None`.
pub(crate) async fn unchanged_principal(
    pool: &PgPool,
    input: &PrincipalInput,
) -> Result<Option<Principal>> {
    let Some(row) = find_principal_by_foreign_id(pool, &input.foreign_id).await? else {
        return Ok(None);
    };
    let linked_user = match slack_dm_email(&row, input) {
        Some(email) => Some(centaur_console_db::find_user_id_by_email(pool, &email).await?),
        None => None,
    };
    if !upsert_changes_nothing(&row, input, linked_user) {
        return Ok(None);
    }
    principal_record(row).map(Some)
}

/// The normalized email Console links a `slack_dm` principal's user by when
/// `input` is upserted, if it does.
fn slack_dm_email(row: &centaur_console_db::Principal, input: &PrincipalInput) -> Option<String> {
    let email = input
        .slack_email
        .as_deref()
        .filter(|email| !email.trim().is_empty())?;
    (input.kind.as_deref().unwrap_or(&row.kind) == SLACK_DM_KIND)
        .then(|| email.trim().to_lowercase())
}

/// Whether Console's principal upsert of `input` leaves `row` as it is:
/// absent fields are kept, labels are replaced, and `linked_user` is
/// `Some(user)` when the upsert links the principal to Console user `user`.
fn upsert_changes_nothing(
    row: &centaur_console_db::Principal,
    input: &PrincipalInput,
    linked_user: Option<Option<i64>>,
) -> bool {
    if row.name.as_deref() != Some(input.name.as_str()) {
        return false;
    }
    if !input.labels.is_empty() {
        let mut labels = input.labels.clone();
        labels.insert(
            SANDBOX_REPO_CACHE_LABEL.to_owned(),
            row.sandbox_repo_cache.clone(),
        );
        if row.labels != json!(labels) {
            return false;
        }
    }
    let fields = [
        (&input.kind, Some(&row.kind)),
        (&input.slack_user_id, row.slack_user_id.as_ref()),
        (&input.slack_channel_id, row.slack_channel_id.as_ref()),
        (&input.slack_team_id, row.slack_team_id.as_ref()),
        (&input.slack_email, row.slack_email.as_ref()),
    ];
    if fields
        .iter()
        .any(|(wanted, stored)| wanted.is_some() && wanted.as_ref() != *stored)
    {
        return false;
    }
    linked_user.is_none_or(|user| user == row.console_user_id)
}

/// Whether the principal already holds exactly `input` as a direct Slack
/// channel permission.
pub(crate) async fn slack_channel_permission_is_current(
    pool: &PgPool,
    principal: &str,
    input: &SlackChannelPermissionInput,
) -> Result<bool> {
    let Some(id) = decode_oid(PRINCIPAL_OID_PREFIX, principal) else {
        return Ok(false);
    };
    let channel_id = input.channel_id.trim().to_uppercase();
    let current =
        centaur_console_db::find_direct_slack_channel_permission(pool, id, &channel_id).await?;
    Ok(current.is_some_and(|current| {
        current.upload_enabled == input.upload_enabled
            && current.download_enabled == input.download_enabled
            && current.history_enabled == input.history_enabled
    }))
}

/// Resolves an identifier the way the admin API routes it: anything with the
/// OID prefix is an OID, everything else a `foreign_id`.
async fn find(pool: &PgPool, ident: &str) -> Result<Option<centaur_console_db::Principal>> {
    if ident.starts_with("prn_") {
        return match decode_oid(PRINCIPAL_OID_PREFIX, ident) {
            Some(id) => Ok(find_principal(pool, id).await?),
            None => Ok(None),
        };
    }
    Ok(find_principal_by_foreign_id(pool, ident).await?)
}

fn principal_record(row: centaur_console_db::Principal) -> Result<Principal> {
    let Value::Object(stored) = row.labels else {
        return Err(invalid_data(format!(
            "principal {} labels are not an object",
            row.id
        )));
    };
    let mut labels = BTreeMap::new();
    for (key, value) in stored {
        let Value::String(value) = value else {
            return Err(invalid_data(format!(
                "principal {} label {key:?} is not a string",
                row.id
            )));
        };
        labels.insert(key, value);
    }
    labels.insert(SANDBOX_REPO_CACHE_LABEL.to_owned(), row.sandbox_repo_cache);
    Ok(Principal {
        id: centaur_console_db::oid(PRINCIPAL_OID_PREFIX, row.id),
        foreign_id: row.foreign_id,
        name: row.name.unwrap_or_default(),
        labels,
        sandbox_observability_enabled: row.sandbox_observability_enabled,
    })
}

fn not_found(ident: &str) -> IronControlError {
    IronControlError::NotFound {
        resource: "principal",
        ident: ident.to_owned(),
    }
}

fn invalid_data(message: String) -> IronControlError {
    IronControlError::ConsoleDb(centaur_console_db::Error::InvalidData(message))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{principal_record, slack_dm_email, upsert_changes_nothing};
    use crate::models::PrincipalInput;

    fn row() -> centaur_console_db::Principal {
        centaur_console_db::Principal {
            id: 123,
            foreign_id: Some("slack-dm-u1".to_owned()),
            name: Some("Ada".to_owned()),
            kind: "slack_dm".to_owned(),
            labels: json!({ "team": "core", "centaur.sandbox_repo_cache": "public" }),
            slack_user_id: Some("U12345678".to_owned()),
            slack_channel_id: None,
            slack_team_id: Some("T12345678".to_owned()),
            slack_email: Some("ada@example.com".to_owned()),
            console_user_id: Some(7),
            sandbox_repo_cache: "public".to_owned(),
            sandbox_observability_enabled: false,
        }
    }

    fn input() -> PrincipalInput {
        PrincipalInput {
            foreign_id: "slack-dm-u1".to_owned(),
            name: "Ada".to_owned(),
            labels: BTreeMap::from([("team".to_owned(), "core".to_owned())]),
            kind: Some("slack_dm".to_owned()),
            slack_user_id: Some("U12345678".to_owned()),
            slack_channel_id: None,
            slack_team_id: Some("T12345678".to_owned()),
            slack_email: Some("ada@example.com".to_owned()),
        }
    }

    #[test]
    fn a_stored_principal_matching_the_input_needs_no_upsert() {
        assert!(upsert_changes_nothing(&row(), &input(), Some(Some(7))));
    }

    #[test]
    fn fields_the_input_omits_are_left_alone() {
        let input = PrincipalInput {
            labels: BTreeMap::new(),
            kind: None,
            slack_user_id: None,
            slack_team_id: None,
            slack_email: None,
            ..input()
        };
        assert!(upsert_changes_nothing(&row(), &input, None));
    }

    #[test]
    fn any_changed_attribute_needs_an_upsert() {
        let changed = [
            PrincipalInput {
                name: "Ada L".to_owned(),
                ..input()
            },
            PrincipalInput {
                labels: BTreeMap::from([("team".to_owned(), "infra".to_owned())]),
                ..input()
            },
            PrincipalInput {
                labels: BTreeMap::from([
                    ("team".to_owned(), "core".to_owned()),
                    ("extra".to_owned(), "1".to_owned()),
                ]),
                ..input()
            },
            PrincipalInput {
                slack_team_id: Some("T87654321".to_owned()),
                ..input()
            },
            PrincipalInput {
                slack_channel_id: Some("C12345678".to_owned()),
                ..input()
            },
        ];
        for input in changed {
            assert!(
                !upsert_changes_nothing(&row(), &input, Some(Some(7))),
                "{input:?}"
            );
        }
    }

    #[test]
    fn a_slack_dm_email_that_links_another_user_needs_an_upsert() {
        assert!(!upsert_changes_nothing(&row(), &input(), Some(Some(8))));
        assert!(!upsert_changes_nothing(&row(), &input(), Some(None)));
    }

    #[test]
    fn only_slack_dm_emails_link_users() {
        let mut input = input();
        input.slack_email = Some(" Ada@Example.com ".to_owned());
        assert_eq!(
            slack_dm_email(&row(), &input).as_deref(),
            Some("ada@example.com")
        );
        input.kind = Some("user".to_owned());
        assert_eq!(slack_dm_email(&row(), &input), None);
    }

    #[test]
    fn blank_slack_dm_emails_link_no_user() {
        let mut input = input();
        input.slack_email = Some("  ".to_owned());
        assert_eq!(slack_dm_email(&row(), &input), None);
    }

    #[test]
    fn records_carry_the_oid_and_repo_cache_label() {
        let principal = principal_record(row()).unwrap();
        assert_eq!(principal.id, "prn_yRoWctYw");
        assert_eq!(principal.name, "Ada");
        assert_eq!(
            principal
                .labels
                .get("centaur.sandbox_repo_cache")
                .map(String::as_str),
            Some("public")
        );
        assert!(!principal.sandbox_observability_enabled);
    }
}

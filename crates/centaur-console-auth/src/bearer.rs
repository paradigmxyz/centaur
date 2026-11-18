//! Bearer API keys (`/api/v1/*` outside the console and sandbox
//! namespaces): an `iak_` key looked up by SHA-256 hash, whose owner must be
//! an active admin.

use http::HeaderMap;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use super::{AuthUser, Denial};

/// The token in `Authorization: Bearer <token>`, unless blank.
pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(http::header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?.trim();
    (!token.is_empty()).then_some(token)
}

/// Whether the request carries a non-blank `Authorization` header.
pub fn authorization_present(headers: &HeaderMap) -> bool {
    headers
        .get(http::header::AUTHORIZATION)
        .is_some_and(|value| !value.as_bytes().trim_ascii().is_empty())
}

/// Lowercase hex SHA-256 (how `api_keys.token_hash` is stored).
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// An authenticated bearer caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BearerAuth {
    pub api_key_id: i64,
    pub user: AuthUser,
}

/// Authenticates a bearer API request: unknown or deleted key, or an
/// inactive owner, is 401; an active non-admin owner is 403.
pub async fn authenticate(
    pool: &PgPool,
    headers: &HeaderMap,
) -> Result<Result<BearerAuth, Denial>, sqlx::Error> {
    let Some(token) = bearer_token(headers) else {
        return Ok(Err(Denial::INVALID_API_KEY));
    };
    let key: Option<(i64, i64)> = sqlx::query_as(
        "SELECT id, user_id FROM api_keys WHERE deleted_at IS NULL AND token_hash = $1",
    )
    .bind(hash_token(token))
    .fetch_optional(pool)
    .await?;
    let Some((api_key_id, user_id)) = key else {
        return Ok(Err(Denial::INVALID_API_KEY));
    };
    let Some(user) = AuthUser::find(pool, user_id)
        .await?
        .filter(AuthUser::is_active)
    else {
        return Ok(Err(Denial::INVALID_API_KEY));
    };
    if !user.admin {
        return Ok(Err(Denial::API_KEY_NOT_ADMIN));
    }
    Ok(Ok(BearerAuth { api_key_id, user }))
}

#[cfg(test)]
mod tests {
    use http::{HeaderValue, header::AUTHORIZATION};

    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn extracts_bearer_tokens() {
        assert_eq!(bearer_token(&headers("Bearer iak_x")), Some("iak_x"));
        assert_eq!(bearer_token(&headers("Bearer   iak_x ")), Some("iak_x"));
        for value in [
            "Bearer ",
            "Bearer    ",
            "bearer iak_x",
            "Basic Zm9v",
            "Bearer",
        ] {
            assert_eq!(bearer_token(&headers(value)), None, "{value:?}");
        }
        assert_eq!(bearer_token(&HeaderMap::new()), None);
        assert!(authorization_present(&headers("Basic x")));
        assert!(!authorization_present(&headers("   ")));
        assert!(!authorization_present(&HeaderMap::new()));
    }
}

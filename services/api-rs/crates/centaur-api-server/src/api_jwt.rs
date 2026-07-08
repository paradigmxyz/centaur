use std::{env, sync::OnceLock};

use axum::http::{HeaderMap, header};
use base64::{Engine as _, engine::general_purpose};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::ApiError;

const DEFAULT_API_JWT_AUDIENCE: &str = "centaur-api";
const DEFAULT_API_JWT_ISSUER: &str = "centaur-console";
const JWT_CLOCK_SKEW_SECONDS: i64 = 30;

pub(crate) fn bearer_token(headers: &HeaderMap) -> Result<&str, ApiError> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::Unauthorized("missing bearer token".to_owned()))?;
    value
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))
        .map(|(_, token)| token.trim())
        .filter(|token| !token.is_empty())
        .ok_or_else(|| ApiError::Unauthorized("missing bearer token".to_owned()))
}

pub(crate) fn bearer_jwt_from_headers(headers: &HeaderMap) -> Option<&str> {
    let token = bearer_token(headers).ok()?;
    if token.matches('.').count() == 2 {
        Some(token)
    } else {
        None
    }
}

pub(crate) fn decode_jwt_payload(token: &str) -> Result<Value, String> {
    let mut parts = token.split('.');
    let _header = parts.next();
    let payload = parts
        .next()
        .ok_or_else(|| "JWT payload is missing".to_owned())?;
    if parts.next().is_none() || parts.next().is_some() {
        return Err("JWT must have three segments".to_owned());
    }
    let decoded = general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| general_purpose::URL_SAFE.decode(payload))
        .map_err(|_| "JWT payload is not valid base64url".to_owned())?;
    serde_json::from_slice(&decoded).map_err(|_| "JWT payload is not valid JSON".to_owned())
}

pub(crate) fn verify_console_jwt<T>(token: &str) -> Result<T, ApiError>
where
    T: DeserializeOwned,
{
    let secret = jwt_signing_secret().ok_or_else(|| {
        ApiError::Internal("CENTAUR_JWT_SIGNING_SECRET is not configured".to_owned())
    })?;
    let audience = non_empty_env("CENTAUR_API_JWT_AUDIENCE")
        .unwrap_or_else(|| DEFAULT_API_JWT_AUDIENCE.to_owned());
    let issuer = non_empty_env("CENTAUR_API_JWT_ISSUER")
        .unwrap_or_else(|| DEFAULT_API_JWT_ISSUER.to_owned());
    verify_hs256_jwt(token, secret.as_bytes(), &audience, &issuer)
}

pub(crate) fn verify_hs256_jwt<T>(
    token: &str,
    secret: &[u8],
    expected_audience: &str,
    expected_issuer: &str,
) -> Result<T, ApiError>
where
    T: DeserializeOwned,
{
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = JWT_CLOCK_SKEW_SECONDS as u64;
    validation.validate_nbf = true;
    validation.set_audience(&[expected_audience]);
    validation.set_issuer(&[expected_issuer]);
    validation.set_required_spec_claims(&["exp", "iss", "sub", "aud"]);
    let token_data = decode::<T>(token, &DecodingKey::from_secret(secret), &validation)
        .map_err(|_| ApiError::Unauthorized("invalid JWT".to_owned()))?;
    let payload =
        decode_jwt_payload(token).map_err(|_| ApiError::Unauthorized("invalid JWT".to_owned()))?;
    validate_standard_claims(&payload)?;
    Ok(token_data.claims)
}

fn validate_standard_claims(claims: &Value) -> Result<(), ApiError> {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let iat = claims
        .get("iat")
        .and_then(Value::as_i64)
        .ok_or_else(|| ApiError::Unauthorized("JWT issued-at is required".to_owned()))?;
    if iat > now + JWT_CLOCK_SKEW_SECONDS {
        return Err(ApiError::Unauthorized(
            "JWT issued-at is in the future".to_owned(),
        ));
    }
    if claims
        .get("sub")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err(ApiError::Unauthorized("JWT subject is required".to_owned()));
    }
    Ok(())
}

// Deployment JWT configuration is static, so it is resolved once per process.
// Tests mutate env per-case, so cfg!(test) reads live.
fn static_env(cell: &'static OnceLock<Option<String>>, name: &str) -> Option<String> {
    if cfg!(test) {
        return env::var(name).ok();
    }
    cell.get_or_init(|| env::var(name).ok()).clone()
}

pub(crate) fn jwt_signing_secret() -> Option<String> {
    static CELL: OnceLock<Option<String>> = OnceLock::new();
    static_env(&CELL, "CENTAUR_JWT_SIGNING_SECRET")
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;

    fn test_jwt(secret: &[u8], claims: Value) -> String {
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret),
        )
        .unwrap()
    }

    #[test]
    fn bearer_token_scheme_is_case_insensitive() {
        for value in ["Bearer token-1", "bearer token-1", "BEARER token-1"] {
            let mut headers = HeaderMap::new();
            headers.insert(header::AUTHORIZATION, value.parse().unwrap());
            assert_eq!(bearer_token(&headers).unwrap(), "token-1");
        }

        for value in ["Bearer ", "token-1", "Basic token-1"] {
            let mut headers = HeaderMap::new();
            headers.insert(header::AUTHORIZATION, value.parse().unwrap());
            assert!(matches!(
                bearer_token(&headers).unwrap_err(),
                ApiError::Unauthorized(_)
            ));
        }
    }

    #[test]
    fn bearer_jwt_from_headers_requires_jwt_shape() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer not-a-jwt"),
        );
        assert!(bearer_jwt_from_headers(&headers).is_none());

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer header.payload.signature"),
        );
        assert_eq!(
            bearer_jwt_from_headers(&headers),
            Some("header.payload.signature")
        );
    }

    #[test]
    fn verify_console_jwt_rejects_missing_issued_at() {
        let token = test_jwt(
            b"secret",
            json!({
                "iss": "centaur-console",
                "sub": "principal_123",
                "aud": "centaur-api",
                "exp": 4_102_444_800i64,
            }),
        );
        assert!(matches!(
            verify_hs256_jwt::<Value>(&token, b"secret", "centaur-api", "centaur-console")
                .unwrap_err(),
            ApiError::Unauthorized(_)
        ));
    }
}

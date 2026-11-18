//! The api-rs service token on `/internal/*` (RFC 0006a): HS256 with
//! `CENTAUR_JWT_SIGNING_SECRET`, `iss: centaur-api`, `aud: centaur-console`,
//! `sub: centaur-api`, `token_use: api_service`, and an `exp`.

use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;

pub const ISSUER: &str = "centaur-api";
pub const AUDIENCE: &str = "centaur-console";
pub const SUBJECT: &str = "centaur-api";
pub const TOKEN_USE: &str = "api_service";

#[derive(Deserialize)]
struct Claims {
    sub: String,
    token_use: String,
    exp: i64,
}

/// Whether `token` is a valid api-rs service token at `now`.
pub fn verify(token: &str, secret: &str, now: DateTime<Utc>) -> bool {
    if secret.trim().is_empty() {
        return false;
    }
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[AUDIENCE]);
    validation.validate_exp = false;
    validation.required_spec_claims.clear();
    jsonwebtoken::decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .is_ok_and(|data| {
        data.claims.sub == SUBJECT
            && data.claims.token_use == TOKEN_USE
            && data.claims.exp > now.timestamp()
    })
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use serde_json::json;

    use super::*;
    use crate::tokens::{internal_service, sign};

    const SECRET: &str = "internal-test-secret";

    #[test]
    fn accepts_the_minted_service_token_only_while_valid() {
        let now = Utc::now();
        let token = internal_service(SECRET, now).unwrap();
        assert!(verify(&token, SECRET, now));
        assert!(!verify(&token, "other", now));
        assert!(!verify(&token, SECRET, now + Duration::hours(2)));
        assert!(!verify("", SECRET, now));
        assert!(!verify(&token, "", now));
    }

    #[test]
    fn rejects_other_audiences_subjects_and_missing_expiry() {
        let now = Utc::now();
        let exp = (now + Duration::hours(1)).timestamp();
        let good = json!({"iss": ISSUER, "aud": AUDIENCE, "sub": SUBJECT, "token_use": TOKEN_USE, "exp": exp});
        assert!(verify(&sign(SECRET, &good).unwrap(), SECRET, now));
        for (key, value) in [
            ("iss", json!("centaur-console")),
            ("aud", json!("centaur-api")),
            ("sub", json!("someone")),
            ("token_use", json!("console_service")),
        ] {
            let mut claims = good.clone();
            claims[key] = value;
            assert!(
                !verify(&sign(SECRET, &claims).unwrap(), SECRET, now),
                "{key}"
            );
        }
        let mut no_exp = good.clone();
        no_exp.as_object_mut().unwrap().remove("exp");
        assert!(!verify(&sign(SECRET, &no_exp).unwrap(), SECRET, now));
    }
}

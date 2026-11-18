//! HMAC-signed, expiring `state` values for OAuth/SSO redirects: `payload`
//! (JSON, base64url) `.` `signature`, bound to a purpose string so a token
//! minted for one flow is not accepted by another.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::Sha256;

use super::session::derive_key;

/// Signs and verifies state tokens with a key derived from
/// `SECRET_KEY_BASE`. `Debug` hides the key.
#[derive(Clone)]
pub struct StateSigner([u8; 32]);

impl std::fmt::Debug for StateSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StateSigner([REDACTED])")
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    purpose: String,
    expires_at: i64,
    data: T,
}

impl StateSigner {
    pub fn new(secret_key_base: &str) -> Self {
        Self(derive_key(secret_key_base, "state"))
    }

    fn mac(&self, body: &str) -> Hmac<Sha256> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).expect("any key size");
        mac.update(body.as_bytes());
        mac
    }

    pub fn sign<T: Serialize>(
        &self,
        purpose: &str,
        data: &T,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> String {
        let envelope = Envelope {
            purpose: purpose.to_owned(),
            expires_at: (now + ttl).timestamp(),
            data,
        };
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&envelope).expect("state serializes"));
        let signature = URL_SAFE_NO_PAD.encode(self.mac(&body).finalize().into_bytes());
        format!("{body}.{signature}")
    }

    /// The payload, when the signature is valid, the purpose matches, and
    /// the token has not expired.
    pub fn verify<T: DeserializeOwned>(
        &self,
        purpose: &str,
        token: &str,
        now: DateTime<Utc>,
    ) -> Option<T> {
        let (body, signature) = token.split_once('.')?;
        let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
        self.mac(body).verify_slice(&signature).ok()?;
        let envelope: Envelope<T> =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(body).ok()?).ok()?;
        (envelope.purpose == purpose && envelope.expires_at > now.timestamp())
            .then_some(envelope.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_purpose_signature_and_expiry() {
        let signer = StateSigner::new("k");
        let now = Utc::now();
        let token = signer.sign("login", &["google", "n1"], Duration::minutes(10), now);
        assert_eq!(
            signer.verify::<Vec<String>>("login", &token, now),
            Some(vec!["google".into(), "n1".into()])
        );
        assert_eq!(signer.verify::<Vec<String>>("other", &token, now), None);
        assert_eq!(
            signer.verify::<Vec<String>>("login", &token, now + Duration::minutes(11)),
            None
        );
        assert_eq!(
            StateSigner::new("k2").verify::<Vec<String>>("login", &token, now),
            None
        );
        let (body, _) = token.split_once('.').unwrap();
        assert_eq!(
            signer.verify::<Vec<String>>("login", &format!("{body}.AAAA"), now),
            None
        );
        assert_eq!(signer.verify::<Vec<String>>("login", "garbage", now), None);
        assert_eq!(format!("{signer:?}"), "StateSigner([REDACTED])");
    }
}

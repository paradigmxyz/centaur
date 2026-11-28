//! Turn tokens: what a turn may do with Code Mode, signed by api-rs.
//!
//! A turn token is an EdDSA (Ed25519) JWT. api-rs holds the private key and
//! mints one for every turn it submits to the brain, after resolving the
//! session's principal and tool policy. Everything else holds only
//! the public key, so they can verify a token but never mint one: a confused
//! or compromised agent loop cannot act for a principal it was not handed.

use aws_lc_rs::{
    encoding::AsDer,
    signature::{Ed25519KeyPair, KeyPair},
};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const ISSUER: &str = "centaur-api";
pub const AUDIENCE: &str = "centaur-codemode";
/// The audience of the hands token: a distinct credential so the Code Mode
/// turn token cannot authorize hand leases, even though both are forwarded to
/// the brain.
pub const HANDS_AUDIENCE: &str = "centaur-hands";
/// How long a turn token is valid: the longest a turn is expected to run.
/// Steers reuse the running turn's token, which a new turn mints again.
pub const TTL_SECS: i64 = 60 * 60;
/// Clock skew tolerated between api-rs and the verifiers.
const LEEWAY_SECS: u64 = 30;

/// The deployment's tool filter for a principal, in the same shape the Code
/// Mode runtime's policy takes. Entries match connector names exactly.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPolicy {
    /// `None` means every connector not blocklisted.
    #[serde(default)]
    pub allowlist: Option<Vec<String>>,
    #[serde(default)]
    pub blocklist: Option<Vec<String>>,
}

/// What api-rs grants one turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnGrant {
    /// The session's principal (`prn_...`), whose credentials connectors use.
    pub principal: String,
    /// The requesting user's principal on a shared-channel turn, whose
    /// always-available OAuth credentials are layered on top.
    pub requester: Option<String>,
    pub thread_key: String,
    pub execution_id: String,
    pub tools: ToolPolicy,
}

/// The claims of a turn token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnClaims {
    pub iss: String,
    pub aud: String,
    /// The principal.
    pub sub: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester: Option<String>,
    pub thread_key: String,
    pub execution_id: String,
    #[serde(default)]
    pub tools: ToolPolicy,
    pub iat: i64,
    pub exp: i64,
}

impl TurnClaims {
    #[must_use]
    pub fn principal(&self) -> &str {
        &self.sub
    }
}

#[derive(Debug, Error)]
pub enum TokenError {
    #[error("invalid turn token key: {0}")]
    Key(String),
    #[error("turn token signing failed: {0}")]
    Sign(String),
    #[error("invalid turn token: {0}")]
    Invalid(String),
}

/// Mints turn tokens. Only api-rs constructs one.
#[derive(Clone)]
pub struct TurnTokenSigner {
    key: EncodingKey,
    verifier: TurnTokenVerifier,
}

impl std::fmt::Debug for TurnTokenSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TurnTokenSigner(..)")
    }
}

impl TurnTokenSigner {
    /// From a PKCS#8 PEM Ed25519 private key.
    pub fn from_pem(pem: &[u8]) -> Result<Self, TokenError> {
        let key =
            EncodingKey::from_ed_pem(pem).map_err(|error| TokenError::Key(error.to_string()))?;
        let pair = Ed25519KeyPair::from_pkcs8(key.as_bytes())
            .map_err(|error| TokenError::Key(error.to_string()))?;
        let public_der = pair
            .public_key()
            .as_der()
            .map_err(|error| TokenError::Key(error.to_string()))?;
        let verifier = TurnTokenVerifier {
            key: DecodingKey::from_ed_der(public_der.as_ref()),
            audience: AUDIENCE.to_owned(),
        };
        Ok(Self { key, verifier })
    }

    /// A verifier for tokens this signer mints, derived from the private key's
    /// public half. api-rs uses this for its own protected routes so it needs
    /// no second copy of the deployment key pair.
    #[must_use]
    pub fn verifier(&self) -> TurnTokenVerifier {
        self.verifier.clone()
    }

    /// A verifier that accepts only tokens minted for `audience`.
    #[must_use]
    pub fn verifier_for(&self, audience: &str) -> TurnTokenVerifier {
        TurnTokenVerifier {
            key: self.verifier.key.clone(),
            audience: audience.to_owned(),
        }
    }

    /// Mint a Code Mode turn token for `grant`, valid from `now` (unix
    /// seconds) for [`TTL_SECS`].
    pub fn sign(&self, grant: &TurnGrant, now: i64) -> Result<(String, TurnClaims), TokenError> {
        self.sign_for(grant, now, AUDIENCE)
    }

    /// Mint a token for `grant` with an explicit audience. The claims and TTL
    /// are the same as a turn token; only the audience differs.
    pub fn sign_for(
        &self,
        grant: &TurnGrant,
        now: i64,
        audience: &str,
    ) -> Result<(String, TurnClaims), TokenError> {
        let claims = TurnClaims {
            iss: ISSUER.to_owned(),
            aud: audience.to_owned(),
            sub: grant.principal.clone(),
            requester: grant.requester.clone(),
            thread_key: grant.thread_key.clone(),
            execution_id: grant.execution_id.clone(),
            tools: grant.tools.clone(),
            iat: now,
            exp: now + TTL_SECS,
        };
        let token = jsonwebtoken::encode(&Header::new(Algorithm::EdDSA), &claims, &self.key)
            .map_err(|error| TokenError::Sign(error.to_string()))?;
        Ok((token, claims))
    }
}

/// Verifies turn tokens: the brain, the executor and proxy-sync.
#[derive(Clone)]
pub struct TurnTokenVerifier {
    key: DecodingKey,
    audience: String,
}

impl std::fmt::Debug for TurnTokenVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TurnTokenVerifier(..)")
    }
}

impl TurnTokenVerifier {
    /// From an SPKI PEM Ed25519 public key.
    pub fn from_pem(pem: &[u8]) -> Result<Self, TokenError> {
        DecodingKey::from_ed_pem(pem)
            .map(|key| Self {
                key,
                audience: AUDIENCE.to_owned(),
            })
            .map_err(|error| TokenError::Key(error.to_string()))
    }

    /// Verify the signature, issuer, audience and expiry, and return the claims.
    pub fn verify(&self, token: &str) -> Result<TurnClaims, TokenError> {
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_issuer(&[ISSUER]);
        validation.set_audience(&[self.audience.as_str()]);
        validation.set_required_spec_claims(&["iss", "aud", "exp", "sub"]);
        validation.leeway = LEEWAY_SECS;
        let data = jsonwebtoken::decode::<TurnClaims>(token, &self.key, &validation)
            .map_err(|error| TokenError::Invalid(error.to_string()))?;
        if data.claims.sub.trim().is_empty() {
            return Err(TokenError::Invalid(
                "the token names no principal".to_owned(),
            ));
        }
        Ok(data.claims)
    }
}

/// The bearer token in an `Authorization` header value.
#[must_use]
pub fn bearer(value: &str) -> Option<&str> {
    value
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

/// Key pairs for tests.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use aws_lc_rs::{
        encoding::AsDer,
        signature::{Ed25519KeyPair, KeyPair},
    };

    fn pem(label: &str, der: &[u8]) -> String {
        use std::fmt::Write as _;
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut encoded = String::new();
        for chunk in der.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, byte)| n | (u32::from(*byte) << (16 - 8 * i)));
            for i in 0..4 {
                if i <= chunk.len() {
                    encoded.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    encoded.push('=');
                }
            }
        }
        let mut out = format!("-----BEGIN {label}-----\n");
        for line in encoded.as_bytes().chunks(64) {
            let _ = writeln!(out, "{}", std::str::from_utf8(line).expect("ascii"));
        }
        let _ = writeln!(out, "-----END {label}-----");
        out
    }

    /// A fresh (private PEM, public PEM) Ed25519 pair.
    #[must_use]
    pub fn pair() -> (String, String) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&aws_lc_rs::rand::SystemRandom::new())
            .expect("generate key");
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("parse key");
        let spki = pair.public_key().as_der().expect("public key der");
        (
            pem("PRIVATE KEY", pkcs8.as_ref()),
            pem("PUBLIC KEY", spki.as_ref()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant() -> TurnGrant {
        TurnGrant {
            principal: "prn_channel".to_owned(),
            requester: Some("prn_user".to_owned()),
            thread_key: "slack:T1:C1:1.2".to_owned(),
            execution_id: "exe_1".to_owned(),
            tools: ToolPolicy {
                allowlist: Some(vec!["slack".to_owned()]),
                blocklist: None,
            },
        }
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64
    }

    #[test]
    fn signs_and_verifies_round_trip() {
        let (private, public) = testing::pair();
        let signer = TurnTokenSigner::from_pem(private.as_bytes()).expect("signer");
        let verifier = TurnTokenVerifier::from_pem(public.as_bytes()).expect("verifier");
        let (token, claims) = signer.sign(&grant(), now()).expect("sign");
        assert_eq!(verifier.verify(&token).expect("verify"), claims);
        assert_eq!(claims.principal(), "prn_channel");
        assert_eq!(claims.exp - claims.iat, TTL_SECS);
    }

    #[test]
    fn rejects_another_key_an_expired_token_and_tampering() {
        let (private, _) = testing::pair();
        let (_, other_public) = testing::pair();
        let signer = TurnTokenSigner::from_pem(private.as_bytes()).expect("signer");
        let other = TurnTokenVerifier::from_pem(other_public.as_bytes()).expect("verifier");
        let (token, _) = signer.sign(&grant(), now()).expect("sign");
        assert!(other.verify(&token).is_err(), "a token from another key");

        let (private, public) = testing::pair();
        let signer = TurnTokenSigner::from_pem(private.as_bytes()).expect("signer");
        let verifier = TurnTokenVerifier::from_pem(public.as_bytes()).expect("verifier");
        let (expired, _) = signer.sign(&grant(), now() - TTL_SECS - 120).expect("sign");
        assert!(verifier.verify(&expired).is_err(), "an expired token");

        let (token, _) = signer.sign(&grant(), now()).expect("sign");
        let mut parts: Vec<String> = token.split('.').map(str::to_owned).collect();
        parts[1] = parts[1].chars().rev().collect();
        assert!(
            verifier.verify(&parts.join(".")).is_err(),
            "a tampered token"
        );
    }

    #[test]
    fn derives_a_verifier_from_the_signing_key() {
        let (private, _) = testing::pair();
        let signer = TurnTokenSigner::from_pem(private.as_bytes()).expect("signer");
        let (token, claims) = signer.sign(&grant(), now()).expect("sign");
        assert_eq!(signer.verifier().verify(&token).expect("verify"), claims);
    }

    #[test]
    fn hands_tokens_have_a_distinct_audience_and_cannot_use_the_codemode_verifier() {
        let (private, _) = testing::pair();
        let signer = TurnTokenSigner::from_pem(private.as_bytes()).expect("signer");
        let (hands, claims) = signer
            .sign_for(&grant(), now(), HANDS_AUDIENCE)
            .expect("sign hands token");
        assert_eq!(claims.aud, HANDS_AUDIENCE);
        assert_eq!(claims.exp - claims.iat, TTL_SECS);
        assert!(
            signer.verifier().verify(&hands).is_err(),
            "the Code Mode verifier rejects a hands token"
        );
        assert_eq!(
            signer
                .verifier_for(HANDS_AUDIENCE)
                .verify(&hands)
                .expect("hands verifier accepts its token"),
            claims
        );

        let (codemode, _) = signer.sign(&grant(), now()).expect("sign turn token");
        assert!(
            signer
                .verifier_for(HANDS_AUDIENCE)
                .verify(&codemode)
                .is_err(),
            "the hands verifier rejects a Code Mode turn token"
        );
    }

    #[test]
    fn parses_bearer_headers() {
        assert_eq!(bearer("Bearer abc"), Some("abc"));
        assert_eq!(bearer("Bearer "), None);
        assert_eq!(bearer("Basic abc"), None);
    }
}

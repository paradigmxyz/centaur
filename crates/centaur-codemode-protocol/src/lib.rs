//! The contract between the services that run Code Mode for an agent turn.
//!
//! ```text
//! api-rs ── turn token ──▶ centaur-brain ── exec / search / load ──▶ executor
//!                                                                     │ CONNECT, Proxy-Authorization: Bearer <token>
//!                                                                     ▼
//!                                                       tenancy iron-proxy ──▶ upstreams
//! ```
//!
//! api-rs mints one [`token::TurnClaims`] per submitted turn, signed with an
//! Ed25519 key only api-rs holds; it names the principal and the tool policy
//! the turn runs under. The brain forwards it with every Code Mode call. The
//! executor verifies it before running anything and sends it as the
//! `Proxy-Authorization` of every connector tunnel; iron-proxy verifies it
//! too and applies that principal's credentials. Only api-rs can mint one.

pub mod token;
pub mod tools;
pub mod wire;

/// PEM (PKCS#8) Ed25519 private key api-rs signs turn tokens with.
pub const TOKEN_SIGNING_KEY_ENV: &str = "CENTAUR_CODEMODE_TOKEN_SIGNING_KEY";
/// PEM (SPKI) Ed25519 public key turn tokens are verified with.
pub const TOKEN_PUBLIC_KEY_ENV: &str = "CENTAUR_CODEMODE_TOKEN_PUBLIC_KEY";
/// Base URL of the executor, for the brain.
pub const EXECUTOR_URL_ENV: &str = "CENTAUR_CODEMODE_EXECUTOR_URL";

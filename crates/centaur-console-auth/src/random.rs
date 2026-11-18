//! `SecureRandom` equivalents.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

/// `SecureRandom.random_bytes(N)`.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).expect("operating system randomness");
    bytes
}

/// `SecureRandom.hex(16)`: 32 lowercase hex characters.
pub fn hex16() -> String {
    hex::encode(random_bytes::<16>())
}

/// `SecureRandom.hex(32)`: 64 lowercase hex characters.
pub fn hex32() -> String {
    hex::encode(random_bytes::<32>())
}

/// `SecureRandom.urlsafe_base64(32)` (no padding).
pub fn urlsafe_base64_32() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes::<32>())
}

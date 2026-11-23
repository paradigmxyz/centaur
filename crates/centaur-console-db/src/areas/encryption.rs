//! Console's Active Record encryption key, read from the Console service
//! environment.
//!
//! Several Console attributes are Active Record `encrypts` columns: inline
//! secret values (`secret_sources.secret`) and personal SSH private keys. The
//! API server writes them in that format, so stored ciphertext stays readable
//! by the proxy, and it decrypts a stored value only when a write retains or
//! compares one. This is the single place that reads the key material; every
//! area shares it, and missing configuration is an error rather than a silent
//! plaintext write.

use std::sync::OnceLock;

use active_record_encryption::{ActiveRecordEncryption, Error as EncryptionError};

use crate::{Error, Result};

static ENCRYPTION: OnceLock<std::result::Result<ActiveRecordEncryption, String>> = OnceLock::new();

/// The configured key material, from the Console service environment. This is
/// the only place those variables are read.
pub fn keys() -> Result<(String, String)> {
    let primary_key = std::env::var("CENTAUR_CONSOLE_AR_ENCRYPTION_PRIMARY_KEY").ok();
    let salt = std::env::var("CENTAUR_CONSOLE_AR_ENCRYPTION_KEY_DERIVATION_SALT").ok();
    match (primary_key, salt) {
        (Some(primary_key), Some(salt)) if !primary_key.is_empty() && !salt.is_empty() => {
            Ok((primary_key, salt))
        }
        _ => Err(Error::InvalidData(
            "CENTAUR_CONSOLE_AR_ENCRYPTION_PRIMARY_KEY and \
             CENTAUR_CONSOLE_AR_ENCRYPTION_KEY_DERIVATION_SALT must be set to read or \
             write Console secret values"
                .to_owned(),
        )),
    }
}

/// The configured key, built once from `CENTAUR_CONSOLE_AR_ENCRYPTION_*`.
pub fn active_record_encryption() -> Result<&'static ActiveRecordEncryption> {
    ENCRYPTION
        .get_or_init(|| {
            let (primary_key, salt) = keys().map_err(|error| error.to_string())?;
            Ok(ActiveRecordEncryption::new(&primary_key, &salt))
        })
        .as_ref()
        .map_err(|message| Error::InvalidData(message.clone()))
}

pub fn encrypt(plaintext: &str) -> Result<String> {
    Ok(active_record_encryption()?.encrypt(plaintext)?)
}

pub fn decrypt(ciphertext: &str) -> Result<String> {
    active_record_encryption().and_then(|encryption| {
        encryption
            .decrypt(ciphertext)
            .map_err(|error: EncryptionError| Error::Encryption(error))
    })
}

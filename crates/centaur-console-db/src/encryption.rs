//! Active Record encryption for the console's `encrypts` attributes.
//!
//! Rails encrypts `oauth_apps.client_secret`, `secret_sources.secret`, and
//! seven `broker_credentials` columns with non-deterministic AES-256-GCM
//! (see `active-record-encryption`). Table rows hold those columns as
//! [`Ciphertext`]; decrypt through an [`Encryptor`] only where a value is
//! actually needed, and never log or render plaintext.

use std::{fmt, sync::Arc};

use active_record_encryption::ActiveRecordEncryption;
use sqlx::{
    Decode, Encode, Postgres, Type,
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgArgumentBuffer, PgTypeInfo, PgValueRef},
};

use crate::Result;

/// The process's Active Record encryption keys
/// (`CENTAUR_CONSOLE_AR_ENCRYPTION_*`).
#[derive(Clone, Debug)]
pub struct Encryptor(Arc<ActiveRecordEncryption>);

impl Encryptor {
    pub fn new(primary_key: &str, key_derivation_salt: &str) -> Self {
        Self(Arc::new(ActiveRecordEncryption::new(
            primary_key,
            key_derivation_salt,
        )))
    }

    /// The keys Rails falls back to outside production
    /// (`config/initializers/encryption.rb`). Tests and local runs only.
    pub fn development() -> Self {
        Self::new(DEV_PRIMARY_KEY, DEV_KEY_DERIVATION_SALT)
    }

    pub fn encrypt(&self, plaintext: &str) -> Result<Ciphertext> {
        Ok(Ciphertext(self.0.encrypt(plaintext)?))
    }

    pub fn decrypt(&self, ciphertext: &Ciphertext) -> Result<String> {
        Ok(self.0.decrypt(&ciphertext.0)?)
    }

    /// `encrypt` for a nullable attribute: Rails stores `nil` as NULL.
    pub fn encrypt_optional(&self, plaintext: Option<&str>) -> Result<Option<Ciphertext>> {
        plaintext.map(|value| self.encrypt(value)).transpose()
    }

    pub fn decrypt_optional(&self, ciphertext: Option<&Ciphertext>) -> Result<Option<String>> {
        ciphertext.map(|value| self.decrypt(value)).transpose()
    }

    /// The underlying cipher, for code that predates this wrapper
    /// (`SecretValues::Live`).
    pub fn inner(&self) -> &ActiveRecordEncryption {
        &self.0
    }
}

/// Rails' development fallback primary key (not a secret).
pub const DEV_PRIMARY_KEY: &str = "dev_ar_encryption_primary_key_0000000000000000";
/// Rails' development fallback key derivation salt.
pub const DEV_KEY_DERIVATION_SALT: &str = "dev_ar_encryption_key_derivation_salt_000000000";

/// The stored (encrypted) form of an `encrypts` attribute: the JSON envelope
/// Active Record writes. Its `Debug` output never shows the envelope.
#[derive(Clone, PartialEq, Eq)]
pub struct Ciphertext(String);

impl Ciphertext {
    /// Wraps a stored envelope, e.g. one read with a raw query.
    pub fn from_stored(envelope: String) -> Self {
        Self(envelope)
    }

    pub fn as_stored(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Ciphertext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Ciphertext(..)")
    }
}

impl Type<Postgres> for Ciphertext {
    fn type_info() -> PgTypeInfo {
        <String as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <String as Type<Postgres>>::compatible(ty)
    }
}

impl Encode<'_, Postgres> for Ciphertext {
    fn encode_by_ref(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> std::result::Result<IsNull, BoxDynError> {
        <String as Encode<Postgres>>::encode_by_ref(&self.0, buf)
    }
}

impl Decode<'_, Postgres> for Ciphertext {
    fn decode(value: PgValueRef<'_>) -> std::result::Result<Self, BoxDynError> {
        Ok(Self(<String as Decode<Postgres>>::decode(value)?))
    }
}

#[cfg(test)]
mod tests {
    use super::Encryptor;

    #[test]
    fn round_trips_and_hides_ciphertext() {
        let encryptor = Encryptor::development();
        let stored = encryptor.encrypt("known secret").unwrap();
        assert_eq!(format!("{stored:?}"), "Ciphertext(..)");
        assert_eq!(encryptor.decrypt(&stored).unwrap(), "known secret");
        assert_eq!(encryptor.encrypt_optional(None).unwrap(), None);
    }
}

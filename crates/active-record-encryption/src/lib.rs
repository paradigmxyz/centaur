use std::{fmt, io::Read};

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Generate, Payload, consts::U12},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use pbkdf2::pbkdf2_hmac;
use serde::Deserialize;
use sha1::Sha1;
use sha2::Sha256;
use std::io::Write;

#[derive(Debug)]
pub struct Error(&'static str);

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for Error {}

/// Rails compresses plaintexts longer than this many bytes.
const COMPRESSION_THRESHOLD: usize = 140;

pub struct ActiveRecordEncryption {
    keys: [[u8; 32]; 2],
}

#[derive(Deserialize)]
struct EncryptedMessage {
    p: String,
    #[serde(default)]
    h: EncryptedHeaders,
}

#[derive(Default, Deserialize)]
struct EncryptedHeaders {
    iv: String,
    at: String,
    #[serde(default)]
    c: bool,
}

impl fmt::Debug for ActiveRecordEncryption {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ActiveRecordEncryption { .. }")
    }
}

impl ActiveRecordEncryption {
    pub fn new(primary_key: &str, salt: &str) -> Self {
        let mut sha256_key = [0; 32];
        pbkdf2_hmac::<Sha256>(
            primary_key.as_bytes(),
            salt.as_bytes(),
            1 << 16,
            &mut sha256_key,
        );
        let mut sha1_key = [0; 32];
        pbkdf2_hmac::<Sha1>(
            primary_key.as_bytes(),
            salt.as_bytes(),
            1 << 16,
            &mut sha1_key,
        );
        Self {
            keys: [sha256_key, sha1_key],
        }
    }

    /// Encrypts like a non-deterministic Rails `encrypts` attribute
    /// (`ActiveRecord::Encryption::Encryptor` with the default Zlib
    /// compressor): the current (SHA-256-derived) key, a random 12-byte IV,
    /// and Zlib compression with the `"c": true` header when the plaintext is
    /// longer than 140 bytes. The envelope is the JSON Rails'
    /// `MessageSerializer` writes: `{"p":..,"h":{"iv":..,"at":..[,"c":true]}}`.
    pub fn encrypt(&self, plaintext: &str) -> Result<String, Error> {
        let nonce =
            Nonce::<U12>::try_generate().map_err(|_| Error("random IV generation failed"))?;
        self.encrypt_with_iv(plaintext, nonce.into())
    }

    /// [`Self::encrypt`] with a caller-chosen IV. Only for tests that compare
    /// output byte for byte with Rails; never reuse an IV with a real key.
    #[doc(hidden)]
    pub fn encrypt_with_iv(&self, plaintext: &str, iv: [u8; 12]) -> Result<String, Error> {
        // Rails: THRESHOLD_TO_JUSTIFY_COMPRESSION = 140.bytes, `bytesize > 140`.
        let compressed = plaintext.len() > COMPRESSION_THRESHOLD;
        let clear = if compressed {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder
                .write_all(plaintext.as_bytes())
                .and_then(|()| encoder.finish())
                .map_err(|_| Error("attribute compression failed"))?
        } else {
            plaintext.as_bytes().to_vec()
        };
        let nonce = Nonce::<U12>::from(iv);
        let mut ciphertext = Aes256Gcm::new_from_slice(&self.keys[0])
            .map_err(|_| Error("invalid encryption key"))?
            .encrypt(
                &nonce,
                Payload {
                    msg: &clear,
                    aad: b"",
                },
            )
            .map_err(|_| Error("attribute encryption failed"))?;
        let tag = ciphertext.split_off(ciphertext.len() - 16);
        // Rails' key order (payload, then iv, auth tag, compressed). Base64
        // needs no JSON escaping, so the envelope is written directly.
        Ok(format!(
            r#"{{"p":"{}","h":{{"iv":"{}","at":"{}"{}}}}}"#,
            STANDARD.encode(ciphertext),
            STANDARD.encode(nonce),
            STANDARD.encode(tag),
            if compressed { r#","c":true"# } else { "" },
        ))
    }

    pub fn decrypt(&self, encoded: &str) -> Result<String, Error> {
        let message: EncryptedMessage = serde_json::from_str(encoded)
            .map_err(|_| Error("invalid encrypted attribute envelope"))?;
        let ciphertext = STANDARD
            .decode(message.p)
            .map_err(|_| Error("invalid encrypted attribute payload"))?;
        let iv = STANDARD
            .decode(message.h.iv)
            .map_err(|_| Error("invalid encrypted attribute IV"))?;
        let tag = STANDARD
            .decode(message.h.at)
            .map_err(|_| Error("invalid encrypted attribute tag"))?;
        if iv.len() != 12 || tag.len() != 16 {
            return Err(Error("invalid encrypted attribute dimensions"));
        }

        let nonce = <&Nonce<U12>>::try_from(iv.as_slice())
            .map_err(|_| Error("invalid encrypted attribute dimensions"))?;
        let mut ciphertext_and_tag = ciphertext;
        ciphertext_and_tag.extend_from_slice(&tag);
        let mut plaintext = self
            .keys
            .iter()
            .find_map(|key| {
                let cipher = Aes256Gcm::new_from_slice(key).ok()?;
                cipher
                    .decrypt(
                        nonce,
                        Payload {
                            msg: &ciphertext_and_tag,
                            aad: b"",
                        },
                    )
                    .ok()
            })
            .ok_or(Error("encrypted attribute authentication failed"))?;

        if message.h.c {
            let mut decoded = Vec::new();
            ZlibDecoder::new(plaintext.as_slice())
                .read_to_end(&mut decoded)
                .map_err(|_| Error("encrypted attribute decompression failed"))?;
            plaintext = decoded;
        }

        String::from_utf8(plaintext).map_err(|_| Error("encrypted attribute was not UTF-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::ActiveRecordEncryption;

    #[test]
    fn decrypts_current_and_legacy_active_record_envelopes() {
        let encryption = ActiveRecordEncryption::new(
            "dev_ar_encryption_primary_key_0000000000000000",
            "dev_ar_encryption_key_derivation_salt_000000000",
        );
        let ciphertext = r#"{"p":"xOiyro9XYBkfABUF","h":{"iv":"pCjJC5SxH78ZN0Nm","at":"s9DIUyHAkF8ZAthKO6ROtw=="}}"#;
        assert_eq!(encryption.decrypt(ciphertext).unwrap(), "known secret");

        let legacy = r#"{"p":"dSeLBWwnaY1TWU1x","h":{"iv":"SB1xJfmKBwS48kLd","at":"DW6Ug2HRtVMhkRn7VEBbFg=="}}"#;
        assert_eq!(encryption.decrypt(legacy).unwrap(), "known secret");
    }

    #[test]
    fn encrypts_round_trip_values_with_a_fresh_iv() {
        let encryption = ActiveRecordEncryption::new(
            "dev_ar_encryption_primary_key_0000000000000000",
            "dev_ar_encryption_key_derivation_salt_000000000",
        );
        for plaintext in ["", "known secret", &"long token ".repeat(40)] {
            let first = encryption.encrypt(plaintext).unwrap();
            assert_eq!(encryption.decrypt(&first).unwrap(), plaintext);
            assert_ne!(first, encryption.encrypt(plaintext).unwrap());
        }
    }
}

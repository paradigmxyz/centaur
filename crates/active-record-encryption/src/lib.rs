use std::{fmt, io::Read};

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload, consts::U12},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use flate2::read::ZlibDecoder;
use pbkdf2::pbkdf2_hmac;
use serde::Deserialize;
use sha1::Sha1;
use sha2::Sha256;

#[derive(Debug)]
pub struct Error(&'static str);

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for Error {}

#[derive(Debug)]
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
}

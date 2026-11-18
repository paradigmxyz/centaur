//! SSH private keys pasted into the console: validate, then derive the
//! OpenSSH public key and its `SHA256:` fingerprint, on the `ssh-key`
//! crate. Accepts unencrypted Ed25519, RSA, and ECDSA (P-256/384/521) keys
//! in OpenSSH, PKCS#1, SEC1, or PKCS#8 form. Errors are user-facing
//! messages that never include the key or parser output.

use ssh_key::{
    HashAlg, PrivateKey,
    private::{Ed25519Keypair, KeypairData, RsaKeypair},
    public::{EcdsaPublicKey, KeyData},
};

pub const MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("Paste your private key, including its BEGIN and END lines.")]
    Blank,
    #[error("The key is too large. Paste a single private key.")]
    TooLarge,
    #[error("This is a public key. Paste the private key file, without the .pub extension.")]
    Public,
    #[error("Include the complete private key, including its BEGIN and END lines.")]
    Incomplete,
    #[error("This key is protected by a passphrase. Use a dedicated key without a passphrase.")]
    Passphrase,
    #[error("This SSH key is invalid or unsupported. Use an Ed25519, RSA, or ECDSA key.")]
    Invalid,
}

/// What the console stores next to a private key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInfo {
    /// `<type> <base64>` (no comment).
    pub public_key: String,
    /// `SHA256:...`.
    pub fingerprint: String,
}

/// LF line endings, trimmed, one final newline: the form that is stored.
pub fn normalize(private_key: &str) -> String {
    format!(
        "{}\n",
        private_key.replace("\r\n", "\n").replace('\r', "\n").trim()
    )
}

/// Whether `public_key` is an OpenSSH public key (`<type> <base64>`, an
/// optional comment) of a supported type.
pub fn valid_public_key(public_key: &str) -> bool {
    ssh_key::PublicKey::from_openssh(public_key).is_ok_and(|key| {
        matches!(
            key.algorithm(),
            ssh_key::Algorithm::Ed25519
                | ssh_key::Algorithm::Rsa { .. }
                | ssh_key::Algorithm::Ecdsa { .. }
        )
    })
}

/// Validates `private_key` and derives its public key and fingerprint.
pub fn inspect(private_key: &str) -> Result<KeyInfo, KeyError> {
    if private_key.trim().is_empty() {
        return Err(KeyError::Blank);
    }
    if private_key.len() > MAX_BYTES {
        return Err(KeyError::TooLarge);
    }
    let key = normalize(private_key);
    if key.starts_with("ssh-")
        || key.starts_with("ecdsa-")
        || key.starts_with("-----BEGIN PUBLIC KEY-----")
    {
        return Err(KeyError::Public);
    }
    if !(key.starts_with("-----BEGIN ") && key.contains("\n-----END ")) {
        return Err(KeyError::Incomplete);
    }
    if key.contains("BEGIN ENCRYPTED PRIVATE KEY") || key.contains("Proc-Type: 4,ENCRYPTED") {
        return Err(KeyError::Passphrase);
    }
    let public = if key.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----") {
        let parsed = PrivateKey::from_openssh(&key).map_err(|_| KeyError::Invalid)?;
        if parsed.is_encrypted() {
            return Err(KeyError::Passphrase);
        }
        parsed.public_key().key_data().clone()
    } else {
        pem_public_key(&key).ok_or(KeyError::Invalid)?
    };
    let public = ssh_key::PublicKey::from(public);
    if !matches!(
        public.algorithm(),
        ssh_key::Algorithm::Ed25519
            | ssh_key::Algorithm::Rsa { .. }
            | ssh_key::Algorithm::Ecdsa { .. }
    ) {
        return Err(KeyError::Invalid);
    }
    Ok(KeyInfo {
        public_key: public
            .to_openssh()
            .map_err(|_| KeyError::Invalid)?
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" "),
        fingerprint: public.fingerprint(HashAlg::Sha256).to_string(),
    })
}

fn pem_public_key(pem: &str) -> Option<KeyData> {
    use pkcs8::{DecodePrivateKey, PrivateKeyInfo, der::Decode};
    use rsa::pkcs1::DecodeRsaPrivateKey;

    if pem.starts_with("-----BEGIN RSA PRIVATE KEY-----") {
        let key = rsa::RsaPrivateKey::from_pkcs1_pem(pem).ok()?;
        return KeyData::try_from(&KeypairData::from(RsaKeypair::try_from(key).ok()?)).ok();
    }
    if pem.starts_with("-----BEGIN EC PRIVATE KEY-----") {
        return ec_from_sec1(pem);
    }
    if !pem.starts_with("-----BEGIN PRIVATE KEY-----") {
        return None;
    }
    if let Ok(key) = rsa::RsaPrivateKey::from_pkcs8_pem(pem) {
        return KeyData::try_from(&KeypairData::from(RsaKeypair::try_from(key).ok()?)).ok();
    }
    if let Some(key) = ec_from_pkcs8(pem) {
        return Some(key);
    }
    // Ed25519: the PKCS#8 payload is an OCTET STRING holding the 32-byte seed.
    let (_, der) = pem_rfc7468_decode(pem)?;
    let info = PrivateKeyInfo::from_der(&der).ok()?;
    let seed: [u8; 32] = info
        .private_key
        .strip_prefix(&[0x04, 0x20])?
        .try_into()
        .ok()?;
    KeyData::try_from(&KeypairData::from(Ed25519Keypair::from_seed(&seed))).ok()
}

fn pem_rfc7468_decode(pem: &str) -> Option<(String, Vec<u8>)> {
    let (label, der) = pkcs8::der::pem::decode_vec(pem.as_bytes()).ok()?;
    Some((label.to_owned(), der))
}

macro_rules! ec_public {
    ($curve:ident, $secret:expr) => {{
        let point = $curve::elliptic_curve::sec1::ToEncodedPoint::to_encoded_point(
            &$secret.public_key(),
            false,
        );
        EcdsaPublicKey::from_sec1_bytes(point.as_bytes())
            .ok()
            .map(KeyData::Ecdsa)
    }};
}

fn ec_from_sec1(pem: &str) -> Option<KeyData> {
    if let Ok(secret) = p256::SecretKey::from_sec1_pem(pem) {
        return ec_public!(p256, secret);
    }
    if let Ok(secret) = p384::SecretKey::from_sec1_pem(pem) {
        return ec_public!(p384, secret);
    }
    let secret = p521::SecretKey::from_sec1_pem(pem).ok()?;
    ec_public!(p521, secret)
}

fn ec_from_pkcs8(pem: &str) -> Option<KeyData> {
    use pkcs8::DecodePrivateKey;
    if let Ok(secret) = p256::SecretKey::from_pkcs8_pem(pem) {
        return ec_public!(p256, secret);
    }
    if let Ok(secret) = p384::SecretKey::from_pkcs8_pem(pem) {
        return ec_public!(p384, secret);
    }
    let secret = p521::SecretKey::from_pkcs8_pem(pem).ok()?;
    ec_public!(p521, secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway key generated for this test only.
    const ED25519: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACCM5PbHMICER29FSSQxVg1BlvlSr181HbjPaJLDhYMQ3AAAAJhEonyMRKJ8
jAAAAAtzc2gtZWQyNTUxOQAAACCM5PbHMICER29FSSQxVg1BlvlSr181HbjPaJLDhYMQ3A
AAAED96ea9aUZyIb7JKnzjtYc4Q17zOVoDO3kvWsa6p6n2FYzk9scwgIRHb0VJJDFWDUGW
+VKvXzUduM9oksOFgxDcAAAAEXRlc3RAZXhhbXBsZS50ZXN0AQIDBA==
-----END OPENSSH PRIVATE KEY-----
";

    #[test]
    fn derives_public_key_and_fingerprint() {
        let info = inspect(ED25519).unwrap();
        assert!(info.public_key.starts_with("ssh-ed25519 AAAA"));
        assert_eq!(info.public_key.split(' ').count(), 2);
        assert!(info.fingerprint.starts_with("SHA256:"));
        // CRLF and surrounding whitespace are tolerated.
        let crlf = format!("  {}  ", ED25519.replace('\n', "\r\n"));
        assert_eq!(inspect(&crlf).unwrap(), info);
    }

    #[test]
    fn recognizes_openssh_public_keys() {
        let info = inspect(ED25519).unwrap();
        assert!(valid_public_key(&info.public_key));
        assert!(valid_public_key(&format!("{} me@host", info.public_key)));
        for bad in [
            "",
            "ssh-ed25519",
            "ssh-ed25519 !!!!",
            "hello world",
            "ssh-dss AAAA",
        ] {
            assert!(!valid_public_key(bad), "{bad}");
        }
    }

    #[test]
    fn rejects_bad_input_without_echoing_it() {
        assert_eq!(inspect("  "), Err(KeyError::Blank));
        assert_eq!(inspect("ssh-ed25519 AAAA x"), Err(KeyError::Public));
        assert_eq!(inspect("hello"), Err(KeyError::Incomplete));
        assert_eq!(
            inspect(
                "-----BEGIN ENCRYPTED PRIVATE KEY-----\nx\n-----END ENCRYPTED PRIVATE KEY-----"
            ),
            Err(KeyError::Passphrase)
        );
        assert_eq!(
            inspect("-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----"),
            Err(KeyError::Invalid)
        );
        assert_eq!(inspect(&"a".repeat(MAX_BYTES + 1)), Err(KeyError::TooLarge));
    }

    #[test]
    fn reads_pem_keys() {
        for (name, pem, fingerprint) in [
            (
                "ed",
                "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIF68v5Uhz/bEh2/Kt4bz8Omu+UHt00Aw7C9bnxSyJy07\n-----END PRIVATE KEY-----",
                "SHA256:Yxh06ieJ0Ftzovmo25VR/VwcmqGqSzD2OkQov7eKits",
            ),
            (
                "ec",
                "-----BEGIN EC PRIVATE KEY-----\nMHcCAQEEIG17RefyVAUJFH72IbMXmWXcNAUxYtJkWEGMpalK7vVToAoGCCqGSM49\nAwEHoUQDQgAEScQqDpoSTLM9ltHf1w8O4LcZKrwpyvPLcUfDBdXYi7zSGZiQG/s9\nXAaAgn9FOOriGuPkP8EOQNFNvJqPQvKzXQ==\n-----END EC PRIVATE KEY-----",
                "SHA256:mjaqXR1JvxUdrU9MxVGINEq4AaCARdPmGeEHxjaT2t8",
            ),
            (
                "ec384",
                "-----BEGIN PRIVATE KEY-----\nMIG2AgEAMBAGByqGSM49AgEGBSuBBAAiBIGeMIGbAgEBBDBp/faDnvs5ev8nVLWL\nKcFV7sd2H+sngQLQALS33sYpxg4XokcVhjPvfHvgfqiKiPqhZANiAARIyvPMPfH0\nqmuTbBKAlTHlaiKZzcDtp7n2S/YdyV/sSccg1OgxqMBpnzWrtETKODDuujfG2u1r\nu91XNoPUevLJW/hHM4pjkBovkVC+M+tjN6H3tli7WsFBAhpSgUKX/rY=\n-----END PRIVATE KEY-----",
                "SHA256:msVnCPZ+u8vNS/WggD4BKohCL781D/lb+wrtAE7NNG0",
            ),
            (
                "rsa",
                "-----BEGIN RSA PRIVATE KEY-----\nMIIEogIBAAKCAQEAy/cZfwxwwGuCVKj7joT+fLPyq7OcTpaUlTBveKZCcBCY2wI1\nuPRFWrVf+IyyrhBz8h4uJxIfKxBklwhFro2qaESGe9/NqICYUEDNDX2ENrouX+t7\nUHnnh83OqDEHpwNZOoHyss06p1xcS6kOo+cwoKacZycbE4lDFEBWo6e0eh3FSZ6t\n/EvkmO8vN5qT+welMtD2tj3AveSueTO0C8UgStNGJL6DdRasdaA9zOHVKPmsr3lk\nurvnI8xmbYp9qx0LlRa0LnfVrK0E6JO23EkWQ8oMuLqLzll2eFYzr76Q2MjZVaU/\nxioB3A1CBzg7OpsIOCJN16J7SdJ0mIr3TsO3lQIDAQABAoIBAA8EmzZ9GoI4iEim\nhps4VKqm70vQNkadFMSlY9fM2iB5QGAD2iPLrJ2gtDfPWEqzwG3H5hsPXQclNlre\ni47uNGln/kT5tiFdkpl6I/8FymnjLI34f0RuLWuU7k35Cji0aOoDbnTW4qY3CT6y\n7HaKC80QZ3y65uX52dwQXrC/JO1GyTkEdjbqkPrzh8WlLUhyxE4ARz1iiM9O+tng\neUC89aA4kG7iC8ba/YoppUNTYfCAnQ7rwXSv4Q51I8qRyhpCsgRggF+7okg4iyyZ\nkLlA4vWeoCJZbgolRcd5VVsAhbikOjWH22a/lAjn7l5M839Y1/cu1/UkOOz4YCAA\n9sX9oHMCgYEA64/jrU6qR2F+Rb664Ld5aZEr6/4hLJxe/P+sdLdwT4jf3O2Y7B9e\nbcwt0ulMrdbVDV8Z/3/q4RqPcVj+eoVbA1vZa6ji5uvlzMO3fujyrxPZUG1JB/FH\nNG4ll6n/Dni1cqqYsLyLNDmOwlr6jAOfSimIQjUkymw1ajMt2hYJemMCgYEA3alo\nSHag50aInTpULboEVrMzSWQQQqb1Ob6+SAuPVqZoMhkDwykdobo6WJZ27TxPPvh5\n5rR46yyWJnXJOX8AU42JiBCn5dJSKrAUx+OMgRpDqwccFOA2GSaKOPUfd7QIUjrJ\n4Gm5ZMC1cg2FPC7tj6K0grxzPHXhvLwmuL6U66cCgYAP0KlIiW+FO1Zu+E8zpx3P\nqJ96SKWRiS4XblL1wxsXO+lLu+f4cmK0nHC3KFKtmS7Upv3n2X67fj0DLyZms21Y\n3GHMiJozRsM27Cz1qE4i0wJUFAJDNj/BlWOQuykUn6YO/MUjmIUADJ05D0KjA9s9\n72vmhgriTEGJz8XUToZ0YQKBgGLP5SQAdgQDZl0opmPn+40mv2MlDzaXNNQ0GqEF\nPjWzYtmKWyoBxyrUWLQrBwMEkpMQs07L/udfbePhN/3AhsW4X6nfOulxk51h9zGQ\nV48pxSaVucf4l0Uo8A8p/ogfImJ9wIaguruWSIx6SH2msdIJBw0MsxYtup6nBV/v\n3XUbAoGARZzRyzZMRxBXeZzDkTk0SD2MHPtOtxNz5MelVTtxZQIvk6cyLMgikVPD\nTsW8gUUM4Ga8HfYl/m3vTAXTOZuNdaVg8u3Js+0MKAaSPbNvx/UgBs5y380oszh7\nD/rx34MjEbTsSFMXMMmbkbnLNnKiJ9W6JMZB/PbImoab6z5x2uM=\n-----END RSA PRIVATE KEY-----",
                "SHA256:zd52ol7MQmW3+IN+eVBfi6k25IozyN6f100IrcHiHzk",
            ),
        ] {
            assert_eq!(inspect(pem).unwrap().fingerprint, fingerprint, "{name}");
        }
    }
}

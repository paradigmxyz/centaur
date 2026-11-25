//! Private-key validation, public-key derivation, and fingerprints for the two
//! Console SSH key paths.
//!
//! A pasted key may be OpenSSH format or any PEM private-key encoding: PKCS#1
//! RSA, SEC1 EC, and PKCS#8. The bearer `ssh_key_secrets` API and the console
//! `me/ssh_key` page share this module so their accept/reject set, stored public
//! key, and fingerprints cannot drift.
//!
//! Only public material is derived here. Submitted private-key bytes are never
//! returned, logged, or copied into an error message.
//!
//! Known deviation: an EC PEM key that omits its public point is reported as
//! unreadable instead of having its point computed from the private scalar.
//! OpenSSL-generated SEC1 and PKCS#8 EC keys always embed the point.

use base64::Engine as _;
use sha2::{Digest, Sha256};

/// The largest accepted private key, in bytes.
pub const MAX_BYTES: usize = 64 * 1024;

pub const BLANK_MESSAGE: &str = "Paste your private key, including its BEGIN and END lines.";
pub const TOO_LARGE_MESSAGE: &str = "The key is too large. Paste a single private key.";
pub const PUBLIC_KEY_MESSAGE: &str =
    "This is a public key. Paste the private key file, without the .pub extension.";
pub const INCOMPLETE_MESSAGE: &str =
    "Include the complete private key, including its BEGIN and END lines.";
pub const PASSPHRASE_MESSAGE: &str =
    "This key is protected by a passphrase. Use a dedicated key without a passphrase.";
pub const UNSUPPORTED_TYPE_MESSAGE: &str =
    "This key type is not supported. Use an Ed25519, RSA, or ECDSA key.";
pub const UNREADABLE_MESSAGE: &str =
    "This key could not be read. Copy the entire private key file, including both header lines.";

/// The supported OpenSSH key types.
pub const SUPPORTED_TYPES: [&str; 5] = [
    "ssh-ed25519",
    "ssh-rsa",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
];

/// CRLF/CR to LF, strip, and end with one newline.
pub fn normalize_private_key(value: &str) -> String {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    format!("{}\n", normalized.trim())
}

/// The `<type> <base64>` OpenSSH public key, or the exact Console error
/// message. Messages never include the submitted key.
pub fn derive_public_key(private_key: &str) -> Result<String, String> {
    if private_key.trim().is_empty() {
        return Err(BLANK_MESSAGE.to_owned());
    }
    if private_key.len() > MAX_BYTES {
        return Err(TOO_LARGE_MESSAGE.to_owned());
    }
    let key = normalize_private_key(private_key);
    if key.starts_with("ssh-")
        || key.starts_with("ecdsa-")
        || key.starts_with("-----BEGIN PUBLIC KEY-----")
    {
        return Err(PUBLIC_KEY_MESSAGE.to_owned());
    }
    if !(key.starts_with("-----BEGIN ") && key.contains("\n-----END ")) {
        return Err(INCOMPLETE_MESSAGE.to_owned());
    }
    // Rejected before parsing so nothing ever asks for a PEM passphrase.
    if key.contains("BEGIN ENCRYPTED PRIVATE KEY") || key.contains("Proc-Type: 4,ENCRYPTED") {
        return Err(PASSPHRASE_MESSAGE.to_owned());
    }
    // OpenSSH-format keys go to the OpenSSH loader; every other PEM label goes
    // to the PEM reader.
    if key.contains("-----BEGIN OPENSSH PRIVATE KEY-----") {
        openssh_public_key(&key)
    } else {
        pem_public_key(&key)
    }
}

/// The OpenSSH `SHA256:` fingerprint without padding. `None` when the key is
/// not a supported type.
pub fn fingerprint(public_key: &str) -> Option<String> {
    let mut parts = public_key.split_whitespace();
    let algorithm = parts.next()?;
    let body = parts.next()?;
    if !SUPPORTED_TYPES.contains(&algorithm) || body.is_empty() {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(body)
        .ok()?;
    let digest = Sha256::digest(&decoded);
    let encoded = base64::engine::general_purpose::STANDARD.encode(digest);
    Some(format!("SHA256:{}", encoded.trim_end_matches('=')))
}

/// A supported type and an OpenSSH base64 body (`^[A-Za-z0-9+/]+={0,2}$`).
pub fn supported_public_key(value: &str) -> bool {
    let mut parts = value.split_whitespace();
    let (Some(algorithm), Some(body)) = (parts.next(), parts.next()) else {
        return false;
    };
    SUPPORTED_TYPES.contains(&algorithm) && openssh_base64(body)
}

fn openssh_base64(value: &str) -> bool {
    let Some(padding_start) = value.find('=') else {
        return !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'));
    };
    let (body, padding) = value.split_at(padding_start);
    !body.is_empty()
        && body
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
        && padding.len() <= 2
        && padding.bytes().all(|byte| byte == b'=')
}

/// The OpenSSH private-key form, including its passphrase-protected variant.
fn openssh_public_key(key: &str) -> Result<String, String> {
    let parsed = ssh_key::PrivateKey::from_openssh(key).map_err(|_| unreadable())?;
    // A bcrypt-KDF key parses without its passphrase; refuse before anything
    // tries to derive a public key from it.
    if parsed.is_encrypted() {
        return Err(PASSPHRASE_MESSAGE.to_owned());
    }
    key_string(parsed.public_key())
}

/// Every PEM private-key form, dispatched by label.
fn pem_public_key(key: &str) -> Result<String, String> {
    let (label, der) = pem_body(key).ok_or_else(unreadable)?;
    match label {
        "RSA PRIVATE KEY" => rsa_public_key(&der),
        "EC PRIVATE KEY" => ec_public_key(&der, None),
        "PRIVATE KEY" => pkcs8_public_key(&der),
        _ => Err(unreadable()),
    }
}

/// The single PEM block's label and DER content.
fn pem_body(key: &str) -> Option<(&str, Vec<u8>)> {
    let begin = key.strip_prefix("-----BEGIN ")?;
    let (label, rest) = begin.split_once("-----\n")?;
    let (body, _) = rest.split_once(&format!("\n-----END {label}-----"))?;
    let der = base64::engine::general_purpose::STANDARD
        .decode(body.replace('\n', ""))
        .ok()?;
    Some((label, der))
}

/// `PKCS#1 RSAPrivateKey`, and the inner key of an RSA PKCS#8 document.
fn rsa_public_key(der: &[u8]) -> Result<String, String> {
    let fields = der_sequence(der).ok_or_else(unreadable)?;
    // version, modulus, publicExponent, privateExponent, prime1, prime2,
    // exponent1, exponent2, coefficient. A multi-prime key appends more.
    if fields.len() < 9 || !fields[..9].iter().all(|(tag, _)| *tag == 0x02) {
        return Err(unreadable());
    }
    // A multi-prime key appends exactly one `OtherPrimeInfos` sequence.
    if fields.len() > 9 && (fields.len() != 10 || fields[9].0 != 0x30) {
        return Err(unreadable());
    }
    let modulus = ssh_key::Mpint::try_from(fields[1].1).map_err(|_| unreadable())?;
    let exponent = ssh_key::Mpint::try_from(fields[2].1).map_err(|_| unreadable())?;
    let data = ssh_key::public::KeyData::Rsa(ssh_key::public::RsaPublicKey {
        e: exponent,
        n: modulus,
    });
    key_string(&ssh_key::PublicKey::new(data, ""))
}

/// `SEC1 ECPrivateKey`, and the inner key of an EC PKCS#8 document.
/// `algorithm_curve` carries the curve OID from a PKCS#8 algorithm identifier.
fn ec_public_key(der: &[u8], algorithm_curve: Option<&[u8]>) -> Result<String, String> {
    let fields = der_sequence(der).ok_or_else(unreadable)?;
    // version, privateKey, parameters [0], publicKey [1].
    let version_ok = fields
        .first()
        .is_some_and(|(tag, value)| *tag == 0x02 && *value == [1]);
    let secret_ok = fields
        .get(1)
        .is_some_and(|(tag, value)| *tag == 0x04 && !value.is_empty());
    if !version_ok || !secret_ok {
        return Err(unreadable());
    }
    let mut parameters = None;
    let mut point = None;
    for (tag, value) in fields.iter().skip(2) {
        match tag {
            0xa0 if parameters.is_none() => {
                parameters = Some(explicit_oid(value).ok_or_else(unreadable)?);
            }
            0xa1 if point.is_none() => {
                point = Some(bit_string(value).ok_or_else(unreadable)?);
            }
            _ => return Err(unreadable()),
        }
    }
    let Some(oid) = parameters.or(algorithm_curve) else {
        return Err(unreadable());
    };
    let Some((algorithm, curve)) = curve_for_oid(oid) else {
        // An unknown curve has no supported algorithm name either.
        return Err(UNSUPPORTED_TYPE_MESSAGE.to_owned());
    };
    let Some(point) = point else {
        return Err(unreadable());
    };
    let parsed =
        ssh_key::public::EcdsaPublicKey::from_sec1_bytes(point).map_err(|_| unreadable())?;
    if parsed.curve().as_str() != curve {
        return Err(unreadable());
    }
    let data = ssh_key::public::KeyData::Ecdsa(parsed);
    let public = ssh_key::PublicKey::new(data, "");
    debug_assert_eq!(public.algorithm().as_str(), algorithm);
    key_string(&public)
}

/// `PKCS#8 PrivateKeyInfo`/`OneAsymmetricKey`.
fn pkcs8_public_key(der: &[u8]) -> Result<String, String> {
    let fields = der_sequence(der).ok_or_else(unreadable)?;
    // version, privateKeyAlgorithm, privateKey, attributes [0].
    let version_ok = fields
        .first()
        .is_some_and(|(tag, value)| *tag == 0x02 && matches!(*value, [0] | [1]));
    let (Some((0x30, algorithm)), Some((0x04, private_key))) = (fields.get(1), fields.get(2))
    else {
        return Err(unreadable());
    };
    if !version_ok || private_key.is_empty() {
        return Err(unreadable());
    }
    let algorithm_fields = children(algorithm).ok_or_else(unreadable)?;
    let (Some((0x06, oid)), parameters) = (algorithm_fields.first(), algorithm_fields.get(1))
    else {
        return Err(unreadable());
    };
    match *oid {
        RSA_ENCRYPTION_OID | RSASSA_PSS_OID => rsa_public_key(private_key),
        EC_PUBLIC_KEY_OID => {
            let curve = match parameters {
                Some((0x06, oid)) => Some(*oid),
                None => None,
                // Explicit curve parameters are not an algorithm OID.
                Some(_) => return Err(unreadable()),
            };
            ec_public_key(private_key, curve)
        }
        // Ed25519, X25519, and friends have no SSH type, so they are unreadable.
        _ => Err(unreadable()),
    }
}

/// `<algorithm> <base64 key data>` for a parsed public key.
fn key_string(public: &ssh_key::PublicKey) -> Result<String, String> {
    let openssh = public.to_openssh().map_err(|_| unreadable())?;
    // `to_openssh` keeps the key's comment; the stored public key is
    // "<type> <base64>" only.
    let mut parts = openssh.split_whitespace();
    let (Some(algorithm), Some(body)) = (parts.next(), parts.next()) else {
        return Err(unreadable());
    };
    if !SUPPORTED_TYPES.contains(&algorithm) {
        return Err(UNSUPPORTED_TYPE_MESSAGE.to_owned());
    }
    Ok(format!("{algorithm} {body}"))
}

fn unreadable() -> String {
    UNREADABLE_MESSAGE.to_owned()
}

/// `id-rsaEncryption` and `id-RSASSA-PSS`, both read as RSA keys.
const RSA_ENCRYPTION_OID: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const RSASSA_PSS_OID: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0a];
const EC_PUBLIC_KEY_OID: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const PRIME256V1_OID: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const SECP384R1_OID: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];
const SECP521R1_OID: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x23];

/// The curve name and SSH algorithm for a curve OID. Unknown curves have no
/// alias, so they are unsupported types.
fn curve_for_oid(oid: &[u8]) -> Option<(&'static str, &'static str)> {
    match oid {
        PRIME256V1_OID => Some(("ecdsa-sha2-nistp256", "nistp256")),
        SECP384R1_OID => Some(("ecdsa-sha2-nistp384", "nistp384")),
        SECP521R1_OID => Some(("ecdsa-sha2-nistp521", "nistp521")),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Minimal DER reader: enough for the PEM private-key structures above.
// ---------------------------------------------------------------------------

/// Read one tag-length-value at `position`, returning its tag and value.
fn read_tlv<'a>(data: &'a [u8], position: &mut usize) -> Option<(u8, &'a [u8])> {
    let tag = *data.get(*position)?;
    let first = *data.get(*position + 1)?;
    let (length, header) = if first < 0x80 {
        (usize::from(first), 2)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 {
            return None;
        }
        let mut length = 0usize;
        for offset in 0..count {
            length = (length << 8) | usize::from(*data.get(*position + 2 + offset)?);
        }
        (length, 2 + count)
    };
    let start = position.checked_add(header)?;
    let end = start.checked_add(length)?;
    let value = data.get(start..end)?;
    *position = end;
    Some((tag, value))
}

/// The values of a DER SEQUENCE, which must be the whole input.
fn der_sequence(der: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let mut position = 0;
    let (tag, value) = read_tlv(der, &mut position)?;
    if tag != 0x30 || position != der.len() {
        return None;
    }
    children(value)
}

fn children(value: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let mut position = 0;
    let mut items = Vec::new();
    while position < value.len() {
        items.push(read_tlv(value, &mut position)?);
    }
    Some(items)
}

/// The OID inside an explicit `[0]` container.
fn explicit_oid(value: &[u8]) -> Option<&[u8]> {
    let mut position = 0;
    let (tag, oid) = read_tlv(value, &mut position)?;
    if tag != 0x06 || position != value.len() {
        return None;
    }
    Some(oid)
}

/// The point bytes of the explicit `[1]` BIT STRING.
fn bit_string(value: &[u8]) -> Option<&[u8]> {
    let mut position = 0;
    let (tag, bits) = read_tlv(value, &mut position)?;
    if tag != 0x03 || position != value.len() {
        return None;
    }
    let (unused, point) = bits.split_first()?;
    if *unused != 0 || point.is_empty() {
        return None;
    }
    Some(point)
}

/// Parser tests: the structurally malformed DER bodies a request can still send,
/// the public-key shape rules, and one case per PEM parse branch against
/// fixtures that keep stored fingerprints stable.
#[cfg(test)]
mod tests {
    use super::*;

    fn pem(label: &str, der: &[u8]) -> String {
        let body = base64::engine::general_purpose::STANDARD.encode(der);
        format!("-----BEGIN {label}-----\n{body}\n-----END {label}-----\n")
    }

    fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
        assert!(value.len() < 0x80, "test fixtures use short lengths");
        let mut out = vec![tag, u8::try_from(value.len()).expect("short length")];
        out.extend_from_slice(value);
        out
    }

    fn message(key: &str) -> String {
        derive_public_key(key).expect_err("key should be rejected")
    }

    fn fixture(name: &str, file: &str, public_key: &str, fingerprint: &str) {
        let key = KEY_FIXTURES
            .iter()
            .find(|(n, _)| *n == file)
            .expect("fixture")
            .1;
        assert_eq!(derive_public_key(key), Ok(public_key.to_owned()), "{name}");
        assert_eq!(
            super::fingerprint(public_key).as_deref(),
            Some(fingerprint),
            "{name}"
        );
    }

    const KEY_FIXTURES: [(&str, &str); 3] = [
        (
            "rsa-pkcs1.pem",
            r#"-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEAz5ZwhZOGKbhgBIhjZl+CsXNBPP3Xa9h4yHgyIIDaelxxnn6d
jNn261euH3T75YWNZEUOuQTt+t6NCQRSbdgCQBF9dBPvcJVt10A/5e6lws8kS/we
mljom9QENsBD4XX+NUJ0f1AJmdIlYELk6McQXG9+GZx+yQHduK3cNLNTxeme7230
xb6f1Bvtn6YYNP1YcGN2Tz0XZrAoFZrCeMRNBVfFoqTuPS8Iyi9Pm09e7PJ4rH7G
qnJjV/R6y2CGF7PNUKKC/bwOEQqMzm3rPiAcsA9UQ9OV66M8nrjyAnMN06U1IiSK
l6VDO57iOVfwYw63MQsSz6BAMDeOmVG+fcnO2wIDAQABAoIBAEiOwde+n8dXXaBX
975hpfHWYjgqVbCpYlZXxNWdlD/urfwDL5h62xaZeBAguKcaDji89o9vDKuCPHOp
YhJozSiNtHbtPOTyJgl1uJ/q0V+NPZedkSjL6MoGyk5E17Uno+oDx2p0lFaCoHPM
DezbH3nfsojLLXmewFv70RtOjvp+syiUno0AUv0F6ygEI5oeGSigjjLbLfCHUF4j
TY9Wg35ZWZk0TZOBOha2y+sbZbgIqoIbCvEkkpxWTeOiU2bidjyGmO5iIu3RMr5U
UU1nBnkrD6zPxOn2CZg0h4z4RvETD3K1pQnmiYKv6eKjB5D99Kz3IKp/F813fGj+
6AaG1AkCgYEA/Pb86NszKReTidhGLX8HEi7LSdG0jvQzumPiZB/5skIlJfi3c+JG
mCHYZ/3Byj2b1/Ao9Qb+VfoyDM6uRwmkl34aflp6Pb4zFwFHUBGJxKJqL7eqMEHF
Dp/SkNvR//ql2FIX8CuGrTjexCfqLOwJv1SDGNrn0oLyBSh9XT8RwC8CgYEA0hQR
+VuTkPY2EXAC83sUvHEG52Aukk6wDMNTUmpQYO4gBFEBGtP5xysyNUai/sgN+BY/
heNWr3g8ST4JqWCgXnq8y6RrKPr3XxkIWL/YRTWDhF5kfzPunOClaDsMhRZuDR80
g6EQKjJunlA5vR3uxBXubW062DgO97pC1uiF5RUCgYEA+5JGAu27+2nO+I91e1mR
Fqwiz88V2fVSXFNpmlmSYY0WlKS9DSpDOpiuNv1N9GtAyYe1s7i4x5iCneoNt+gH
IAHxJyej2jOFa6OB4EcQ09UR9VKallNSPjEDn7x9JC2JMZCy7c7bqPmd0UZAxVwY
dYb+uYE6e0humGKkCk/mv1ECgYAVJy40JJN0SkZhdRzm9WK+IsdmLtoho0i40EYo
i0nyqKeSp7FrySJieqgAk09ARlaZv2vACIcm/f1cBqIkavVa7hLqknCz3Klh6vxb
K14UgfHvNy35XPpO05P0LdXx5LNKIPVBt8sP5KocyMk3kLcFCYxLem8tbF+ddCTc
zOcGjQKBgCekrB5ZpT/8fL9crBVQXzhLPnnhaS1VqeD4GUovfz7W/hfAfMK4b6x7
CYNaGqbAYqhtgwmMdK/uSG02woUd8l8MNUYOQ6Q+ITTN/ejeK5+2edaqy6dC7ZRk
H+Z+tOxs2g8DcMoE5jK7/SsoDGjDGsWGNu2d9WyCR8TOYEhcr8st
-----END RSA PRIVATE KEY-----
"#,
        ),
        (
            "p384-sec1.pem",
            r#"-----BEGIN EC PRIVATE KEY-----
MIGkAgEBBDDch9CZhHu1Hla8IKXulaTNQ7nSbTmusw0+r2THBoEUKCAQHT735gFY
GPnuZinXkrmgBwYFK4EEACKhZANiAAQ8KPGKHpLHU3HlvAvM5cxboMf9ifrMLTxI
tKhJOWk4SwX0IX8s67gcTnSgBsbYXg/LVSj6rfvtksbLMhVwKuGGTa8ZlcaqF6LF
RbJoJk92f5DdP9RHzaR6PnRemuRjVmY=
-----END EC PRIVATE KEY-----
"#,
        ),
        (
            "p256-pkcs8.pem",
            r#"-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg5N3MNwQx1AwgLelq
J21ANffevsBSZ0GpvmZ7apJOS+yhRANCAASBdUub0O7lFglOawriKr96z4cX5u+e
rYcf5QLidsSYuSb5W6HMAcNNeePs+nR9n/qR3SQpxTZT38JPJwWOuaG5
-----END PRIVATE KEY-----
"#,
        ),
    ];

    #[test]
    fn derives_the_net_ssh_public_key_and_fingerprint() {
        fixture(
            "RSA PKCS#1",
            "rsa-pkcs1.pem",
            r#"ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQDPlnCFk4YpuGAEiGNmX4Kxc0E8/ddr2HjIeDIggNp6XHGefp2M2fbrV64fdPvlhY1kRQ65BO363o0JBFJt2AJAEX10E+9wlW3XQD/l7qXCzyRL/B6aWOib1AQ2wEPhdf41QnR/UAmZ0iVgQuToxxBcb34ZnH7JAd24rdw0s1PF6Z7vbfTFvp/UG+2fphg0/VhwY3ZPPRdmsCgVmsJ4xE0FV8WipO49LwjKL0+bT17s8nisfsaqcmNX9HrLYIYXs81QooL9vA4RCozObes+IBywD1RD05XrozyeuPICcw3TpTUiJIqXpUM7nuI5V/BjDrcxCxLPoEAwN46ZUb59yc7b"#,
            "SHA256:09RdrCa9b+cdrTp6rMy2vg2yySERCRYI0r433tNyOaM",
        );
        fixture(
            "SEC1 EC",
            "p384-sec1.pem",
            r#"ecdsa-sha2-nistp384 AAAAE2VjZHNhLXNoYTItbmlzdHAzODQAAAAIbmlzdHAzODQAAABhBDwo8YoeksdTceW8C8zlzFugx/2J+swtPEi0qEk5aThLBfQhfyzruBxOdKAGxtheD8tVKPqt++2SxssyFXAq4YZNrxmVxqoXosVFsmgmT3Z/kN0/1EfNpHo+dF6a5GNWZg=="#,
            "SHA256:B7DmIRlCMHTZjkoCqS/dIA1Q3Xwy7DziQ1ZSWBO4H2Q",
        );
        fixture(
            "PKCS#8 EC",
            "p256-pkcs8.pem",
            r#"ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBIF1S5vQ7uUWCU5rCuIqv3rPhxfm756thx/lAuJ2xJi5JvlbocwBw0154+z6dH2f+pHdJCnFNlPfwk8nBY65obk="#,
            "SHA256:gZ74zLpwwMvW6ZxhlCg0AVri/dYhtMsR1l3yb5zshMw",
        );
    }

    #[test]
    fn structurally_broken_pem_bodies_are_unreadable() {
        // Indefinite length, a length past the end, and a non-sequence outer tag.
        for der in [
            vec![0x30, 0x80, 0x00],
            vec![0x30, 0x05, 0x02, 0x01],
            vec![0x02, 0x01, 0x00],
        ] {
            for label in ["RSA PRIVATE KEY", "EC PRIVATE KEY", "PRIVATE KEY"] {
                assert_eq!(message(&pem(label, &der)), UNREADABLE_MESSAGE);
            }
        }
        assert_eq!(
            message(
                "-----BEGIN RSA PRIVATE KEY-----\n***not base64***\n-----END RSA PRIVATE KEY-----\n"
            ),
            UNREADABLE_MESSAGE
        );
    }

    #[test]
    fn incomplete_key_structures_are_unreadable() {
        // EC without its public point, and EC without curve parameters.
        let pointless = tlv(
            0x30,
            &[
                tlv(0x02, &[1]),
                tlv(0x04, &[1; 32]),
                tlv(0xa0, &tlv(0x06, PRIME256V1_OID)),
            ]
            .concat(),
        );
        assert_eq!(
            message(&pem("EC PRIVATE KEY", &pointless)),
            UNREADABLE_MESSAGE
        );
        let curveless = tlv(0x30, &[tlv(0x02, &[1]), tlv(0x04, &[1; 32])].concat());
        assert_eq!(
            message(&pem("EC PRIVATE KEY", &curveless)),
            UNREADABLE_MESSAGE
        );
        // PKCS#1 RSA truncated before its CRT parameters.
        let truncated = tlv(
            0x30,
            &[
                tlv(0x02, &[0]),
                tlv(0x02, &[0x80; 4]),
                tlv(0x02, &[0x01, 0x00, 0x01]),
            ]
            .concat(),
        );
        assert_eq!(
            message(&pem("RSA PRIVATE KEY", &truncated)),
            UNREADABLE_MESSAGE
        );
        // PKCS#8 with no OCTET STRING private key.
        let empty = tlv(
            0x30,
            &[tlv(0x02, &[0]), tlv(0x30, &tlv(0x06, RSA_ENCRYPTION_OID))].concat(),
        );
        assert_eq!(message(&pem("PRIVATE KEY", &empty)), UNREADABLE_MESSAGE);
    }

    #[test]
    fn oversized_keys_are_rejected_before_parsing() {
        let key = format!("{}\n{}", KEY_FIXTURES[0].1, "A".repeat(MAX_BYTES));
        assert_eq!(message(&key), TOO_LARGE_MESSAGE);
    }

    #[test]
    fn public_key_shape_matches_the_model_validation() {
        // A trailing comment is a label, not key material.
        assert!(supported_public_key(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEuNuStnfDhVkTG1ZZLnHKzeZZZOqu1eI0osLjEgf3q9 diff@example.test"
        ));
        assert!(!supported_public_key("ssh-dss AAAAB3NzaC1kc3M="));
        assert!(!supported_public_key("ssh-rsa "));
        assert!(!supported_public_key("ssh-rsa AA=A"));
        assert!(!supported_public_key("ssh-rsa A==="));
        assert!(!supported_public_key(""));
    }
}

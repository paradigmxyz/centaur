use sha2::{Digest, Sha256};
use sqids::Sqids;

/// Console's opaque id for row `id` of the model with oid prefix `prefix`.
pub fn oid(prefix: &str, id: i64) -> String {
    format!(
        "{prefix}_{}",
        encoder(prefix)
            .encode(&[id as u64])
            .expect("database IDs fit sqids")
    )
}

/// The row id an opaque id names, if `value` is a canonical oid with `prefix`.
pub fn decode_oid(prefix: &str, value: &str) -> Option<i64> {
    let encoded = value.strip_prefix(prefix)?.strip_prefix('_')?;
    let sqids = encoder(prefix);
    let [id] = sqids.decode(encoded)[..] else {
        return None;
    };
    // Sqids decodes some non-canonical strings; Console rejects them too.
    if sqids.encode(&[id]).ok()? != encoded {
        return None;
    }
    i64::try_from(id).ok()
}

/// Console permutes the base alphabet per prefix, so ids of different models
/// do not decode as each other.
fn encoder(prefix: &str) -> Sqids {
    let mut alphabet: Vec<char> = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        .chars()
        .collect();
    alphabet.sort_by_key(|character| {
        hex::encode(Sha256::digest(format!("{prefix}:{character}").as_bytes()))
    });
    Sqids::builder()
        .alphabet(alphabet)
        .min_length(8)
        .build()
        .expect("valid sqids settings")
}

#[cfg(test)]
mod tests {
    use super::{decode_oid, oid};

    #[test]
    fn opaque_ids_match_rails() {
        assert_eq!(oid("prn", 1), "prn_5CO4fITZ");
        assert_eq!(oid("prn", 3), "prn_xUC2fVYG");
        assert_eq!(oid("prn", 123), "prn_yRoWctYw");
    }

    #[test]
    fn decodes_only_canonical_oids_of_the_prefix() {
        assert_eq!(decode_oid("prn", "prn_yRoWctYw"), Some(123));
        assert_eq!(decode_oid("role", "prn_yRoWctYw"), None);
        assert_eq!(decode_oid("prn", "prn_"), None);
        assert_eq!(decode_oid("prn", "yRoWctYw"), None);
        assert_eq!(decode_oid("prn", "prn_yRoWctY"), None);
    }
}

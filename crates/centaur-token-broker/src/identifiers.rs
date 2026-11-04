use sha2::{Digest, Sha256};

pub(crate) fn oid(prefix: &str, id: i64) -> String {
    let mut alphabet: Vec<char> = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        .chars()
        .collect();
    alphabet.sort_by_key(|character| {
        hex::encode(Sha256::digest(format!("{prefix}:{character}").as_bytes()))
    });
    let sqids = sqids::Sqids::builder()
        .alphabet(alphabet)
        .min_length(8)
        .build()
        .expect("valid sqids settings");
    format!(
        "{prefix}_{}",
        sqids.encode(&[id as u64]).expect("database IDs fit sqids")
    )
}

#[cfg(test)]
mod tests {
    use super::oid;

    #[test]
    fn opaque_ids_match_rails() {
        assert_eq!(oid("prn", 1), "prn_5CO4fITZ");
        assert_eq!(oid("prn", 3), "prn_xUC2fVYG");
        assert_eq!(oid("prn", 123), "prn_yRoWctYw");
    }
}

//! Ciphertexts Active Record wrote (with the development keys) still decrypt:
//! one plain, one compressed.

use active_record_encryption::ActiveRecordEncryption;

fn encryption() -> ActiveRecordEncryption {
    ActiveRecordEncryption::new(
        "dev_ar_encryption_primary_key_0000000000000000",
        "dev_ar_encryption_key_derivation_salt_000000000",
    )
}

#[test]
fn decrypts_ciphertexts_written_by_active_record() {
    let encryption = encryption();
    let plain = "{\"p\":\"kASUBypoZf54C8+c\",\"h\":{\"iv\":\"WDbQUyv12Ro7sqSk\",\"at\":\"Ugbw7JdqET/qSUBkp6MfBA==\"}}";
    assert_eq!(encryption.decrypt(plain).unwrap(), "known secret");
    let compressed = "{\"p\":\"D8yNlq/OZANVMko6\",\"h\":{\"iv\":\"dWPbrq+AHyMayXw6\",\"at\":\"28d0ho86Y+7Eotrc9/g4pQ==\",\"c\":true}}";
    assert_eq!(encryption.decrypt(compressed).unwrap(), "a".repeat(141));
}

#[test]
fn round_trips_its_own_output() {
    let encryption = encryption();
    for plaintext in ["", "pässwörd ✓ \"quoted\" \\ <tag>", &"a".repeat(500)] {
        let ciphertext = encryption.encrypt(plaintext).unwrap();
        assert_eq!(encryption.decrypt(&ciphertext).unwrap(), plaintext);
    }
}

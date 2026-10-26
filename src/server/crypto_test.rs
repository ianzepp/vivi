use super::*;

fn key32(seed: u8) -> String {
    STANDARD.encode([seed; 32])
}

#[test]
fn key_parsing_accepts_exactly_32_bytes() {
    assert!(StoreCipher::from_base64(&key32(1)).is_ok());
    let short = STANDARD.encode([1u8; 31]);
    assert!(StoreCipher::from_base64(&short).is_err());
    assert!(StoreCipher::from_base64("not base64!!").is_err());
}

#[test]
fn round_trip_and_per_record_nonces() {
    let cipher = StoreCipher::from_base64(&key32(7)).unwrap();
    let first = cipher.encrypt(b"hello at rest").unwrap();
    let second = cipher.encrypt(b"hello at rest").unwrap();
    assert_ne!(first, second, "per-record nonces");
    assert_eq!(cipher.decrypt(&first).unwrap(), b"hello at rest");
}

#[test]
fn decrypt_with_wrong_key_fails() {
    let cipher = StoreCipher::from_base64(&key32(1)).unwrap();
    let other = StoreCipher::from_base64(&key32(2)).unwrap();
    let token = cipher.encrypt(b"secret").unwrap();
    assert!(other.decrypt(&token).is_err());
}

#[test]
fn verifier_accepts_right_key_and_rejects_wrong_or_missing() {
    let dir = std::env::temp_dir().join(format!("vivi-server-vc5-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let right = StoreCipher::from_base64(&key32(3)).unwrap();
    let wrong = StoreCipher::from_base64(&key32(4)).unwrap();

    // First boot with a key seals the canary.
    verify_store(&dir, Some(&right)).unwrap();
    assert!(dir.join(VERIFIER_FILE).is_file());
    assert!(verify_store(&dir, Some(&right)).is_ok(), "right key passes");

    let wrong_err = verify_store(&dir, Some(&wrong)).unwrap_err();
    assert!(wrong_err.contains("does not match"), "{wrong_err}");
    let missing_err = verify_store(&dir, None).unwrap_err();
    assert!(missing_err.contains("not set"), "{missing_err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fresh_store_without_key_verifies_clean() {
    let dir = std::env::temp_dir().join(format!("vivi-server-vc5-fresh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    verify_store(&dir, None).unwrap();
    assert!(!dir.join(VERIFIER_FILE).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

use super::*;

#[test]
fn base64url_has_no_padding_or_unsafe_chars() {
    assert_eq!(b64url(b"\xfb\xff\xfe"), "-__-");
    assert_eq!(b64url(b"a"), "YQ");
    assert_eq!(b64url(b""), "");
}

#[test]
fn signing_input_carries_rs256_header_and_claims() {
    let input = jwt_signing_input(5_268_371, 1_000_000);
    let (header, claims) = input.split_once('.').unwrap();
    assert_eq!(header, b64url(br#"{"alg":"RS256","typ":"JWT"}"#));
    let decoded = crate::chat::b64::decode(&pad(claims)).unwrap();
    let claims: Value = serde_json::from_slice(&decoded).unwrap();
    assert_eq!(claims["iss"], "5268371");
    assert_eq!(claims["iat"], 999_970);
    assert_eq!(claims["exp"], 1_000_540);
}

fn pad(segment: &str) -> String {
    let mut text = segment.replace('-', "+").replace('_', "/");
    while !text.len().is_multiple_of(4) {
        text.push('=');
    }
    text
}

#[test]
fn secret_never_prints() {
    let secret = Secret::new("ghs_abcdef".to_string());
    assert!(!format!("{secret:?}").contains("ghs_abcdef"));
    assert_eq!(secret.expose(), "ghs_abcdef");
}

#[test]
fn refs_are_plain_names_only() {
    assert!(check_ref("main").is_ok());
    assert!(check_ref("feature/x-1.2").is_ok());
    assert!(check_ref(&"a".repeat(40)).is_ok());
    for bad in ["", "a b", "a?b", "a#b", "a%2fb"] {
        assert!(check_ref(bad).is_err(), "{bad}");
    }
}

#[test]
fn openssl_signature_verifies_against_the_public_key() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("k.pem");
    let public = dir.path().join("k.pub");
    let made = Command::new("openssl")
        .args(["genrsa", "-out"])
        .arg(&key)
        .arg("2048")
        .output();
    let Ok(made) = made else { return };
    if !made.status.success() {
        return;
    }
    let exported = Command::new("openssl")
        .args(["rsa", "-in"])
        .arg(&key)
        .args(["-pubout", "-out"])
        .arg(&public)
        .output()
        .unwrap();
    assert!(exported.status.success());
    let input = jwt_signing_input(7, 1_700_000_000);
    let signature = sign_rs256(&key, &input).unwrap();
    assert_eq!(signature.len(), 256);
    let sig_file = dir.path().join("sig");
    let msg_file = dir.path().join("msg");
    std::fs::write(&sig_file, &signature).unwrap();
    std::fs::write(&msg_file, &input).unwrap();
    let verified = Command::new("openssl")
        .args(["dgst", "-sha256", "-verify"])
        .arg(&public)
        .arg("-signature")
        .arg(&sig_file)
        .arg(&msg_file)
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stdout)
    );
}

#[test]
fn signing_with_a_missing_key_fails_without_leaking() {
    let err = sign_rs256(Path::new("/nonexistent/key.pem"), "x").unwrap_err();
    assert!(err.to_string().contains("openssl could not sign"));
}

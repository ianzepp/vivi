use crate::b64;

use super::jwt;

fn token_with_exp(exp: i64) -> String {
    let header = b64::encode(br#"{"alg":"none"}"#);
    let payload = b64::encode(format!(r#"{{"exp":{exp}}}"#).as_bytes());
    format!("{header}.{payload}.sig")
}

#[test]
fn reads_exp() {
    let token = token_with_exp(1_700_000_000);
    assert_eq!(jwt::exp_unix(&token), Some(1_700_000_000));
}

#[test]
fn expires_within_skew() {
    let token = token_with_exp(1_000);
    assert!(jwt::expires_within(&token, 30, 990));
    assert!(!jwt::expires_within(&token, 30, 900));
}

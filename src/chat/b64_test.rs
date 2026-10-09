use super::*;

#[test]
fn encode_known_vectors() {
    assert_eq!(encode(b""), "");
    assert_eq!(encode(b"f"), "Zg==");
    assert_eq!(encode(b"fo"), "Zm8=");
    assert_eq!(encode(b"foo"), "Zm9v");
    assert_eq!(encode(b"foob"), "Zm9vYg==");
    assert_eq!(encode(b"fooba"), "Zm9vYmE=");
    assert_eq!(encode(b"foobar"), "Zm9vYmFy");
}

#[test]
fn decode_round_trips_and_rejects_garbage() {
    for sample in [
        "",
        "f",
        "fo",
        "foo",
        "foob",
        "fooba",
        "foobar",
        "challenge-bytes-32!!",
    ] {
        assert_eq!(
            decode(&encode(sample.as_bytes())).as_deref(),
            Some(sample.as_bytes())
        );
    }
    assert_eq!(decode("Zm9vYmFy").as_deref(), Some(b"foobar" as &[u8]));
    assert_eq!(decode("Zg==").as_deref(), Some(b"f" as &[u8]));
    assert_eq!(decode("Zm9vYg==").as_deref(), Some(b"foob" as &[u8]));
    assert_eq!(decode("Z=9v"), None);
    assert_eq!(
        decode("Zm9v="),
        None,
        "pad must be a multiple the chunk rule accepts"
    );
    assert_eq!(decode("Zm9"), None, "length must be a multiple of 4");
    assert_eq!(decode("===="), None);
    assert_eq!(decode("Zm9*"), None);
    assert_eq!(decode("Zg=Z"), None);
    assert_eq!(decode("Z=g="), None);
}

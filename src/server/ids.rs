//! Room ids: 25 Crockford-base32 chars (~125 bits), grouped in fives
//! (`xxxxx-xxxxx-xxxxx-xxxxx-xxxxx`) per docs/contract.md.
//!
//! Canonical ids are lowercase and use only the unambiguous Crockford
//! digits (no `i`, `l`, `o`, `u`). Parsing accepts Crockford's homoglyph
//! aliases: uppercase input is lowercased, `i`/`l` read as `1`, `o` reads
//! as `0`, and `U`/`u` is rejected.

use getrandom::fill;

/// 32 unambiguous Crockford digits, lowercase.
const CANONICAL_ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
/// Body length in chars; grouped form adds four dashes.
pub const ROOM_ID_LEN: usize = 25;

#[derive(Debug, PartialEq, Eq)]
pub enum RoomIdError {
    /// Entropy source failure while minting.
    Mint,
    /// Not a canonical room id (length, charset, grouping, or `U`).
    Invalid,
}

/// Mint a fresh grouped room id from 125 bits of OS randomness.
pub fn mint_room_id() -> Result<String, RoomIdError> {
    let mut bytes = [0u8; 16];
    fill(&mut bytes).map_err(|_| RoomIdError::Mint)?;
    Ok(group(&encode_body(&bytes)))
}

/// The project id of a mailspace known only by its id: a canonical
/// hyphenated UUID is 128 bits, and the first 125 of them are the 25
/// Crockford digits of a project id, the same encoding a minted id uses.
/// `None` when `uuid` is not 32 hex digits in 8-4-4-4-12 form.
pub fn room_id_from_uuid(uuid: &str) -> Option<String> {
    let hex: String = uuid.chars().filter(|c| *c != '-').collect();
    let dashes_ok = uuid.len() == 36
        && [8, 13, 18, 23]
            .iter()
            .all(|i| uuid.as_bytes().get(*i) == Some(&b'-'));
    if hex.len() != 32 || !dashes_ok || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(group(&encode_body(&bytes)))
}

/// Canonicalize a raw id: case-fold, map `i`/`l`→`1` and `o`→`0`, reject
/// `u`, validate length and charset, and regroup in fives. Accepts grouped
/// (5x5, four dashes) or ungrouped 25-char input.
pub fn normalize_room_id(raw: &str) -> Result<String, RoomIdError> {
    if raw.contains('-') {
        let groups: Vec<&str> = raw.split('-').collect();
        if groups.len() != 5 || groups.iter().any(|g| g.len() != 5) {
            return Err(RoomIdError::Invalid);
        }
    }
    let lowered: String = raw
        .chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if lowered.chars().count() != ROOM_ID_LEN {
        return Err(RoomIdError::Invalid);
    }
    let mut canonical = String::with_capacity(ROOM_ID_LEN);
    for c in lowered.chars() {
        let digit = match c {
            'i' | 'l' => '1',
            'o' => '0',
            c if CANONICAL_ALPHABET.contains(&(c as u8)) => c,
            _ => return Err(RoomIdError::Invalid),
        };
        canonical.push(digit);
    }
    Ok(group(&canonical))
}

/// Insert dashes every five chars.
fn group(body: &str) -> String {
    let mut out = String::with_capacity(ROOM_ID_LEN + 4);
    for (i, c) in body.chars().enumerate() {
        if i > 0 && i % 5 == 0 {
            out.push('-');
        }
        out.push(c);
    }
    out
}

/// Encode the first 125 bits of 16 random bytes as 25 Crockford chars.
fn encode_body(bytes: &[u8; 16]) -> String {
    let mut padded = [0u8; 17];
    padded[..16].copy_from_slice(bytes);
    (0..ROOM_ID_LEN)
        .map(|i| {
            let bit = i * 5;
            let window = u16::from(padded[bit / 8]) << 8 | u16::from(padded[bit / 8 + 1]);
            let idx = (window >> (16 - 5 - bit % 8)) & 0x1f;
            CANONICAL_ALPHABET[idx as usize] as char
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn minted_ids_are_grouped_lowercase_crockford() {
        for _ in 0..64 {
            let id = mint_room_id().unwrap();
            let groups: Vec<&str> = id.split('-').collect();
            assert_eq!(groups.len(), 5, "id {id} must have five groups");
            assert!(
                groups.iter().all(|g| g.len() == 5),
                "id {id} groups of five"
            );
            for c in id.chars().filter(|c| *c != '-') {
                assert!(CANONICAL_ALPHABET.contains(&(c as u8)), "id {id} char {c}");
            }
        }
    }

    #[test]
    fn uuid_maps_to_a_canonical_room_id() {
        let id = room_id_from_uuid("3f9a1c7e-0b2d-4c5e-8a1f-123456789abc").unwrap();
        assert_eq!(normalize_room_id(&id).unwrap(), id);
        assert_eq!(
            room_id_from_uuid("3f9a1c7e-0b2d-4c5e-8a1f-123456789abc").unwrap(),
            id
        );
        assert_eq!(room_id_from_uuid("not-a-uuid"), None);
    }

    #[test]
    fn minted_ids_are_entropically_distinct() {
        let ids: HashSet<String> = (0..256).map(|_| mint_room_id().unwrap()).collect();
        assert_eq!(ids.len(), 256, "collision in 256 mints is not plausible");
    }

    #[test]
    fn normalize_applies_contract_aliases() {
        let id = mint_room_id().unwrap();
        assert_eq!(normalize_room_id(&id).unwrap(), id);
        let aliased = id.replace('1', "L").replace('0', "O");
        assert_eq!(normalize_room_id(&aliased).unwrap(), id);
        assert_eq!(normalize_room_id(&id.to_uppercase()).unwrap(), id);
    }

    #[test]
    fn normalize_rejects_bad_ids() {
        assert_eq!(normalize_room_id("u").unwrap_err(), RoomIdError::Invalid);
        assert_eq!(normalize_room_id("U").unwrap_err(), RoomIdError::Invalid);
        assert_eq!(
            normalize_room_id("2222-22222-22222-22222-22222").unwrap_err(),
            RoomIdError::Invalid
        );
        let id = mint_room_id().unwrap();
        assert_eq!(
            normalize_room_id(&id[..id.len() - 1]).unwrap_err(),
            RoomIdError::Invalid
        );
        assert_eq!(
            normalize_room_id("zzzzz!zzzz-zzzzz-zzzzz-zzzzz").unwrap_err(),
            RoomIdError::Invalid
        );
    }
}

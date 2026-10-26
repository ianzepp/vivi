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

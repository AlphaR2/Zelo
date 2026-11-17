//! The proof checks a transaction's Ed25519 signatures itself, because replay does not
//! always do it: the components before an UpdateParent marker (fast leader handover) are
//! never replayed yet are covered by the block_id. Without this check, an unsigned
//! transaction "from" any key could sit there and be proven.

use {
    alpenglow_core::block,
    serde_json::Value,
};

fn json(file: &str) -> Value {
    let p = format!("{}/../../fixtures/local/{file}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn real_raw() -> Vec<u8> {
    hex::decode(json("tx_46718.json")["raw"].as_str().unwrap()).unwrap()
}

/// An entry batch (a component) holding one entry with `txs` transactions, in the format
/// `block::find_in_component` reads: count u64 | (num_hashes u64 | hash | tx_count u64 | txs)*.
fn component(txs: &[&[u8]]) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&1u64.to_le_bytes());
    c.extend_from_slice(&1u64.to_le_bytes());
    c.extend_from_slice(&[0u8; 32]);
    c.extend_from_slice(&(txs.len() as u64).to_le_bytes());
    for t in txs {
        c.extend_from_slice(t);
    }
    c
}

fn first_sig(raw: &[u8]) -> [u8; 64] {
    // compact-u16 signature count (1 here), then the signature.
    assert_eq!(raw[0], 1);
    raw[1..65].try_into().unwrap()
}

fn check(raw: &[u8]) -> Result<(), alpenglow_core::tx::Error> {
    let comp = component(&[raw]);
    let (_, _, tx) = block::find_in_component(&comp, &first_sig(raw)).unwrap().unwrap();
    tx.verify_signatures()
}

#[test]
fn the_real_transaction_verifies() {
    assert_eq!(check(&real_raw()), Ok(()));
}

#[test]
fn a_changed_signature_is_refused() {
    let mut raw = real_raw();
    raw[1 + 10] ^= 1; // a byte inside the signature
    assert_eq!(check(&raw), Err(alpenglow_core::tx::Error::BadSignature(0)));
}

#[test]
fn an_unsigned_transaction_from_a_real_key_is_refused() {
    // What a leader could plant in a never-replayed prefix: the real message (so it names
    // the controller as signer) with a made-up signature.
    let mut raw = real_raw();
    for b in &mut raw[1..65] {
        *b = 0;
    }
    assert_eq!(check(&raw), Err(alpenglow_core::tx::Error::BadSignature(0)));
    for b in &mut raw[1..65] {
        *b = 0xAB;
    }
    assert_eq!(check(&raw), Err(alpenglow_core::tx::Error::BadSignature(0)));
}

#[test]
fn a_signature_over_another_message_is_refused() {
    // Change a byte of the message (the memo's last byte): the old signature no longer fits.
    let mut raw = real_raw();
    let n = raw.len();
    raw[n - 1] ^= 1;
    assert_eq!(check(&raw), Err(alpenglow_core::tx::Error::BadSignature(0)));
}

#[test]
fn a_signature_from_another_key_is_refused() {
    // Swap the signer's account key (the first key, right after the 3-byte header and the
    // key count) for a different valid point: the signature was made by the first key.
    let mut raw = real_raw();
    let at = 1 + 64 + 3 + 1; // sigs count + sig + header + account count
    let other: [u8; 32] = raw[at + 32..at + 64].try_into().unwrap();
    raw[at..at + 32].copy_from_slice(&other);
    assert_eq!(check(&raw), Err(alpenglow_core::tx::Error::BadSignature(0)));
}

#[test]
fn a_key_that_is_not_a_curve_point_is_refused() {
    let mut raw = real_raw();
    let at = 1 + 64 + 3 + 1;
    // y = 2^255 - 18 is not the y of any point on the curve.
    let mut bad = [0xFFu8; 32];
    bad[0] = 0xED;
    bad[31] = 0x7F;
    raw[at..at + 32].copy_from_slice(&bad);
    assert!(check(&raw).is_err());
}

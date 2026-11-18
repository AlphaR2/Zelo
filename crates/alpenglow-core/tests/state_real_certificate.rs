//! A state claim certified by the chain's own certificate, on real data: the block (slot 2103 of
//! a local Alpenglow chain) where the fill program paid a merchant and the outbox recorded the
//! order, and the certificate for slot 2105 from the chain's own footers (`ledger-dump`).

use {
    alpenglow_core::{
        fixture::{self, Certifier},
        proof::{Error, validator_set_hash},
    },
    serde_json::Value,
};

fn load(name: &str) -> Value {
    let path = format!("{}/../../fixtures/local/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// The shard emitter account the fill wrote, and the outbox program that owns it.
const EMITTER: &str = "bbc286f64d77857febcbb2ee1503a754a18d7161a28131a5cc6e642312aea05f";
const OUTBOX: &str = "ba881b6f33cb5d892df0e9d0ad04bc70135aff316e51a13748c3a922f67156ab";

#[test]
fn the_chains_own_certificate_proves_the_fill_blocks_change_set() {
    let (state, block) = (load("state_2103.json"), load("block_2105.json"));
    let input = fixture::state_claim(&state, 2, 2, Certifier::Real(&block));
    let out = input
        .verify()
        .expect("the real certificate and witness verify");
    assert_eq!(out.finality.slot, 2105, "certified block");
    assert_eq!(out.slot, 2103, "the block the claim is about");
    assert_eq!(
        out.finality.validator_set_hash,
        validator_set_hash(&input.finality.validators),
        "the set hash is the real set's commitment"
    );
    let claim = out.state.expect("a change-set claim");
    assert_eq!((claim.leaf_count, claim.confirmations), (12, 2));
    // The outbox's emitter account is among the changes, owned by the outbox after the block.
    let witness = input.state.as_ref().unwrap();
    let emitter = witness
        .changes
        .iter()
        .find(|c| hex::encode(c.pubkey) == EMITTER)
        .expect("the emitter account changed");
    assert_eq!(hex::encode(emitter.post.as_ref().unwrap().owner), OUTBOX);
}

#[test]
fn a_changed_certificate_is_refused() {
    let (state, mut block) = (load("state_2103.json"), load("block_2105.json"));
    let sig = block["block_final_cert"]["final_aggregate"]["signature"]
        .as_str()
        .unwrap()
        .to_string();
    let flipped = format!(
        "{}{}",
        &sig[..sig.len() - 1],
        if sig.ends_with('0') { '1' } else { '0' }
    );
    block["block_final_cert"]["final_aggregate"]["signature"] = Value::String(flipped);
    let input = fixture::state_claim(&state, 2, 2, Certifier::Real(&block));
    assert!(
        matches!(input.verify(), Err(Error::Finality(_))),
        "a forged signature fails"
    );
}

#[test]
fn the_certificate_must_lead_down_to_the_block() {
    // Claiming slot 2103 with only one descendant expects a certificate for slot 2104: the
    // file for 2105 is refused before anything is proven.
    let (state, block) = (load("state_2103.json"), load("block_2105.json"));
    let r =
        std::panic::catch_unwind(|| fixture::state_claim(&state, 1, 1, Certifier::Real(&block)));
    assert!(r.is_err());
}

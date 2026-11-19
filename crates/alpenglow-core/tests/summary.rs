//! The summary claim (tag 3): the same proof as a transaction claim, but the destination gets
//! the transaction's signers and a hash of each instruction's data instead of its bytes.
//!
//! Real data: the memo transfer of local slot 46718 (a system transfer plus a memo).

use {
    alpenglow_core::{
        fixture,
        proof::{ClaimInput, Error},
    },
    serde_json::Value,
    sha2::{Digest, Sha256},
};

fn json(file: &str) -> Value {
    let p = format!("{}/../../fixtures/local/{file}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn input(summarize: bool) -> ClaimInput {
    let sig: [u8; 64] = hex::decode(json("tx_46718.json")["signature"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let tx = fixture::tx_input(&json("block_46718.json"), sig);
    ClaimInput {
        finality: tx.finality,
        ancestry: tx.ancestry,
        tx: Some(tx.inclusion),
        state: None,
        summarize,
    }
}

const SYSTEM_PROGRAM: [u8; 32] = [0; 32];

#[test]
fn a_summary_shows_signers_and_data_hashes_not_bytes() {
    let full = input(false).verify().unwrap();
    let out = input(true).verify().unwrap();
    assert!(
        out.tx.is_none(),
        "the summary replaces the transaction claim"
    );
    let s = out.summary.as_ref().unwrap();
    let tx = full.tx.as_ref().unwrap();

    assert_eq!(s.signature, tx.raw[1..65].to_vec(), "first signature");
    assert_eq!(
        (s.component_start_shred, s.entry_index, s.tx_index),
        (tx.component_start_shred, tx.entry_index, tx.tx_index)
    );
    assert_eq!(
        s.signers.len(),
        1,
        "a memo transfer has one signer, its payer"
    );
    assert_eq!(s.instructions.len(), 2, "a system transfer and a memo");
    assert_eq!(s.instructions[0].program_id, SYSTEM_PROGRAM);
    // The memo's data is the memo text; the claim holds only its length and hash.
    let memo = json("tx_46718.json")["memo"].as_str().map(str::to_owned);
    if let Some(m) = memo {
        assert_eq!(s.instructions[1].data_len as usize, m.len());
        assert_eq!(
            s.instructions[1].data_hash,
            <[u8; 32]>::from(Sha256::digest(m.as_bytes()))
        );
    }
    // The signer is a key inside the transaction bytes.
    assert!(tx.raw.windows(32).any(|w| w == s.signers[0]));
}

#[test]
fn the_summary_encoding_has_the_documented_layout() {
    let full = input(false).verify().unwrap().encode();
    let pv = input(true).verify().unwrap().encode();
    // Same header: same block, same certificate.
    assert_eq!(pv[..144], full[..144]);
    assert_eq!((pv[144], pv[145]), (1, 3), "one claim, tag 3");
    let s = input(true).verify().unwrap().summary.unwrap();
    let mut p = 146 + 12;
    assert_eq!(pv[p..p + 64], s.signature[..]);
    p += 64;
    assert_eq!(pv[p] as usize, s.signers.len());
    p += 1 + 32 * s.signers.len();
    assert_eq!(pv[p] as usize, s.instructions.len());
    p += 1;
    for ix in &s.instructions {
        assert_eq!(pv[p..p + 32], ix.program_id);
        assert_eq!(u16::from_be_bytes([pv[p + 32], pv[p + 33]]), ix.data_len);
        assert_eq!(pv[p + 34..p + 66], ix.data_hash);
        p += 66;
    }
    assert_eq!(p, pv.len(), "nothing after the last instruction");
    // The full transaction never appears in the summary's output.
    let tx_len = u16::from_be_bytes([full[158], full[159]]) as usize;
    let raw = &full[160..160 + tx_len];
    assert!(!pv.windows(raw.len()).any(|w| w == raw));
}

#[test]
fn a_summary_needs_a_transaction() {
    let mut i = input(true);
    i.tx = None;
    i.state = None;
    assert_eq!(i.verify().unwrap_err(), Error::NoClaims);
}

#[test]
fn a_summary_of_a_changed_transaction_is_refused() {
    // The summary goes through the same inclusion checks: one wrong signature byte and the
    // transaction is not found.
    let mut i = input(true);
    i.tx.as_mut().unwrap().signature[0] ^= 1;
    assert!(i.verify().is_err());
}

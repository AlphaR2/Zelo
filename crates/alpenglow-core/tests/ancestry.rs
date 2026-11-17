//! A transaction in a block that has no certificate of its own: proven through a certified
//! descendant, by walking the parent links the block_ids commit to.
//!
//! Real data: the transaction of local slot 46718 and its real inclusion. The descendants
//! are built here (their FEC roots are made up; a block_id only needs *some* roots) and
//! certified by synthetic validators, since the local chain has one validator.

use {
    alpenglow_core::{
        fixture,
        proof::{self, AncestryLink, Error, MAX_ANCESTRY, TxInput},
        shred::{self, Hash},
    },
    serde_json::Value,
};

const SLOT_B: u64 = 46718;

fn json(file: &str) -> Value {
    let p = format!("{}/../../fixtures/local/{file}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn real_tx() -> TxInput {
    let sig: [u8; 64] = hex::decode(json("tx_46718.json")["signature"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    fixture::tx_input(&json("block_46718.json"), sig)
}

fn roots(seed: u8) -> Vec<Hash> {
    vec![[seed; 32], [seed.wrapping_add(1); 32]]
}

/// B (slot 46718, real) <- M (46719) <- D (46720), D certified by `n` synthetic validators.
/// Returns the input and the ids of M and D.
fn chain() -> (TxInput, Hash, Hash) {
    let real = real_tx();
    let id_b = shred::block_id(
        &real.inclusion.fec_roots,
        real.inclusion.parent_slot,
        &real.inclusion.parent_block_id,
    )
    .unwrap();
    let roots_m = roots(10);
    let id_m = shred::block_id(&roots_m, SLOT_B, &id_b).unwrap();
    let roots_d = roots(20);
    let id_d = shred::block_id(&roots_d, SLOT_B + 1, &id_m).unwrap();

    let mut base = real.finality.clone();
    base.slot = SLOT_B + 2;
    base.block_id = id_d;
    let input = TxInput {
        finality: fixture::synthetic(&base, 7),
        ancestry: vec![
            AncestryLink {
                fec_roots: roots_d,
                parent_slot: SLOT_B + 1,
                parent_block_id: id_m,
            },
            AncestryLink {
                fec_roots: roots_m,
                parent_slot: SLOT_B,
                parent_block_id: id_b,
            },
        ],
        inclusion: real.inclusion,
    };
    (input, id_m, id_d)
}

#[test]
fn a_transaction_two_blocks_below_the_certified_block_is_proven() {
    let (input, _, id_d) = chain();
    let out = input.verify().unwrap();
    assert_eq!(
        (out.finality.slot, out.finality.block_id),
        (SLOT_B + 2, id_d)
    );
    assert_eq!(out.tx_slot, SLOT_B);
    let rpc = json("tx_46718.json");
    assert_eq!(hex::encode(&out.tx.raw), rpc["raw"].as_str().unwrap());

    // The public values name both blocks: D (certified) and B (holding the transaction).
    let pv = out.encode();
    assert_eq!(
        u64::from_be_bytes(pv[35..43].try_into().unwrap()),
        SLOT_B + 2
    );
    assert_eq!(&pv[43..75], &id_d);
    assert_eq!(u64::from_be_bytes(pv[92..100].try_into().unwrap()), SLOT_B);
    assert_eq!(&pv[100..132], &out.tx_block_id);
}

#[test]
fn no_links_means_the_certified_block_holds_the_transaction() {
    let input = real_tx();
    let out = input.verify().unwrap();
    assert_eq!(out.tx_slot, out.finality.slot);
    assert_eq!(out.tx_block_id, out.finality.block_id);
}

#[test]
fn a_changed_root_in_any_link_fails() {
    for link in 0..2 {
        let (mut input, _, _) = chain();
        input.ancestry[link].fec_roots[0][0] ^= 1;
        assert!(
            matches!(input.verify().unwrap_err(), Error::BlockIdMismatch { .. }),
            "link {link}"
        );
    }
}

#[test]
fn a_changed_parent_in_any_link_fails() {
    let (mut input, _, _) = chain();
    input.ancestry[0].parent_block_id[0] ^= 1;
    assert!(matches!(
        input.verify().unwrap_err(),
        Error::BlockIdMismatch { .. }
    ));
    let (mut input, _, _) = chain();
    input.ancestry[1].parent_slot -= 1;
    assert!(matches!(
        input.verify().unwrap_err(),
        Error::BlockIdMismatch { .. }
    ));
}

#[test]
fn links_in_the_wrong_order_fail() {
    let (mut input, _, _) = chain();
    input.ancestry.swap(0, 1);
    assert!(matches!(
        input.verify().unwrap_err(),
        Error::BlockIdMismatch { .. }
    ));
}

#[test]
fn a_walk_that_stops_early_does_not_reach_the_transactions_block() {
    // Only M is reached; the inclusion is against B, so its block_id doesn't match.
    let (mut input, _, _) = chain();
    input.ancestry.truncate(1);
    assert!(matches!(
        input.verify().unwrap_err(),
        Error::BlockIdMismatch { .. }
    ));
    // And with the links dropped altogether, D is not the block holding the transaction.
    let (mut input, _, _) = chain();
    input.ancestry.clear();
    assert!(matches!(
        input.verify().unwrap_err(),
        Error::BlockIdMismatch { .. }
    ));
}

#[test]
fn an_extra_link_past_the_transactions_block_fails() {
    // B's own parent is genuine (the real block's), but the transaction's block is B, not it.
    let (mut input, _, _) = chain();
    let real = real_tx();
    input.ancestry.push(AncestryLink {
        fec_roots: real.inclusion.fec_roots.clone(),
        parent_slot: real.inclusion.parent_slot,
        parent_block_id: real.inclusion.parent_block_id,
    });
    assert!(matches!(
        input.verify().unwrap_err(),
        Error::NotInBlock(_) | Error::BlockIdMismatch { .. }
    ));
}

#[test]
fn a_parent_slot_that_does_not_go_down_is_refused() {
    // A self-consistent chain whose link names its own slot (or a later one) as the parent.
    let real = real_tx();
    let id_b = shred::block_id(
        &real.inclusion.fec_roots,
        real.inclusion.parent_slot,
        &real.inclusion.parent_block_id,
    )
    .unwrap();
    for parent_slot in [SLOT_B + 1, SLOT_B + 5] {
        let roots_d = roots(30);
        let id_d = shred::block_id(&roots_d, parent_slot, &id_b).unwrap();
        let mut base = real.finality.clone();
        base.slot = SLOT_B + 1;
        base.block_id = id_d;
        let input = TxInput {
            finality: fixture::synthetic(&base, 7),
            ancestry: vec![AncestryLink {
                fec_roots: roots_d,
                parent_slot,
                parent_block_id: id_b,
            }],
            inclusion: real.inclusion.clone(),
        };
        assert_eq!(
            input.verify().unwrap_err(),
            Error::AncestryOrder {
                slot: SLOT_B + 1,
                parent_slot
            },
            "parent slot {parent_slot}"
        );
    }
}

#[test]
fn a_wrong_slot_for_the_transactions_block_fails() {
    // The chain commits to B being slot 46717 (the shreds say 46718), consistently: only the
    // inclusion check, which reads the slot from the shreds themselves, can catch it.
    let real = real_tx();
    let id_b = shred::block_id(
        &real.inclusion.fec_roots,
        real.inclusion.parent_slot,
        &real.inclusion.parent_block_id,
    )
    .unwrap();
    let roots_d = roots(40);
    let id_d = shred::block_id(&roots_d, SLOT_B - 1, &id_b).unwrap();
    let mut base = real.finality.clone();
    base.slot = SLOT_B + 1;
    base.block_id = id_d;
    let input = TxInput {
        finality: fixture::synthetic(&base, 7),
        ancestry: vec![AncestryLink {
            fec_roots: roots_d,
            parent_slot: SLOT_B - 1,
            parent_block_id: id_b,
        }],
        inclusion: real.inclusion,
    };
    assert!(matches!(input.verify().unwrap_err(), Error::NotInBlock(_)));
}

#[test]
fn too_many_links_are_refused() {
    let id = [9u8; 32];
    let link = |i: u64| AncestryLink {
        fec_roots: roots(1),
        parent_slot: 1_000 - i,
        parent_block_id: id,
    };
    let links: Vec<_> = (0..=MAX_ANCESTRY as u64).map(link).collect();
    assert_eq!(
        proof::verify_ancestry(2_000, &id, &links).unwrap_err(),
        Error::AncestryTooLong(MAX_ANCESTRY + 1)
    );
}

#[test]
fn an_uncertified_child_cannot_vouch_for_a_forged_parent() {
    // D is certified, but its block_id names the real M; a link that swaps in a different
    // parent changes the rebuilt id, so a forged ancestor can't be slipped in.
    let (mut input, _, _) = chain();
    input.ancestry[0].parent_block_id = [0xEE; 32];
    assert!(matches!(
        input.verify().unwrap_err(),
        Error::BlockIdMismatch { .. }
    ));
}

// ---------- A real devnet block that never got its own certificate ----------

/// Slot 522,347,550 on devnet (15 Nov 2026): no footer ever certified it. Slot 522,347,551's
/// footer carries the certificate for 551, and 551's block_id names 550 as its parent. The
/// transaction is a 479-byte memo from a 3-of-5 council (a failed first attempt at the demo
/// stopped here: "no footer within 24 blocks of 522347550 carries its certificate").
#[test]
fn a_real_devnet_transaction_in_a_block_with_no_certificate_of_its_own() {
    let f = json_devnet("block_522347550.json");
    let sig: [u8; 64] = hex::decode(
        "1168a2526c78137b2370a113993f9534d25ca06d6381ff252f00fdacc5c6aa1cb9da4fbf5b782256cc1cf9e46558662571222aeb38b32d58738a68f6c17f3b02",
    )
    .unwrap()
    .try_into()
    .unwrap();
    let input = fixture::tx_input(&f, sig);
    assert_eq!(input.ancestry.len(), 1, "551 is one link above 550");
    let out = input.verify().unwrap();
    assert_eq!(out.finality.slot, 522_347_551);
    assert_eq!(out.tx_slot, 522_347_550);
    assert_eq!(out.tx.raw.len(), 479);
    // Without the link, the certificate says nothing about 550.
    let mut broken = input.clone();
    broken.ancestry.clear();
    assert!(matches!(
        broken.verify().unwrap_err(),
        Error::BlockIdMismatch { .. }
    ));
}

fn json_devnet(file: &str) -> Value {
    let p = format!(
        "{}/../../fixtures/devnet/{file}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

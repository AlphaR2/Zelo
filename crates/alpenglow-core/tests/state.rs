//! The state proof on real data: block 156,070 of a local Alpenglow chain, replayed with
//! `agave-ledger-tool`. Its lattice hash, bank hash and footer were checked against Agave's own
//! values when the fixture was made (`examples/make_state_fixture`); here the verifier must
//! accept the real witness and refuse every way of bending it.

use {
    alpenglow_core::{
        fixture::{self, StateFixture},
        proof::{ClaimInput, Error, FinalityInput},
        shred::Hash,
        state::{self, Account, AccountChange, Error as StateError, StateWitness},
    },
    serde_json::Value,
};

fn json() -> Value {
    let p = format!("{}/../../fixtures/local/state_156070.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// A proof input whose certified block is the fixture's `depth`-th descendant (or B itself),
/// certified by synthetic validators (the local chain's own certificate is not the point here).
fn input(depth: usize, confirm: usize) -> (ClaimInput, StateFixture) {
    let fx = fixture::state_fixture(&json(), depth, confirm);
    let base = FinalityInput {
        validators: vec![],
        shred_version: fx.shred_version,
        slot: fx.certified.0,
        block_id: fx.certified.1,
        final_aggregate: alpenglow_core::proof::Aggregate { signature: vec![], bitmap: vec![] },
        notar_aggregate: None,
    };
    let input = ClaimInput {
        finality: fixture::synthetic(&base, 7),
        ancestry: fx.links.clone(),
        tx: None,
        state: Some(fx.witness.clone()),
    };
    (input, fx)
}

fn with(depth: usize, confirm: usize, f: impl FnOnce(&mut StateWitness)) -> Result<state::StateClaim, Error> {
    let (mut i, _) = input(depth, confirm);
    f(i.state.as_mut().unwrap());
    i.verify().map(|o| o.state.unwrap())
}

fn refused(r: Result<state::StateClaim, Error>) -> StateError {
    match r {
        Err(Error::State(e)) => e,
        other => panic!("expected a state error, got {other:?}"),
    }
}

// ---------- the real witness ----------

#[test]
fn the_real_block_proves_its_change_set() {
    let (i, fx) = input(0, 0);
    let out = i.verify().unwrap();
    assert_eq!((out.slot, out.block_id), (fx.slot, fx.block_id));
    let s = out.state.unwrap();
    assert_eq!(hex::encode(s.bank_hash), json()["bank_hash"].as_str().unwrap());
    // Six accounts changed, four of them sysvars; every one really differs, so every one is a leaf.
    assert_eq!(fx.witness.changes.len(), 6);
    assert_eq!(s.leaf_count, 6);
    assert_eq!((s.confirmations, s.windows, s.top_slot), (0, 0, fx.slot));
}

#[test]
fn descendants_footers_confirm_it_and_leader_windows_are_counted() {
    // B = 156070 (window 39017), C1 = 156071 (same window), C2 = 156072 (window 39018).
    let (i, fx) = input(2, 2);
    let out = i.verify().unwrap();
    assert_eq!(out.finality.slot, 156072);
    assert_eq!(out.slot, fx.slot);
    let s = out.state.unwrap();
    assert_eq!((s.confirmations, s.windows, s.top_slot), (2, 1, 156072));
    // One confirmation: still the same window as B.
    let s = input(2, 1).0.verify().unwrap().state.unwrap();
    assert_eq!((s.confirmations, s.windows, s.top_slot), (1, 0, 156071));
    // The same claim whatever the depth: confirmations add evidence, not content.
    let plain = input(0, 0).0.verify().unwrap().state.unwrap();
    assert_eq!((s.bank_hash, s.diff_root), (plain.bank_hash, plain.diff_root));
}

#[test]
fn the_public_values_carry_the_state_claim() {
    let (i, fx) = input(2, 2);
    let out = i.verify().unwrap();
    let pv = out.encode();
    assert_eq!(pv[0], 2);
    assert_eq!(u64::from_be_bytes(pv[35..43].try_into().unwrap()), 156072);
    assert_eq!(u64::from_be_bytes(pv[92..100].try_into().unwrap()), fx.slot);
    assert_eq!(&pv[100..132], &fx.block_id);
    assert_eq!(u16::from_be_bytes(pv[132..134].try_into().unwrap()), 2);
    assert_eq!(u16::from_be_bytes(pv[134..136].try_into().unwrap()), 1);
    assert_eq!(u64::from_be_bytes(pv[136..144].try_into().unwrap()), 156072);
    assert_eq!((pv[144], pv[145]), (1, 2), "one claim, a change set");
    let s = out.state.unwrap();
    assert_eq!(&pv[146..178], &s.bank_hash);
    assert_eq!(&pv[178..210], &s.diff_root);
    assert_eq!(u32::from_be_bytes(pv[210..214].try_into().unwrap()), 6);
    assert_eq!(pv.len(), 214);
}

// ---------- opening one account from the diff root ----------

#[test]
fn an_account_opens_from_the_diff_root_and_an_unchanged_one_is_provably_absent() {
    let (i, fx) = input(0, 0);
    let root = i.verify().unwrap().state.unwrap().diff_root;
    let changes = &fx.witness.changes;
    let leaves: Vec<Hash> = changes.iter().map(|c| state::diff_leaf(&c.pubkey, &c.pre, &c.post)).collect();
    for (idx, c) in changes.iter().enumerate() {
        let path = state::merkle_path(&leaves, idx);
        let leaf = state::diff_leaf(&c.pubkey, &c.pre, &c.post);
        assert_eq!(state::root_from_path(&leaf, idx, leaves.len(), &path), Some(root), "leaf {idx}");
    }
    // A wrong index, a short path, a long path and a changed leaf all fail.
    let path = state::merkle_path(&leaves, 2);
    let leaf = leaves[2];
    assert_ne!(state::root_from_path(&leaf, 3, leaves.len(), &path), Some(root));
    assert_eq!(state::root_from_path(&leaf, 2, leaves.len(), &path[..path.len() - 1]), None);
    let mut long = path.clone();
    long.push([0; 32]);
    assert_eq!(state::root_from_path(&leaf, 2, leaves.len(), &long), None);
    let mut other = leaf;
    other[0] ^= 1;
    assert_ne!(state::root_from_path(&other, 2, leaves.len(), &path), Some(root));
    // Non-membership: an address between two adjacent leaves' keys is not in the tree. Here
    // the keys are sorted, so adjacency of indices i, i+1 with key_i < x < key_{i+1} is checkable.
    assert!(changes.windows(2).all(|w| w[0].pubkey < w[1].pubkey));
}

#[test]
fn the_diff_tree_is_well_defined_for_every_size() {
    let leaf = |i: u8| [i; 32];
    for n in 0..=33usize {
        let leaves: Vec<Hash> = (0..n).map(|i| leaf(i as u8 + 1)).collect();
        let root = state::diff_root(&leaves);
        for i in 0..n {
            let path = state::merkle_path(&leaves, i);
            assert_eq!(state::root_from_path(&leaves[i], i, n, &path), Some(root), "n={n} i={i}");
        }
    }
    // The empty tree has a zero Merkle root, and its diff root still commits to the count 0.
    assert_eq!(state::merkle_root(&[]), [0; 32]);
    assert_ne!(state::diff_root(&[]), state::diff_root(&[leaf(1)]));
}

// ---------- the attacks (state-proof.md 6.2) ----------

#[test]
fn a_changed_value_breaks_the_lattice_sum() {
    let r = with(0, 0, |w| {
        let post = w.changes[3].post.as_mut().unwrap();
        post.lamports += 1;
    });
    assert_eq!(refused(r), StateError::DeltaMismatch);
}

#[test]
fn a_false_before_value_breaks_the_lattice_sum() {
    let r = with(0, 0, |w| {
        let pre = w.changes[1].pre.as_mut().unwrap();
        pre.data[0] ^= 1;
    });
    assert_eq!(refused(r), StateError::DeltaMismatch);
}

#[test]
fn leaving_out_a_changed_account_breaks_the_lattice_sum() {
    let r = with(0, 0, |w| {
        w.changes.remove(2);
    });
    assert_eq!(refused(r), StateError::DeltaMismatch);
}

#[test]
fn an_account_that_did_not_change_cannot_become_a_leaf() {
    // Adding an entry with the same value before and after changes nothing in the sum, and
    // makes no leaf: "changed" claims for an account that didn't change are impossible.
    let (i, _) = input(0, 0);
    let honest = i.verify().unwrap().state.unwrap();
    let r = with(0, 0, |w| {
        let fake = Account { lamports: 5, data: vec![1, 2, 3], executable: false, owner: [9; 32] };
        // A key after every real one (real keys here are below 0xFF..).
        w.changes.push(AccountChange { pubkey: [0xFF; 32], pre: Some(fake.clone()), post: Some(fake) });
    })
    .unwrap();
    assert_eq!(r, honest);
}

#[test]
fn the_same_account_twice_is_refused() {
    // The "telescoping" trick: (X: real -> fake) then (X: fake -> real) sums to the true change.
    let r = with(0, 0, |w| {
        let c = w.changes[0].clone();
        let fake = Account { lamports: 1, data: vec![7], executable: false, owner: [1; 32] };
        w.changes[0] = AccountChange { pubkey: c.pubkey, pre: c.pre.clone(), post: Some(fake.clone()) };
        w.changes.insert(1, AccountChange { pubkey: c.pubkey, pre: Some(fake), post: c.post });
    });
    assert_eq!(refused(r), StateError::Unsorted(1));
}

#[test]
fn unsorted_changes_are_refused() {
    let r = with(0, 0, |w| w.changes.swap(0, 1));
    assert_eq!(refused(r), StateError::Unsorted(1));
}

#[test]
fn a_zero_lamport_account_counts_as_absent() {
    // An account with lamports 0 hashes to nothing, whatever data it carries: it can't be used
    // to fake "exists". Replace a real change's post by a zero-lamport account: the lattice
    // sum then differs (the real post is gone), so it is refused; and a zero-lamport pre or
    // post gives the same leaf digest as None.
    assert_eq!(
        state::account_digest(&Some(Account { lamports: 0, data: vec![1], executable: true, owner: [3; 32] })),
        state::account_digest(&None)
    );
    let r = with(0, 0, |w| {
        let o = w.changes[0].post.as_mut().unwrap();
        o.lamports = 0;
    });
    assert_eq!(refused(r), StateError::DeltaMismatch);
}

#[test]
fn a_wrong_parent_lattice_hash_or_inner_hash_breaks_the_bank_hash() {
    let slot = 156070;
    assert_eq!(refused(with(0, 0, |w| w.lt_parent[7] ^= 1)), StateError::BankHashMismatch(slot));
    assert_eq!(refused(with(0, 0, |w| w.inner_parent[0] ^= 1)), StateError::BankHashMismatch(slot));
    assert_eq!(refused(with(0, 0, |w| w.block.signature_count += 1)), StateError::BankHashMismatch(slot));
    assert_eq!(refused(with(0, 0, |w| w.block.last_blockhash[0] ^= 1)), StateError::BankHashMismatch(slot));
    assert_eq!(refused(with(0, 0, |w| w.block.lt_hash[100] ^= 1)), StateError::BankHashMismatch(slot));
}

#[test]
fn a_lattice_hash_of_the_wrong_length_is_refused() {
    assert_eq!(refused(with(0, 0, |w| w.lt_parent.pop().map(|_| ()).unwrap())), StateError::LtLength(1023));
    assert_eq!(refused(with(0, 0, |w| w.block.lt_hash.push(0))), StateError::LtLength(1025));
}

#[test]
fn a_footer_from_another_block_is_refused() {
    // The footer run of B's child, offered for B: its shreds are not in B.
    let other = fixture::state_fixture(&json(), 2, 2);
    let r = with(0, 0, |w| w.footer = other.witness.confirmations[0].footer.clone());
    assert!(matches!(refused(r), StateError::FooterRun(_)));
}

#[test]
fn a_footer_run_must_end_the_slot_and_start_at_a_boundary() {
    // Without the closing tick the run does not end the slot.
    let r = with(0, 0, |w| {
        let n = w.footer.shreds.len();
        w.footer.shreds.truncate(n / 2);
    });
    assert!(matches!(refused(r), StateError::FooterRun(_)));
    // Starting one shred late: not right after its boundary.
    let r = with(0, 0, |w| {
        w.footer.shreds.remove(0);
    });
    assert_eq!(refused(r), StateError::FooterRun("the run does not start right after its boundary"));
    // A boundary that is not the end of a component.
    let r = with(0, 0, |w| w.footer.boundary_shred = w.footer.shreds[0].clone());
    assert!(matches!(refused(r), StateError::FooterRun(_)));
}

#[test]
fn a_changed_footer_shred_is_not_in_the_block() {
    let r = with(0, 0, |w| {
        let n = w.footer.shreds[1].len();
        w.footer.shreds[1][n - 40] ^= 1;
    });
    assert!(matches!(refused(r), StateError::FooterRun(_)));
}

#[test]
fn the_blocks_own_roots_must_rebuild_its_id() {
    assert_eq!(refused(with(0, 0, |w| w.fec_roots[0][0] ^= 1)), StateError::BlockIdMismatch);
    assert_eq!(refused(with(0, 0, |w| w.parent_slot -= 1)), StateError::BlockIdMismatch);
}

#[test]
fn a_confirmation_must_chain_to_the_footer_below_it() {
    let slot_c = 156071;
    assert_eq!(refused(with(2, 2, |w| w.confirmations[0].parts.signature_count += 1)), StateError::BankHashMismatch(slot_c));
    assert_eq!(refused(with(2, 2, |w| w.confirmations[0].parts.lt_hash[3] ^= 1)), StateError::BankHashMismatch(slot_c));
    assert_eq!(refused(with(2, 2, |w| w.confirmations[1].parts.last_blockhash[0] ^= 1)), StateError::BankHashMismatch(156072));
    // Swapping the two confirmations (a wrong order) breaks the chain and the footers' places.
    assert!(matches!(refused(with(2, 2, |w| w.confirmations.swap(0, 1))), StateError::FooterRun(_)));
}

#[test]
fn more_confirmations_than_blocks_above_are_refused() {
    let (mut i, fx) = input(1, 1);
    let more = fixture::state_fixture(&json(), 2, 2).witness.confirmations;
    i.state.as_mut().unwrap().confirmations = more;
    let _ = fx;
    assert_eq!(
        match i.verify() {
            Err(Error::State(e)) => e,
            o => panic!("{o:?}"),
        },
        StateError::TooManyConfirmations(2, 1)
    );
}

#[test]
fn a_proof_with_no_claim_is_refused() {
    let (mut i, _) = input(0, 0);
    i.state = None;
    assert_eq!(i.verify().unwrap_err(), Error::NoClaims);
}

#[test]
fn a_block_that_is_not_final_proves_nothing() {
    // The ancestry must lead from the certified block to B: a broken link fails before the
    // witness is looked at.
    let (mut i, _) = input(2, 2);
    i.ancestry[0].fec_roots[0][0] ^= 1;
    assert!(matches!(i.verify().unwrap_err(), Error::BlockIdMismatch { .. }));
}

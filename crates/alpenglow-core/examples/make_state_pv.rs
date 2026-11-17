//! Writes the Solidity test fixture for the StateRegistry from the real state fixture: the
//! public values a state proof would commit, and an opening (digests, index, Merkle path) for
//! every changed account. Accounts under 200 bytes are written out in full so contracts can
//! recompute their digests.
//!
//!   make_state_pv <state_156070.json> <depth> <confirm> <out.json>

use alpenglow_core::{fixture, proof::{Aggregate, ClaimInput, FinalityInput}, state};

fn acct(a: &Option<state::Account>) -> serde_json::Value {
    match a {
        None => serde_json::Value::Null,
        Some(a) => serde_json::json!({"lamports": a.lamports, "data": hex::encode(&a.data), "executable": a.executable, "owner": hex::encode(a.owner)}),
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let f: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&a[1]).unwrap()).unwrap();
    let (depth, confirm): (usize, usize) = (a[2].parse().unwrap(), a[3].parse().unwrap());
    let fx = fixture::state_fixture(&f, depth, confirm);
    let base = FinalityInput {
        validators: vec![],
        shred_version: fx.shred_version,
        slot: fx.certified.0,
        block_id: fx.certified.1,
        final_aggregate: Aggregate { signature: vec![], bitmap: vec![] },
        notar_aggregate: None,
    };
    let finality = fixture::synthetic(&base, 7);
    let input = ClaimInput { finality: finality.clone(), ancestry: fx.links.clone(), tx: None, state: Some(fx.witness.clone()) };
    let out = input.verify().expect("the real witness verifies");
    let claim = out.state.clone().unwrap();
    let leaves: Vec<_> = fx.witness.changes.iter().map(|c| state::diff_leaf(&c.pubkey, &c.pre, &c.post)).collect();
    let openings: Vec<_> = fx
        .witness
        .changes
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let small = c.pre.as_ref().map_or(0, |x| x.data.len()).max(c.post.as_ref().map_or(0, |x| x.data.len())) < 200;
            serde_json::json!({
                "index": i,
                "pubkey": hex::encode(c.pubkey),
                "pre_digest": hex::encode(state::account_digest(&c.pre)),
                "post_digest": hex::encode(state::account_digest(&c.post)),
                "leaf": hex::encode(leaves[i]),
                "path": state::merkle_path(&leaves, i).iter().map(hex::encode).collect::<Vec<_>>(),
                "pre": if small { acct(&c.pre) } else { serde_json::Value::Null },
                "post": if small { acct(&c.post) } else { serde_json::Value::Null },
            })
        })
        .collect();
    let v = serde_json::json!({
        "public_values": hex::encode(out.encode()),
        "validator_set_hash": hex::encode(out.finality.validator_set_hash),
        "shred_version": fx.shred_version,
        "certified_slot": out.finality.slot,
        "slot": out.slot,
        "block_id": hex::encode(out.block_id),
        "bank_hash": hex::encode(claim.bank_hash),
        "diff_root": hex::encode(claim.diff_root),
        "leaf_count": claim.leaf_count,
        "confirmations": claim.confirmations,
        "windows": claim.windows,
        "top_slot": claim.top_slot,
        "openings": openings,
    });
    std::fs::write(&a[4], serde_json::to_string_pretty(&v).unwrap() + "\n").unwrap();
    println!("wrote {} ({} openings)", a[4], leaves.len());
}

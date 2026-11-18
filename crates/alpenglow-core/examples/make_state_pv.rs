//! Writes what the destination chain needs for a state claim: the public values the proof
//! commits, and an opening (digests, index, Merkle path) for every changed account. Accounts
//! under 200 bytes are written out in full so contracts can recompute their digests.
//!
//!   make_state_pv <state_<slot>.json> <depth> <confirm> <out.json> [block file]
//!
//! With a block file (`ledger-dump`, for the top block of the ancestry), the claim is certified
//! by the chain's own certificate; without one, by 7 synthetic validators (test fixtures).

use alpenglow_core::{fixture, state};

fn acct(a: &Option<state::Account>) -> serde_json::Value {
    match a {
        None => serde_json::Value::Null,
        Some(a) => {
            serde_json::json!({"lamports": a.lamports, "data": hex::encode(&a.data), "executable": a.executable, "owner": hex::encode(a.owner)})
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let f: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&a[1]).unwrap()).unwrap();
    let (depth, confirm): (usize, usize) = (a[2].parse().unwrap(), a[3].parse().unwrap());
    // The chain's own certificate from a `ledger-dump` block file, or 7 synthetic validators.
    let block: Option<serde_json::Value> = a
        .get(5)
        .map(|p| serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap());
    let by = match &block {
        Some(b) => fixture::Certifier::Real(b),
        None => fixture::Certifier::Synthetic(7),
    };
    let input = fixture::state_claim(&f, depth, confirm, by);
    let witness = input.state.clone().unwrap();
    let shred_version = input.finality.shred_version;
    let out = input.verify().expect("the witness and certificate verify");
    let claim = out.state.clone().unwrap();
    let leaves: Vec<_> = witness
        .changes
        .iter()
        .map(|c| state::diff_leaf(&c.pubkey, &c.pre, &c.post))
        .collect();
    let openings: Vec<_> = witness
        .changes
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let small = c
                .pre
                .as_ref()
                .map_or(0, |x| x.data.len())
                .max(c.post.as_ref().map_or(0, |x| x.data.len()))
                < 200;
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
        "shred_version": shred_version,
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

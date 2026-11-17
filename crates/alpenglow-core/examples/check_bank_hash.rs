//! S1, offline: do our bank-hash and lattice-hash formulas (src/bank.rs) reproduce what a real
//! Agave validator computed?
//!
//! Inputs, both from `agave-ledger-tool` replaying a ledger to the same slot:
//!   accounts.out   `accounts --halt-at-slot S --include-sysvars` (every account, text format)
//!   details.json   `verify --halt-at-slot S --write-bank-file` (the bank-hash components)
//! and optionally the block's own footer: 64 hex characters, or the `shreds_<slot>.json` that
//! `ledger-dump --export-shreds` writes (the footer is read from its shreds).
//!
//!   cargo run --release -p alpenglow-core --example check_bank_hash -- accounts.out details.json [footer]
//!
//! Checks, each printed PASS or FAIL: the lattice hash we sum from the accounts has Agave's
//! checksum; the bank hash we rebuild equals Agave's; and equals the footer's.

use alpenglow_core::{
    accounts_dump::{key, Dump},
    bank::{bank_hash, inner_hash},
    block::{self, Component, Marker},
    shred::Shred,
};

/// The bank hash in the footer of the block whose data shreds are in a `shreds_<slot>.json`.
fn footer_bank_hash(path: &str) -> Vec<u8> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let raw: Vec<Vec<u8>> = v["data_shreds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| hex::decode(h.as_str().unwrap()).unwrap())
        .collect();
    let shreds: Vec<Shred> = raw.iter().map(|b| Shred::parse(b).unwrap()).collect();
    for c in block::deshred(&shreds).unwrap() {
        if let Component::Marker(Marker::Footer(b)) = block::parse_component(&c).unwrap() {
            return block::parse_footer(b).unwrap().bank_hash.to_vec();
        }
    }
    panic!("no footer in {path}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let footer = args.get(3).map(|a| if a.ends_with(".json") { footer_bank_hash(a) } else { hex::decode(a).expect("footer hex") });
    let mut dump = Dump::parse(&std::fs::read_to_string(&args[1]).expect("accounts file"));
    let d: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&args[2]).unwrap()).unwrap();
    let s = &d["bank_hash_details"][0];
    let h = |k: &str| -> [u8; 32] { key(s[k].as_str().unwrap()) };
    println!("slot {}: {} accounts", s["slot"], dump.accounts.len());

    let (lt, recovered) = dump.lt_hash(&h("accounts_lt_hash_checksum"));
    if let Some(l) = recovered {
        println!("recovered the large account's lamports: {l}");
    }
    let ok = |name: &str, pass: bool| {
        println!("{} {name}", if pass { "PASS" } else { "FAIL" });
        pass
    };
    let mut all = ok("accounts lattice hash checksum equals Agave's", lt.checksum() == h("accounts_lt_hash_checksum"));
    let inner = inner_hash(&h("parent_bank_hash"), s["signature_count"].as_u64().unwrap(), &h("last_blockhash"));
    let ours = bank_hash(&inner, &lt);
    all &= ok("bank hash rebuilt from the formula equals Agave's", ours == h("bank_hash"));
    if let Some(f) = footer {
        all &= ok("bank hash equals the one in the block's own footer", ours.as_slice() == f.as_slice());
    }
    std::process::exit(if all { 0 } else { 1 });
}

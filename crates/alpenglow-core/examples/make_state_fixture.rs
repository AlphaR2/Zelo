//! Builds a real-data state-proof fixture from a local ledger replayed with `agave-ledger-tool`.
//!
//!   make_state_fixture <work dir> <bank_hash_details dir> <slot B> <descendants> <out.json>
//!
//! The work dir holds, for slots B-1 ..= B+descendants: `accounts_<slot>.out`
//! (`accounts --halt-at-slot S --include-sysvars`) and `shreds_<slot>.json`
//! (`ledger-dump --export-shreds`). The details dir is where `verify --write-bank-file` wrote
//! `<slot>-<hash>.json`. Every lattice hash is rebuilt from the accounts and checked against
//! Agave's checksum before anything is written.

use {
    alpenglow_core::{
        accounts_dump::{key, Dump, Parsed},
        bank::{bank_hash, inner_hash, LtHash},
        block::{self, Component, Marker},
        shred::{self, Shred},
    },
    serde_json::{json, Value},
    std::collections::BTreeMap,
};

fn read(p: &str) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn details(dir: &str, slot: u64) -> Value {
    let prefix = format!("{slot}-");
    let f = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.file_name().unwrap().to_str().unwrap().starts_with(&prefix))
        .unwrap_or_else(|| panic!("no details for slot {slot}"));
    serde_json::from_str::<Value>(&read(f.to_str().unwrap())).unwrap()["bank_hash_details"][0].clone()
}

struct Block {
    lt: LtHash,
    accounts: Vec<Parsed>,
    d: Value,
}

fn load(work: &str, dir: &str, slot: u64) -> Block {
    let d = details(dir, slot);
    let mut dump = Dump::parse(&read(&format!("{work}/accounts_{slot}.out")));
    let checksum = key(d["accounts_lt_hash_checksum"].as_str().unwrap());
    let (lt, _) = dump.lt_hash(&checksum);
    assert_eq!(lt.checksum(), checksum, "lattice hash of slot {slot}");
    let inner = inner_hash(
        &key(d["parent_bank_hash"].as_str().unwrap()),
        d["signature_count"].as_u64().unwrap(),
        &key(d["last_blockhash"].as_str().unwrap()),
    );
    assert_eq!(bank_hash(&inner, &lt), key(d["bank_hash"].as_str().unwrap()), "bank hash of slot {slot}");
    Block { lt, accounts: dump.accounts, d }
}

fn hex32(h: &[u8]) -> String {
    hex::encode(h)
}

fn lt_hex(lt: &LtHash) -> String {
    hex::encode(lt.to_bytes())
}

fn account(a: &Parsed) -> Value {
    json!({"lamports": a.lamports, "data": hex::encode(&a.data), "executable": a.executable, "owner": hex32(&a.owner)})
}

/// Shreds of the block, its FEC roots, parent, and the footer run (the last two components).
/// A block's raw data shreds, FEC roots, parent (slot, id) and own block id.
type Shreds = (Vec<Vec<u8>>, Vec<[u8; 32]>, (u64, [u8; 32]), [u8; 32]);

fn shreds_of(work: &str, slot: u64) -> Shreds {
    let v: Value = serde_json::from_str(&read(&format!("{work}/shreds_{slot}.json"))).unwrap();
    let raw: Vec<Vec<u8>> = v["data_shreds"].as_array().unwrap().iter().map(|h| hex::decode(h.as_str().unwrap()).unwrap()).collect();
    let parsed: Vec<Shred> = raw.iter().map(|b| Shred::parse(b).unwrap()).collect();
    let roots = shred::fec_roots(slot, &parsed).unwrap();
    let mut parent = None;
    for c in block::deshred(&parsed).unwrap() {
        if let Component::Marker(Marker::Header { parent_slot, parent_block_id }) = block::parse_component(&c).unwrap() {
            parent = Some((parent_slot, parent_block_id));
        }
    }
    let parent = parent.expect("a header marker");
    let id = shred::block_id(&roots, parent.0, &parent.1).unwrap();
    (raw, roots, parent, id)
}

fn footer_run(raw: &[Vec<u8>]) -> Value {
    let parsed: Vec<Shred> = raw.iter().map(|b| Shred::parse(b).unwrap()).collect();
    let ends: Vec<usize> = (0..parsed.len()).filter(|&i| parsed[i].data_complete()).collect();
    let n = ends.len();
    let boundary = ends[n - 3];
    json!({
        "boundary_shred": hex::encode(&raw[boundary]),
        "shreds": raw[boundary + 1..].iter().map(hex::encode).collect::<Vec<_>>(),
    })
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (work, dir) = (&a[1], &a[2]);
    let slot_b: u64 = a[3].parse().unwrap();
    let n: u64 = a[4].parse().unwrap();

    let parent = load(work, dir, slot_b - 1);
    let b = load(work, dir, slot_b);
    // The change set: every account whose value differs at B's two boundaries.
    let pre: BTreeMap<[u8; 32], &Parsed> = parent.accounts.iter().map(|x| (x.pubkey, x)).collect();
    let post: BTreeMap<[u8; 32], &Parsed> = b.accounts.iter().map(|x| (x.pubkey, x)).collect();
    let mut keys: Vec<[u8; 32]> = pre.keys().chain(post.keys()).copied().collect();
    keys.sort();
    keys.dedup();
    let same = |x: &Parsed, y: &Parsed| x.lamports == y.lamports && x.data == y.data && x.executable == y.executable && x.owner == y.owner;
    let changes: Vec<Value> = keys
        .iter()
        .filter(|k| match (pre.get(*k), post.get(*k)) {
            (Some(x), Some(y)) => !same(x, y),
            _ => true,
        })
        .map(|k| json!({"pubkey": hex32(k), "pre": pre.get(k).map(|x| account(x)), "post": post.get(k).map(|x| account(x))}))
        .collect();

    let (raw_b, roots_b, parent_b, id_b) = shreds_of(work, slot_b);
    let pd = &parent.d;
    let inner_parent = inner_hash(&key(pd["parent_bank_hash"].as_str().unwrap()), pd["signature_count"].as_u64().unwrap(), &key(pd["last_blockhash"].as_str().unwrap()));
    let parts = |bl: &Block| json!({"signature_count": bl.d["signature_count"], "last_blockhash": hex32(&key(bl.d["last_blockhash"].as_str().unwrap())), "lt_hash": lt_hex(&bl.lt)});

    // Descendants: each block's FEC roots, parent, footer run and bank parts.
    let mut desc = Vec::new();
    let mut want_parent = (slot_b, id_b);
    for s in slot_b + 1..=slot_b + n {
        let bl = load(work, dir, s);
        let (raw, roots, par, id) = shreds_of(work, s);
        assert_eq!(par, want_parent, "slot {s} must descend from the previous block");
        desc.push(json!({
            "slot": s, "block_id": hex32(&id), "fec_roots": roots.iter().map(|r| hex32(r)).collect::<Vec<_>>(),
            "parent_slot": par.0, "parent_block_id": hex32(&par.1),
            "footer": footer_run(&raw), "parts": parts(&bl),
        }));
        want_parent = (s, id);
    }

    let out = json!({
        "slot": slot_b, "block_id": hex32(&id_b),
        "fec_roots": roots_b.iter().map(|r| hex32(r)).collect::<Vec<_>>(),
        "parent_slot": parent_b.0, "parent_block_id": hex32(&parent_b.1),
        "footer": footer_run(&raw_b),
        "inner_parent": hex32(&inner_parent), "lt_parent": lt_hex(&parent.lt),
        "block": parts(&b),
        "bank_hash": hex32(&key(b.d["bank_hash"].as_str().unwrap())),
        "changes": changes,
        "descendants": desc,
    });
    std::fs::write(&a[5], serde_json::to_string(&out).unwrap() + "\n").unwrap();
    println!("slot {slot_b}: {} changed accounts, {n} descendants -> {}", out["changes"].as_array().unwrap().len(), a[5]);
}

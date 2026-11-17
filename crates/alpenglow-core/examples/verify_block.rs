//! Native check of one transaction in a `ledger-dump` block file, the same code the zkVM
//! runs: certificate, parent links, inclusion, signatures.
//!
//!   cargo run --release -p alpenglow-core --example verify_block -- block_<slot>.json <signature>
//!
//! The signature is base58 (what explorers show), 128 hex characters, or `first` for the block's
//! first transaction.

use alpenglow_core::fixture;

fn base58(s: &str) -> Option<Vec<u8>> {
    const A: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut n = vec![0u8; 0];
    for c in s.bytes() {
        let mut carry = A.iter().position(|&a| a == c)? as u32;
        for b in n.iter_mut() {
            carry += (*b as u32) * 58;
            *b = carry as u8;
            carry >>= 8;
        }
        while carry > 0 {
            n.push(carry as u8);
            carry >>= 8;
        }
    }
    n.extend(s.bytes().take_while(|&c| c == b'1').map(|_| 0));
    n.reverse();
    Some(n)
}

/// The first signature of the block's first transaction.
fn first_signature(f: &serde_json::Value) -> Vec<u8> {
    use alpenglow_core::{block::{self, Component}, shred::Shred};
    let raw: Vec<Vec<u8>> = f["data_shreds"].as_array().unwrap().iter().map(|h| hex::decode(h.as_str().unwrap()).unwrap()).collect();
    let shreds: Vec<Shred> = raw.iter().map(|b| Shred::parse(b).unwrap()).collect();
    for c in block::deshred(&shreds).unwrap() {
        if let Ok(Component::Entries(entries)) = block::parse_component(&c) {
            if let Some(t) = entries.iter().flat_map(|e| &e.transactions).next() {
                return t.signatures[0].to_vec();
            }
        }
    }
    panic!("no transaction in the block");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, path, sig] = &args[..] else {
        eprintln!("usage: verify_block <block.json> <signature>");
        std::process::exit(2);
    };
    let f: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json");
    let sig = if sig == "first" {
        first_signature(&f)
    } else if sig.len() == 128 {
        hex::decode(sig).expect("hex")
    } else {
        base58(sig).expect("base58")
    };
    let sig: [u8; 64] = sig.try_into().expect("a signature is 64 bytes");
    let input = fixture::tx_input(&f, sig);
    let links = input.ancestry.len();
    match input.verify() {
        Ok(out) => println!(
            "OK slot {} certified via slot {} ({links} link(s)) {} path, {} signature(s) checked, tx {} bytes",
            out.tx_slot,
            out.finality.slot,
            if matches!(out.finality.path, alpenglow_core::proof::Path::Fast) { "fast" } else { "slow" },
            out.tx.raw[0],
            out.tx.raw.len()
        ),
        Err(e) => {
            println!("REFUSED {e:?}");
            std::process::exit(1);
        }
    }
}

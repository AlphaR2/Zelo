//! Pins the outbox program id at build time: `HUGINN_OUTBOX_ID=<base58>` becomes a constant
//! (`OUTBOX_ID`), so the program does not decode it on every fill.

fn b58(s: &str) -> Vec<u8> {
    const A: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut n: Vec<u8> = Vec::new();
    for c in s.bytes() {
        let mut carry = A.iter().position(|&a| a == c).expect("HUGINN_OUTBOX_ID is not base58") as u32;
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
    n
}

fn main() {
    println!("cargo:rerun-if-env-changed=HUGINN_OUTBOX_ID");
    let id = std::env::var("HUGINN_OUTBOX_ID").expect("set HUGINN_OUTBOX_ID to the deployed outbox program id");
    let bytes = b58(&id);
    assert_eq!(bytes.len(), 32, "HUGINN_OUTBOX_ID must be a 32-byte key");
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("outbox_id.rs");
    std::fs::write(out, format!("/// The outbox program (from `HUGINN_OUTBOX_ID` at build time).\npub const OUTBOX_ID: solana_program::pubkey::Pubkey = solana_program::pubkey::Pubkey::new_from_array({bytes:?});\n")).unwrap();
}

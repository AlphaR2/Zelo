//! Prints the escrow's shard list: for each of the fill program's shards, the outbox's emitter
//! state account (what `Escrow` stores as `shards`, as 32-byte hex), the matching base58, and the
//! outbox state bump a fill instruction carries for that shard.
//!
//!   HUGINN_OUTBOX_ID=<outbox id> cargo run --example shards -- <fill program id>

use {huginn_fill::{OUTBOX_ID, SHARDS, SHARD_SEED}, huginn_outbox::state_address, solana_program::pubkey::Pubkey, std::str::FromStr};

fn main() {
    let fill = Pubkey::from_str(&std::env::args().nth(1).expect("fill program id")).expect("a base58 program id");
    for i in 0..SHARDS {
        let (shard, _) = Pubkey::find_program_address(&[SHARD_SEED, &[i]], &fill);
        let (state, state_bump) = state_address(&OUTBOX_ID, &shard);
        let hex: String = state.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        // The escrow stores the hex; a solver passes the state bump in its fill instruction.
        println!("shard {i:2}  0x{hex}  {state}  state_bump {state_bump}");
    }
}

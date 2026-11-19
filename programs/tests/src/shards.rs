//! Prints the escrow's shard list. For each fill shard: the outbox emitter account (the escrow
//! stores it as 32-byte hex), the same in base58, and the `state_bump` a fill instruction carries.
//!
//!   ZELO_OUTBOX_ID=<outbox id> cargo run --bin shards -- <fill program id>

use {
    zelo_programs::{
        fill::{SHARD_SEED, SHARDS},
        outbox::state_address,
    },
    solana_address::Address,
    std::str::FromStr,
};

fn address(s: &str, what: &str) -> Address {
    Address::from_str(s).unwrap_or_else(|_| panic!("{what} is not a base58 address"))
}

fn main() {
    let outbox = address(
        &std::env::var("ZELO_OUTBOX_ID").expect("set ZELO_OUTBOX_ID to the outbox program id"),
        "ZELO_OUTBOX_ID",
    );
    let fill = address(
        &std::env::args()
            .nth(1)
            .expect("usage: shards <fill program id>"),
        "the fill id",
    );
    for i in 0..SHARDS {
        let (shard, _) = Address::find_program_address(&[SHARD_SEED, &[i]], &fill);
        let (state, state_bump) = state_address(&outbox, &shard);
        let hex: String = state
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        println!("shard {i:2}  0x{hex}  {state}  state_bump {state_bump}");
    }
}

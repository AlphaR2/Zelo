//! The verifier, in plain Rust with no Agave crates and no C, so the same code runs natively and
//! inside the SP1 zkVM: finality certificates, shreds and block_id, transactions and their
//! signatures, and what a block changed in the chain's state.

#[cfg(feature = "fixture")]
pub mod accounts_dump;
pub mod bank;
pub mod block;
pub mod finality;
#[cfg(feature = "fixture")]
pub mod fixture;
pub mod proof;
pub mod shred;
pub mod state;
pub mod tx;

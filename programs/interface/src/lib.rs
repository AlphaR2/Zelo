//! What the outbox and fill programs share, with each other and with off-chain code: account
//! layouts, the order id and message format, and PDA helpers.

#![cfg_attr(target_os = "solana", no_std)]

pub mod fill;
pub mod outbox;
pub mod pda;

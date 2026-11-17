//! The reference implementation: the same certificate checks done with Agave's own crates, so
//! tests can confirm `alpenglow-core` agrees with the validator. Also ranks validator sets.

pub mod cert;
pub use alpenglow_core::shred;
pub mod validator_set;

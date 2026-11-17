//! The outbox's emitter account and its hash chain.
//!
//! ```text
//! account data (72 bytes):  emitter [32] | seq u64 LE | chain [32]
//! on each message:          seq' = seq + 1
//!                           chain' = sha256( chain | seq' u64 LE | sha256(payload) )
//! ```

use pinocchio::{Address, error::ProgramError};

pub const STATE_LEN: usize = 72;
pub const EMITTER_SEED: &[u8] = b"emitter";
pub const MAX_PAYLOAD: usize = 1024;

#[derive(Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum OutboxError {
    PayloadTooLarge = 1,
    /// Not this emitter's account: wrong size, wrong owner, or another emitter's key inside.
    BadState = 2,
    /// The account's address is not the emitter's PDA.
    WrongAddress = 3,
    SequenceOverflow = 4,
}

impl From<OutboxError> for ProgramError {
    fn from(e: OutboxError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

/// `sha256( chain | seq u64 LE | sha256(payload) )`.
pub fn next_chain(chain: &[u8; 32], seq: u64, payload: &[u8]) -> [u8; 32] {
    let digest = solana_sha256_hasher::hash(payload).to_bytes();
    solana_sha256_hasher::hashv(&[chain, &seq.to_le_bytes(), &digest]).to_bytes()
}

/// The emitter account of `emitter`, and its canonical bump.
pub fn state_address(outbox: &Address, emitter: &Address) -> (Address, u8) {
    Address::find_program_address(&[EMITTER_SEED, emitter.as_ref()], outbox)
}

/// Appends a message to an emitter account's data and returns the new sequence number.
pub fn append(data: &mut [u8], emitter: &[u8; 32], payload: &[u8]) -> Result<u64, ProgramError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(OutboxError::PayloadTooLarge.into());
    }
    if data.len() != STATE_LEN || data[..32] != emitter[..] {
        return Err(OutboxError::BadState.into());
    }
    let seq = u64::from_le_bytes(data[32..40].try_into().unwrap())
        .checked_add(1)
        .ok_or(OutboxError::SequenceOverflow)?;
    let chain: [u8; 32] = data[40..72].try_into().unwrap();
    data[32..40].copy_from_slice(&seq.to_le_bytes());
    data[40..72].copy_from_slice(&next_chain(&chain, seq, payload));
    Ok(seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cross-checked with Python's hashlib and the Solidity escrow's test vectors.
    pub const PAYLOAD: &str = "2fdd77acaa9e3f37c48dea38292f6a492f31533f5e8f76513fde9a25e9b9902755555555555555555555555555555555555555550000000002faf08044444444444444444444444444444444444444444444444444444444444444443333333333333333333333333333333333333333333333333333333333333333";
    const CHAIN1: &str = "8fce16c73c8504db6a893c2f74df6cea7f2fade96a52b9580d6fd499584ac9cc";
    const CHAIN2: &str = "7b850bf63388437898393b8c0d577955602db9971f0f458c77ed726af265a809";

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn emitter_account(emitter: &[u8; 32]) -> Vec<u8> {
        let mut data = vec![0u8; STATE_LEN];
        data[..32].copy_from_slice(emitter);
        data
    }

    #[test]
    fn the_chain_matches_the_reference_vectors() {
        let c1 = next_chain(&[0; 32], 1, &unhex(PAYLOAD));
        assert_eq!(hex(&c1), CHAIN1);
        assert_eq!(hex(&next_chain(&c1, 2, b"second")), CHAIN2);
    }

    #[test]
    fn append_advances_the_sequence_and_the_chain() {
        let emitter = [9u8; 32];
        let mut data = emitter_account(&emitter);
        assert_eq!(append(&mut data, &emitter, &unhex(PAYLOAD)).unwrap(), 1);
        assert_eq!(hex(&data[40..]), CHAIN1);
        assert_eq!(&data[32..40], &1u64.to_le_bytes());
        assert_eq!(append(&mut data, &emitter, b"second").unwrap(), 2);
        assert_eq!(hex(&data[40..]), CHAIN2);
    }

    #[test]
    fn append_refuses_a_wrong_account_or_a_large_payload() {
        let emitter = [9u8; 32];
        let mut good = emitter_account(&emitter);
        let bad_state = Err(OutboxError::BadState.into());
        assert_eq!(append(&mut good.clone(), &[8u8; 32], b"x"), bad_state);
        assert_eq!(append(&mut [0u8; 71], &emitter, b"x"), bad_state);
        assert_eq!(append(&mut [0u8; 73], &emitter, b"x"), bad_state);
        let before = good.clone();
        let too_big = vec![0u8; MAX_PAYLOAD + 1];
        assert_eq!(
            append(&mut good, &emitter, &too_big),
            Err(OutboxError::PayloadTooLarge.into())
        );
        assert_eq!(
            good, before,
            "a refused payload leaves the account untouched"
        );
        assert!(append(&mut good, &emitter, &vec![0u8; MAX_PAYLOAD]).is_ok());
    }

    #[test]
    fn the_sequence_cannot_wrap() {
        let emitter = [9u8; 32];
        let mut data = emitter_account(&emitter);
        data[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(
            append(&mut data, &emitter, b"x"),
            Err(OutboxError::SequenceOverflow.into())
        );
    }

    #[test]
    fn each_emitter_has_its_own_account() {
        let outbox = Address::new_from_array([1; 32]);
        let (a, _) = state_address(&outbox, &Address::new_from_array([2; 32]));
        let (b, _) = state_address(&outbox, &Address::new_from_array([3; 32]));
        assert_ne!(a, b);
    }
}

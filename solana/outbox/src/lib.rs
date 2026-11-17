//! The outbox: a message box other chains can read through a Huginn state proof.
//!
//! `emit(payload)` appends one message to the caller's **emitter account**, a PDA of this
//! program at `["emitter", emitter]`, where `emitter` is the key (or the PDA, signed for with
//! `invoke_signed`) that sent it. The account holds a running hash chain:
//!
//! ```text
//! data (72 bytes):  emitter [32] | seq u64 LE | chain [32]
//! on emit:          seq' = seq + 1
//!                   chain' = sha256( chain | seq' u64 LE | sha256(payload) )
//! ```
//!
//! A failed transaction leaves nothing behind, so an account value is proof that every message
//! in its chain was emitted by a transaction that succeeded. A state proof shows an account's
//! value at the two boundaries of one block. If an emitter sends several messages in a block, the
//! chain still lets all of them be delivered: the proof's "after" chain, the "before" chain and
//! the payload hashes of the block's messages reproduce each other (see `Escrow.claim`).
//!
//! The payload is also written to the transaction log (`sol_log_data`) so relayers can read it
//! with `getTransaction`. Logs are not trusted: the chain check verifies them.
//!
//! No instruction closes or rewrites an emitter account, so a chain only ever grows. The program
//! is meant to be deployed immutable (upgrade authority revoked): its address is part of what
//! contracts on other chains trust.
//!
//! Accounts for `emit`:
//!   0. `[signer]`            the emitter (a wallet, or a program's PDA signed for by the caller)
//!   1. `[writable]`          the emitter's state PDA, `["emitter", emitter]`
//!   2. `[signer, writable]`  payer of the account's rent, the first time
//!   3. `[]`                  the system program
//! Instruction data: `bump u8 | payload` (payload at most [`MAX_PAYLOAD`] bytes). When the account
//! is created, only the **canonical** address (the one `find_program_address` returns) is accepted,
//! whatever bump was passed: otherwise anyone could create a second chain account for one emitter,
//! and a consumer that trusts the emitter key in the data would read the wrong chain. Afterwards the
//! passed bump is checked with one `create_program_address`: an outbox-owned account holding the
//! emitter's key can only be the canonical one.

use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    hash::hashv,
    log::sol_log_data,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction,
    sysvar::Sysvar,
};

pub const STATE_LEN: usize = 72;
pub const EMITTER_SEED: &[u8] = b"emitter";
pub const MAX_PAYLOAD: usize = 1024;

#[derive(Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum OutboxError {
    PayloadTooLarge = 1,
    /// The state account is not an emitter account of this program (wrong size, wrong owner, or
    /// it holds another emitter's key).
    BadState = 2,
    /// The state account's address is not the emitter's PDA.
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
    let digest = hashv(&[payload]).to_bytes();
    hashv(&[chain, &seq.to_le_bytes(), &digest]).to_bytes()
}

/// The state PDA of `emitter`, and its bump (off chain, or once when building an instruction).
pub fn state_address(program_id: &Pubkey, emitter: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[EMITTER_SEED, emitter.as_ref()], program_id)
}

/// Appends a message to an emitter account's data (72 bytes), returning the new sequence number.
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

/// Creates a PDA account owned by `owner` with `space` bytes, whatever it holds already.
///
/// Anyone can send lamports to a PDA's address before it exists. `create_account` fails on an
/// address that already has lamports, which would let anyone block it; so this tops the balance
/// up to rent exemption with a transfer, then allocates and assigns, both signed by the PDA.
pub fn create_pda<'a>(
    pda: &AccountInfo<'a>,
    payer: &AccountInfo<'a>,
    system: &AccountInfo<'a>,
    owner: &Pubkey,
    space: usize,
    seeds: &[&[u8]],
) -> ProgramResult {
    let needed = Rent::get()?.minimum_balance(space).saturating_sub(pda.lamports());
    if needed > 0 {
        invoke(
            &system_instruction::transfer(payer.key, pda.key, needed),
            &[payer.clone(), pda.clone(), system.clone()],
        )?;
    }
    invoke_signed(
        &system_instruction::allocate(pda.key, space as u64),
        &[pda.clone(), system.clone()],
        &[seeds],
    )?;
    invoke_signed(
        &system_instruction::assign(pda.key, owner),
        &[pda.clone(), system.clone()],
        &[seeds],
    )
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let (&bump, payload) = data.split_first().ok_or(ProgramError::InvalidInstructionData)?;
    let it = &mut accounts.iter();
    let emitter = next_account_info(it)?;
    let state = next_account_info(it)?;
    let payer = next_account_info(it)?;
    let system = next_account_info(it)?;
    if !emitter.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if payload.len() > MAX_PAYLOAD {
        return Err(OutboxError::PayloadTooLarge.into());
    }
    if state.owner == program_id {
        // An existing emitter account: it was created at the canonical address (below), so the
        // passed bump only has to reproduce this address.
        let address = Pubkey::create_program_address(&[EMITTER_SEED, emitter.key.as_ref(), &[bump]], program_id)
            .map_err(|_| OutboxError::WrongAddress)?;
        if address != *state.key {
            return Err(OutboxError::WrongAddress.into());
        }
    } else {
        // Creating: the canonical address only, searched for once in the account's life.
        let (canonical, canonical_bump) = state_address(program_id, emitter.key);
        if canonical != *state.key || canonical_bump != bump {
            return Err(OutboxError::WrongAddress.into());
        }
        // First message from this emitter. The address may hold lamports (anyone can send some),
        // but it must be an empty system account.
        if !state.data_is_empty() {
            return Err(OutboxError::BadState.into());
        }
        let seeds: &[&[u8]] = &[EMITTER_SEED, emitter.key.as_ref(), &[bump]];
        create_pda(state, payer, system, program_id, STATE_LEN, seeds)?;
        let mut data = state.try_borrow_mut_data()?;
        data[..32].copy_from_slice(emitter.key.as_ref());
    }
    let seq = append(&mut state.try_borrow_mut_data()?, &emitter.key.to_bytes(), payload)?;
    sol_log_data(&[&seq.to_le_bytes(), payload]);
    Ok(())
}

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process);

#[cfg(test)]
mod tests {
    use super::*;

    // Cross-checked against Python's hashlib and the Solidity escrow's test vectors.
    const PAYLOAD: &str = "2fdd77acaa9e3f37c48dea38292f6a492f31533f5e8f76513fde9a25e9b9902755555555555555555555555555555555555555550000000002faf08044444444444444444444444444444444444444444444444444444444444444443333333333333333333333333333333333333333333333333333333333333333";
    const CHAIN1: &str = "8fce16c73c8504db6a893c2f74df6cea7f2fade96a52b9580d6fd499584ac9cc";
    const CHAIN2: &str = "7b850bf63388437898393b8c0d577955602db9971f0f458c77ed726af265a809";

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn the_chain_matches_the_reference_vectors() {
        let payload = unhex(PAYLOAD);
        let c1 = next_chain(&[0; 32], 1, &payload);
        assert_eq!(hex(&c1), CHAIN1);
        let c2 = next_chain(&c1, 2, b"second");
        assert_eq!(hex(&c2), CHAIN2);
    }

    #[test]
    fn append_advances_the_sequence_and_the_chain() {
        let emitter = [9u8; 32];
        let mut data = vec![0u8; STATE_LEN];
        data[..32].copy_from_slice(&emitter);
        let payload = unhex(PAYLOAD);
        assert_eq!(append(&mut data, &emitter, &payload).unwrap(), 1);
        assert_eq!(hex(&data[40..]), CHAIN1);
        assert_eq!(&data[32..40], &1u64.to_le_bytes());
        assert_eq!(append(&mut data, &emitter, b"second").unwrap(), 2);
        assert_eq!(hex(&data[40..]), CHAIN2);
    }

    #[test]
    fn append_refuses_a_wrong_account_or_a_large_payload() {
        let emitter = [9u8; 32];
        let mut good = vec![0u8; STATE_LEN];
        good[..32].copy_from_slice(&emitter);
        // Another emitter's account.
        assert_eq!(append(&mut good.clone(), &[8u8; 32], b"x"), Err(OutboxError::BadState.into()));
        // The wrong size.
        assert_eq!(append(&mut vec![0u8; 71], &emitter, b"x"), Err(OutboxError::BadState.into()));
        assert_eq!(append(&mut vec![0u8; 73], &emitter, b"x"), Err(OutboxError::BadState.into()));
        // A payload over the limit leaves the account untouched.
        let before = good.clone();
        assert_eq!(append(&mut good, &emitter, &vec![0u8; MAX_PAYLOAD + 1]), Err(OutboxError::PayloadTooLarge.into()));
        assert_eq!(good, before);
        // Exactly at the limit is fine.
        assert!(append(&mut good, &emitter, &vec![0u8; MAX_PAYLOAD]).is_ok());
    }

    #[test]
    fn the_sequence_cannot_wrap() {
        let emitter = [9u8; 32];
        let mut data = vec![0u8; STATE_LEN];
        data[..32].copy_from_slice(&emitter);
        data[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(append(&mut data, &emitter, b"x"), Err(OutboxError::SequenceOverflow.into()));
    }

    #[test]
    fn only_the_canonical_address_is_the_emitter_account() {
        // Other bumps give other valid addresses; `process` refuses them when creating.
        let program = Pubkey::new_unique();
        let emitter = Pubkey::new_unique();
        let (canonical, bump) = state_address(&program, &emitter);
        let other = (0..bump).rev().find_map(|b| {
            Pubkey::create_program_address(&[EMITTER_SEED, emitter.as_ref(), &[b]], &program).ok()
        });
        assert!(other.is_some_and(|o| o != canonical), "another valid bump exists and differs");
    }

    #[test]
    fn each_emitter_has_its_own_account() {
        let program = Pubkey::new_unique();
        let (a, _) = state_address(&program, &Pubkey::new_unique());
        let (b, _) = state_address(&program, &Pubkey::new_unique());
        assert_ne!(a, b);
    }
}

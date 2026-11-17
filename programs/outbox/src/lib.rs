//! The outbox: a message box that other chains read through a Huginn state proof.
//!
//! `emit(payload)` appends one message to the sender's **emitter account**, a PDA of this
//! program at `["emitter", emitter]`. The sender is a wallet, or a program's PDA that the program
//! signs for. The account holds a hash chain of every message (layout in
//! [`huginn_programs::outbox`]).
//!
//! A failed transaction changes nothing, so an account's value proves that every message in its
//! chain came from a transaction that succeeded. A state proof shows the account before and after
//! one block; with the chain, every message emitted in that block can be checked, even several.
//!
//! The payload is also logged (`sol_log_data`) so relayers can find it with `getTransaction`.
//! Logs are not trusted: the chain check verifies them.
//!
//! Nothing closes or rewrites an emitter account, so a chain only grows. Deploy the program
//! immutable: its address is part of what contracts on other chains trust.
//!
//! Accounts:
//!   0. `[signer]`            the emitter
//!   1. `[writable]`          the emitter account, `["emitter", emitter]`
//!   2. `[signer, writable]`  pays the account's rent the first time
//!   3. `[]`                  the system program
//!
//! Instruction data: `bump u8 | payload` (at most 1,024 bytes).
//!
//! **One account per emitter.** A PDA has many valid bumps. When the account is created, only the
//! canonical address is accepted; otherwise anyone could open a second chain for the same emitter.
//! Later calls only check that the passed bump gives this account's address, which is cheap: an
//! outbox account holding the emitter's key can only be the canonical one.

#![cfg_attr(target_os = "solana", no_std)]

use {
    huginn_programs::{
        outbox::{EMITTER_SEED, MAX_PAYLOAD, OutboxError, STATE_LEN, append, state_address},
        pda::create_pda,
    },
    pinocchio::{
        AccountView, Address, ProgramResult,
        cpi::{Seed, Signer},
        error::ProgramError,
    },
};

pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let (&bump, payload) = data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;
    let [emitter, state, payer, _system, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !emitter.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if payload.len() > MAX_PAYLOAD {
        return Err(OutboxError::PayloadTooLarge.into());
    }
    let key = emitter.address().to_bytes();
    if state.owned_by(program_id) {
        let address = Address::create_program_address(&[EMITTER_SEED, &key, &[bump]], program_id)
            .map_err(|_| OutboxError::WrongAddress)?;
        if address != *state.address() {
            return Err(OutboxError::WrongAddress.into());
        }
    } else {
        // First message: only the canonical address, still an empty account (lamports someone
        // sent are fine).
        let (canonical, canonical_bump) = state_address(program_id, emitter.address());
        if canonical != *state.address() || canonical_bump != bump {
            return Err(OutboxError::WrongAddress.into());
        }
        if !state.is_data_empty() {
            return Err(OutboxError::BadState.into());
        }
        let bump = [bump];
        let seeds = [
            Seed::from(EMITTER_SEED),
            Seed::from(&key),
            Seed::from(&bump),
        ];
        create_pda(state, payer, program_id, STATE_LEN, Signer::from(&seeds))?;
        state.try_borrow_mut()?[..32].copy_from_slice(&key);
    }
    let seq = append(&mut state.try_borrow_mut()?, &key, payload)?;
    solana_program_log::log_data(&[&seq.to_le_bytes(), payload]);
    Ok(())
}

#[cfg(target_os = "solana")]
mod entrypoint {
    pinocchio::program_entrypoint!(crate::process, 4);
    pinocchio::no_allocator!();
    pinocchio::nostd_panic_handler!();
}

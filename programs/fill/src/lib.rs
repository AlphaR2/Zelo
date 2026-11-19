//! The fill program: a solver pays a merchant on Solana, and the escrow on another chain repays
//! the solver, trusting only Solana's validators (through a Zelo state proof).
//!
//! One instruction. In a single transaction it:
//! 1. computes the order id from the order's fields, as the escrow does. The id covers
//!    `amount_in`, so the fill commits to what the solver will be repaid;
//! 2. checks the clock against the deadline. A weak guard: the cluster clock can lag; the
//!    escrow's refund margin covers the rest;
//! 3. checks the destination is the merchant's token account for the order's mint, then moves
//!    `amount_out` from the solver (`TransferChecked`);
//! 4. creates the marker PDA `["filled", order_id]`, so an order is filled once;
//! 5. emits the 124-byte message through the outbox, from one of 16 shard PDAs of this program.
//!
//! If any step fails, the transaction fails and nothing is emitted. Nothing closes or changes a
//! marker. Deploy the program immutable. Layouts: [`zelo_programs::fill`].
//!
//! **Canonical addresses only**: the marker and the shard are checked with
//! `find_program_address`, or one order could get two markers and be filled twice. The outbox
//! itself enforces the canonical emitter account.
//!
//! Accounts:
//!   0. `[signer, writable]`  solver: owns the source token account, pays rent
//!   1. `[writable]`          the solver's token account (source)
//!   2. `[writable]`          the merchant's token account (destination)
//!   3. `[]`                  the mint
//!   4. `[]`                  the SPL Token program
//!   5. `[writable]`          the marker PDA `["filled", order_id]`
//!   6. `[]`                  the shard PDA `["shard", shard]` (signs for the outbox)
//!   7. `[writable]`          the shard's emitter account in the outbox
//!   8. `[]`                  the outbox program (fixed at build time by `ZELO_OUTBOX_ID`)
//!   9. `[]`                  the system program

#![cfg_attr(target_os = "solana", no_std)]

use {
    zelo_programs::{
        fill::{
            FillError, MARKER_SEED, MINT_DECIMALS, MINT_LEN, Order, PAYLOAD_LEN, SHARD_SEED,
            token_account_owner,
        },
        pda::{check_canonical, create_pda},
    },
    pinocchio::{
        AccountView, Address, ProgramResult,
        cpi::{Seed, Signer, invoke_signed},
        error::ProgramError,
        instruction::{InstructionAccount, InstructionView},
        sysvars::{Sysvar, clock::Clock},
    },
    pinocchio_token::instructions::TransferChecked,
};

/// The outbox program this build trusts (`ZELO_OUTBOX_ID`, base58, at build time).
pub const OUTBOX_ID: Address = Address::from_str_const(env!("ZELO_OUTBOX_ID"));

pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let order = Order::parse(data)?;
    let [
        solver,
        source,
        dest,
        mint,
        token,
        marker,
        shard,
        outbox_state,
        outbox,
        system,
        ..,
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !solver.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    // Classic SPL Token only: Token-2022 accounts have another layout.
    if *token.address() != pinocchio_token::ID {
        return Err(FillError::NotTheTokenProgram.into());
    }
    if *outbox.address() != OUTBOX_ID {
        return Err(FillError::NotTheOutbox.into());
    }

    // The destination must be a token account of this mint; its owner is the merchant the id
    // commits to.
    if !mint.owned_by(&pinocchio_token::ID)
        || mint.data_len() != MINT_LEN
        || mint.try_borrow()?[MINT_DECIMALS] != order.decimals
    {
        return Err(FillError::BadMint.into());
    }
    if !dest.owned_by(&pinocchio_token::ID) {
        return Err(FillError::BadDestination.into());
    }
    let mint_key = mint.address().to_bytes();
    let merchant = token_account_owner(&dest.try_borrow()?, &mint_key)?;
    let id = order.id(&merchant, &mint_key);

    let now = Clock::get()?.unix_timestamp;
    if now < 0 || now as u64 > order.deadline {
        return Err(FillError::PastDeadline.into());
    }

    // One fill per order.
    let marker_bump = check_canonical(&[MARKER_SEED, &id], program_id, marker.address())
        .map_err(|_| FillError::WrongMarker)?;
    if marker.owned_by(program_id) {
        return Err(FillError::AlreadyFilled.into());
    }
    let bump = [marker_bump];
    let seeds = [Seed::from(MARKER_SEED), Seed::from(&id), Seed::from(&bump)];
    create_pda(marker, solver, program_id, 1, Signer::from(&seeds))?;
    marker.try_borrow_mut()?[0] = 1;

    // Pay the merchant.
    TransferChecked::<&AccountView>::new(
        source,
        mint,
        dest,
        solver,
        order.amount_out,
        order.decimals,
    )
    .invoke()?;

    // Leave the message, signed by the shard PDA.
    let shard_index = [order.shard];
    let shard_bump = check_canonical(&[SHARD_SEED, &shard_index], program_id, shard.address())
        .map_err(|_| FillError::WrongShard)?;
    let mut emit = [0u8; 1 + PAYLOAD_LEN];
    emit[0] = order.state_bump;
    emit[1..].copy_from_slice(&order.payload(&id, &merchant, &mint_key));
    let metas = [
        InstructionAccount::readonly_signer(shard.address()),
        InstructionAccount::writable(outbox_state.address()),
        InstructionAccount::writable_signer(solver.address()),
        InstructionAccount::readonly(system.address()),
    ];
    let instruction = InstructionView {
        program_id: &OUTBOX_ID,
        accounts: &metas,
        data: &emit,
    };
    let bump = [shard_bump];
    let seeds = [
        Seed::from(SHARD_SEED),
        Seed::from(&shard_index),
        Seed::from(&bump),
    ];
    invoke_signed(
        &instruction,
        &[&*shard, &*outbox_state, &*solver, &*system],
        &[Signer::from(&seeds)],
    )
}

#[cfg(target_os = "solana")]
mod entrypoint {
    pinocchio::program_entrypoint!(crate::process, 10);
    pinocchio::no_allocator!();
    pinocchio::nostd_panic_handler!();
}

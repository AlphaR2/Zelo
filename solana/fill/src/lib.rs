//! The fill program: a solver pays a merchant on Solana, and the escrow on another chain pays the
//! solver back, trusting only Solana's validators (through a Huginn state proof).
//!
//! One instruction, `fill`. In a single transaction it:
//! 1. recomputes the order id from the order's fields (the escrow computes the same id; it covers
//!    `amount_in`, so the fill commits to what the solver will be paid);
//! 2. checks the clock against the order's deadline (a weak guard: Alpenglow's clock is set by
//!    leaders within bounds and can lag, so it cannot make the deadline exact; the escrow's
//!    refund margin covers the rest);
//! 3. checks the destination token account really belongs to the merchant and holds the order's
//!    mint, then transfers `amount_out` from the solver (`transfer_checked`);
//! 4. creates a marker PDA `["filled", order_id]`, so the order can be filled once;
//! 5. emits a message through the outbox, from one of [`SHARDS`] emitter PDAs of this program:
//!    `order_id | solver_evm [20] | amount_out u64 BE | mint [32] | merchant [32]` (124 bytes).
//!
//! If anything fails, the transaction fails and nothing is emitted. The program has no instruction
//! that closes or changes a marker, and it is meant to be deployed immutable.
//!
//! Order id (must match `Escrow.orderId`, which is `keccak256(abi.encodePacked(...))`):
//! `keccak256( chain_id u64 BE | escrow [20] | user [20] | nonce u64 BE | merchant [32] | mint [32]
//!             | amount_out u64 BE | deadline u64 BE | amount_in [32] BE )`.
//!
//! Instruction data (132 bytes; integers little-endian except `amount_in`, which is the EVM's
//! 32-byte big-endian word): `chain_id u64 | escrow [20] | user [20] | nonce u64 | amount_out u64 |
//! deadline u64 | amount_in [32] | solver_evm [20] | shard u8 | decimals u8 | state_bump u8 |
//! reserved [5] (zero)`. The merchant is read from the destination token account's owner, the mint
//! from the mint account.
//!
//! **Canonical addresses only.** A PDA has up to 256 valid bumps; only the canonical one (the
//! highest, what `find_program_address` returns) may be used, or one order could have several
//! markers and be filled twice (a self-fill with an odd bump, then an honest solver's fill that is
//! never repaid). So the marker and the shard are derived here with `find_program_address`. The
//! outbox state account's bump is passed through: the outbox itself only accepts the canonical
//! address when it creates the account.
//!
//! Accounts:
//!   0. `[signer, writable]`  solver: owns the source token account, pays rent
//!   1. `[writable]`          the solver's token account (source)
//!   2. `[writable]`          the merchant's token account (destination)
//!   3. `[]`                  the mint
//!   4. `[]`                  the SPL Token program
//!   5. `[writable]`          the marker PDA `["filled", order_id]`
//!   6. `[]`                  this program's shard PDA `["shard", shard]` (signs for the outbox)
//!   7. `[writable]`          the outbox's emitter state PDA `["emitter", shard PDA]`
//!   8. `[]`                  the outbox program (its id is fixed at build time: `HUGINN_OUTBOX_ID`)
//!   9. `[]`                  the system program

use {
    huginn_outbox::create_pda,
    solana_program::{
        account_info::{next_account_info, AccountInfo},
        clock::Clock,
        entrypoint::ProgramResult,
        instruction::{AccountMeta, Instruction},
        keccak,
        program::invoke_signed,
        program_error::ProgramError,
        pubkey::Pubkey,
        sysvar::Sysvar,
    },
};

include!(concat!(env!("OUT_DIR"), "/outbox_id.rs"));

/// How many emitter shards this program signs for. The escrow on the other chain lists each
/// shard's outbox state account, fixed at its deployment. Spreading solvers over shards avoids
/// every fill writing one account (Solana runs transactions that write the same account in turn).
pub const SHARDS: u8 = 16;
pub const SHARD_SEED: &[u8] = b"shard";
pub const MARKER_SEED: &[u8] = b"filled";
pub const DATA_LEN: usize = 132;
pub const PAYLOAD_LEN: usize = 124;

/// The classic SPL Token program, `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`.
pub const TOKEN_PROGRAM: Pubkey = Pubkey::new_from_array([
    6, 221, 246, 225, 215, 101, 161, 147, 217, 203, 225, 70, 206, 235, 121, 172, 28, 180, 133, 237, 95, 91, 55,
    145, 58, 140, 245, 133, 126, 255, 0, 169,
]);
const TOKEN_ACCOUNT_LEN: usize = 165;
const MINT_LEN: usize = 82;

#[derive(Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum FillError {
    BadData = 1,
    ShardOutOfRange = 2,
    PastDeadline = 3,
    NotTheTokenProgram = 4,
    /// The destination is not a token account of this mint owned by the order's merchant.
    BadDestination = 5,
    BadMint = 6,
    WrongMarker = 7,
    AlreadyFilled = 8,
    WrongShard = 9,
    NotTheOutbox = 10,
}

impl From<FillError> for ProgramError {
    fn from(e: FillError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

/// An order as the fill instruction carries it, plus what the accounts add.
#[derive(Debug, PartialEq, Eq)]
pub struct Order {
    pub chain_id: u64,
    pub escrow: [u8; 20],
    pub user: [u8; 20],
    pub nonce: u64,
    pub amount_out: u64,
    pub deadline: u64,
    /// What Ada locked on the EVM chain: a 32-byte big-endian word, as the escrow hashes it.
    pub amount_in: [u8; 32],
    pub solver_evm: [u8; 20],
    pub shard: u8,
    pub decimals: u8,
    /// The outbox state account's bump (the outbox insists on the canonical one when it creates
    /// the account; afterwards only that account can exist).
    pub state_bump: u8,
}

impl Order {
    pub fn parse(d: &[u8]) -> Result<Order, ProgramError> {
        if d.len() != DATA_LEN {
            return Err(FillError::BadData.into());
        }
        let u64_at = |i: usize| u64::from_le_bytes(d[i..i + 8].try_into().unwrap());
        let o = Order {
            chain_id: u64_at(0),
            escrow: d[8..28].try_into().unwrap(),
            user: d[28..48].try_into().unwrap(),
            nonce: u64_at(48),
            amount_out: u64_at(56),
            deadline: u64_at(64),
            amount_in: d[72..104].try_into().unwrap(),
            solver_evm: d[104..124].try_into().unwrap(),
            shard: d[124],
            decimals: d[125],
            state_bump: d[126],
        };
        // Bytes 127..132 are reserved and must be zero.
        if d[127..].iter().any(|b| *b != 0) {
            return Err(FillError::BadData.into());
        }
        if o.shard >= SHARDS {
            return Err(FillError::ShardOutOfRange.into());
        }
        Ok(o)
    }

    /// `keccak256` over the packed layout the escrow uses.
    pub fn id(&self, merchant: &[u8; 32], mint: &[u8; 32]) -> [u8; 32] {
        keccak::hashv(&[
            &self.chain_id.to_be_bytes(),
            &self.escrow,
            &self.user,
            &self.nonce.to_be_bytes(),
            merchant,
            mint,
            &self.amount_out.to_be_bytes(),
            &self.deadline.to_be_bytes(),
            &self.amount_in,
        ])
        .to_bytes()
    }

    /// The message: `order_id | solver_evm | amount_out BE | mint | merchant`.
    pub fn payload(&self, id: &[u8; 32], merchant: &[u8; 32], mint: &[u8; 32]) -> [u8; PAYLOAD_LEN] {
        let mut p = [0u8; PAYLOAD_LEN];
        p[..32].copy_from_slice(id);
        p[32..52].copy_from_slice(&self.solver_evm);
        p[52..60].copy_from_slice(&self.amount_out.to_be_bytes());
        p[60..92].copy_from_slice(mint);
        p[92..124].copy_from_slice(merchant);
        p
    }
}

/// The merchant (owner) of an SPL token account of `mint`, or an error if it is not one.
pub fn token_account_owner(data: &[u8], mint: &[u8; 32]) -> Result<[u8; 32], ProgramError> {
    // mint [0..32] | owner [32..64] | amount | delegate | state u8 at 108 | ...
    if data.len() != TOKEN_ACCOUNT_LEN || data[..32] != mint[..] || data[108] != 1 {
        return Err(FillError::BadDestination.into());
    }
    Ok(data[32..64].try_into().unwrap())
}

/// Checks that `key` is the canonical PDA for `seeds` (the one `find_program_address` returns) and
/// returns its bump. Any other valid bump is refused: a second address for the same seeds would
/// break "one per seeds" (one marker per order).
pub fn check_canonical(seeds: &[&[u8]], program_id: &Pubkey, key: &Pubkey) -> Result<u8, ProgramError> {
    let (address, bump) = Pubkey::find_program_address(seeds, program_id);
    if address != *key {
        return Err(ProgramError::InvalidSeeds);
    }
    Ok(bump)
}

/// An SPL Token `TransferChecked` instruction (tag 12).
fn transfer_checked(token_program: &Pubkey, from: &Pubkey, mint: &Pubkey, to: &Pubkey, authority: &Pubkey, amount: u64, decimals: u8) -> Instruction {
    let mut data = vec![12u8];
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(decimals);
    Instruction {
        program_id: *token_program,
        accounts: vec![
            AccountMeta::new(*from, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(*to, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data,
    }
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let order = Order::parse(data)?;
    let it = &mut accounts.iter();
    let solver = next_account_info(it)?;
    let source = next_account_info(it)?;
    let dest = next_account_info(it)?;
    let mint = next_account_info(it)?;
    let token_program = next_account_info(it)?;
    let marker = next_account_info(it)?;
    let shard = next_account_info(it)?;
    let outbox_state = next_account_info(it)?;
    let outbox = next_account_info(it)?;
    let system = next_account_info(it)?;

    if !solver.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if *token_program.key != TOKEN_PROGRAM {
        return Err(FillError::NotTheTokenProgram.into());
    }
    // The outbox is fixed when this program is built: the escrow trusts that program's accounts.
    if *outbox.key != OUTBOX_ID {
        return Err(FillError::NotTheOutbox.into());
    }

    // Mint, decimals and destination: the destination must be a token account of this mint, and
    // its owner is the merchant the order names (the id commits to it).
    if mint.owner != token_program.key || mint.data_len() != MINT_LEN {
        return Err(FillError::BadMint.into());
    }
    if mint.try_borrow_data()?[44] != order.decimals {
        return Err(FillError::BadMint.into());
    }
    if dest.owner != token_program.key {
        return Err(FillError::BadDestination.into());
    }
    let merchant = token_account_owner(&dest.try_borrow_data()?, &mint.key.to_bytes())?;
    let id = order.id(&merchant, &mint.key.to_bytes());

    // A weak guard: the clock can lag. The escrow's refund margin is the real protection.
    let now = Clock::get()?.unix_timestamp;
    if now < 0 || now as u64 > order.deadline {
        return Err(FillError::PastDeadline.into());
    }

    // The marker: one fill per order. Created like the outbox's accounts (an address anyone
    // pre-funded must not block it).
    let marker_bump = check_canonical(&[MARKER_SEED, &id], program_id, marker.key)
        .map_err(|_| FillError::WrongMarker)?;
    if marker.owner == program_id {
        return Err(FillError::AlreadyFilled.into());
    }
    let marker_seeds: &[&[u8]] = &[MARKER_SEED, &id, &[marker_bump]];
    create_pda(marker, solver, system, program_id, 1, marker_seeds)?;
    marker.try_borrow_mut_data()?[0] = 1;

    // Pay the merchant. If this fails, so does the whole transaction.
    invoke_signed(
        &transfer_checked(token_program.key, source.key, mint.key, dest.key, solver.key, order.amount_out, order.decimals),
        &[source.clone(), mint.clone(), dest.clone(), solver.clone(), token_program.clone()],
        &[],
    )?;

    // Leave the message, from this program's shard PDA.
    let shard_bump = check_canonical(&[SHARD_SEED, &[order.shard]], program_id, shard.key)
        .map_err(|_| FillError::WrongShard)?;
    let payload = order.payload(&id, &merchant, &mint.key.to_bytes());
    // The outbox checks its state account against the bump we pass it.
    let mut emit_data = Vec::with_capacity(1 + PAYLOAD_LEN);
    emit_data.push(order.state_bump);
    emit_data.extend_from_slice(&payload);
    let emit = Instruction {
        program_id: OUTBOX_ID,
        accounts: vec![
            AccountMeta::new_readonly(*shard.key, true),
            AccountMeta::new(*outbox_state.key, false),
            AccountMeta::new(*solver.key, true),
            AccountMeta::new_readonly(*system.key, false),
        ],
        data: emit_data,
    };
    invoke_signed(
        &emit,
        &[shard.clone(), outbox_state.clone(), solver.clone(), system.clone(), outbox.clone()],
        &[&[SHARD_SEED, &[order.shard], &[shard_bump]]],
    )
}

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process);

#[cfg(test)]
mod tests {
    use super::*;

    fn order() -> Order {
        Order {
            chain_id: 84532,
            escrow: [0x11; 20],
            user: [0x22; 20],
            nonce: 7,
            amount_out: 50_000_000,
            deadline: 1_800_003_600,
            amount_in: amount_in(100_000_000),
            solver_evm: [0x55; 20],
            shard: 3,
            decimals: 6,
            state_bump: 252,
        }
    }

    fn amount_in(v: u128) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[16..].copy_from_slice(&v.to_be_bytes());
        w
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    // Reference values: `cast keccak` over the packed layout the Solidity escrow uses.
    const ORDER_ID: &str = "784041714c70f811a1a12cdbf70ccfff036a5463c7dce02864f1624e51f79de4";
    const PAYLOAD: &str = "784041714c70f811a1a12cdbf70ccfff036a5463c7dce02864f1624e51f79de455555555555555555555555555555555555555550000000002faf08044444444444444444444444444444444444444444444444444444444444444443333333333333333333333333333333333333333333333333333333333333333";

    #[test]
    fn the_order_id_matches_the_escrows_packed_layout() {
        let id = order().id(&[0x33; 32], &[0x44; 32]);
        assert_eq!(hex(&id), ORDER_ID);
    }

    #[test]
    fn the_payload_is_the_124_bytes_the_escrow_reads() {
        let o = order();
        let id = o.id(&[0x33; 32], &[0x44; 32]);
        let p = o.payload(&id, &[0x33; 32], &[0x44; 32]);
        assert_eq!(hex(&p), PAYLOAD);
        assert_eq!(p.len(), PAYLOAD_LEN);
    }

    #[test]
    fn every_order_field_changes_the_id() {
        let base = order().id(&[0x33; 32], &[0x44; 32]);
        let mut o = order();
        o.chain_id += 1;
        assert_ne!(o.id(&[0x33; 32], &[0x44; 32]), base);
        let mut o = order();
        o.escrow[0] ^= 1;
        assert_ne!(o.id(&[0x33; 32], &[0x44; 32]), base);
        let mut o = order();
        o.user[0] ^= 1;
        assert_ne!(o.id(&[0x33; 32], &[0x44; 32]), base);
        let mut o = order();
        o.nonce += 1;
        assert_ne!(o.id(&[0x33; 32], &[0x44; 32]), base);
        let mut o = order();
        o.amount_out += 1;
        assert_ne!(o.id(&[0x33; 32], &[0x44; 32]), base);
        let mut o = order();
        o.deadline += 1;
        assert_ne!(o.id(&[0x33; 32], &[0x44; 32]), base);
        let mut o = order();
        o.amount_in = amount_in(99_000_000);
        assert_ne!(o.id(&[0x33; 32], &[0x44; 32]), base, "amount_in");
        assert_ne!(order().id(&[0x34; 32], &[0x44; 32]), base, "merchant");
        assert_ne!(order().id(&[0x33; 32], &[0x45; 32]), base, "mint");
        // The solver, the shard, the decimals and the bumps are not part of the id: any solver may fill.
        let mut o = order();
        o.solver_evm[0] ^= 1;
        o.shard = 9;
        o.state_bump = 1;
        assert_eq!(o.id(&[0x33; 32], &[0x44; 32]), base);
    }

    fn encode(o: &Order) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&o.chain_id.to_le_bytes());
        d.extend_from_slice(&o.escrow);
        d.extend_from_slice(&o.user);
        d.extend_from_slice(&o.nonce.to_le_bytes());
        d.extend_from_slice(&o.amount_out.to_le_bytes());
        d.extend_from_slice(&o.deadline.to_le_bytes());
        d.extend_from_slice(&o.amount_in);
        d.extend_from_slice(&o.solver_evm);
        d.push(o.shard);
        d.push(o.decimals);
        d.push(o.state_bump);
        d.extend_from_slice(&[0; 5]);
        d
    }

    #[test]
    fn instruction_data_round_trips_and_is_strict() {
        let d = encode(&order());
        assert_eq!(d.len(), DATA_LEN);
        assert_eq!(Order::parse(&d).unwrap(), order());
        assert_eq!(Order::parse(&d[..DATA_LEN - 1]), Err(FillError::BadData.into()));
        let mut long = d.clone();
        long.push(0);
        assert_eq!(Order::parse(&long), Err(FillError::BadData.into()));
        let mut reserved = d.clone();
        reserved[DATA_LEN - 1] = 1;
        assert_eq!(Order::parse(&reserved), Err(FillError::BadData.into()));
        let mut shard = d.clone();
        shard[124] = SHARDS;
        assert_eq!(Order::parse(&shard), Err(FillError::ShardOutOfRange.into()));
    }

    #[test]
    fn a_token_account_must_be_initialized_for_the_mint() {
        let mint = [0x44u8; 32];
        let mut acct = vec![0u8; TOKEN_ACCOUNT_LEN];
        acct[..32].copy_from_slice(&mint);
        acct[32..64].copy_from_slice(&[0x33; 32]);
        acct[108] = 1; // initialized
        assert_eq!(token_account_owner(&acct, &mint).unwrap(), [0x33; 32]);
        // Another mint, an uninitialized or frozen account, a wrong size: all refused.
        assert!(token_account_owner(&acct, &[0x45; 32]).is_err());
        let mut uninit = acct.clone();
        uninit[108] = 0;
        assert!(token_account_owner(&uninit, &mint).is_err());
        let mut frozen = acct.clone();
        frozen[108] = 2;
        assert!(token_account_owner(&frozen, &mint).is_err());
        assert!(token_account_owner(&acct[..164], &mint).is_err());
    }

    #[test]
    fn transfer_checked_uses_the_spl_layout() {
        let k = Pubkey::new_unique();
        let ix = transfer_checked(&k, &k, &k, &k, &k, 50_000_000, 6);
        assert_eq!(ix.data[0], 12);
        assert_eq!(&ix.data[1..9], &50_000_000u64.to_le_bytes());
        assert_eq!(ix.data[9], 6);
        assert_eq!(ix.accounts.len(), 4);
        assert!(ix.accounts[3].is_signer && !ix.accounts[3].is_writable);
    }
}

#[cfg(test)]
mod canonical_tests {
    use super::*;

    /// A non-canonical bump for `seeds`: the next valid one below the canonical bump.
    fn other_bump(seeds: &[&[u8]], program: &Pubkey) -> Option<(Pubkey, u8)> {
        let (_, canonical) = Pubkey::find_program_address(seeds, program);
        (0..canonical).rev().find_map(|b| {
            let mut s = seeds.to_vec();
            let bb = [b];
            s.push(&bb);
            Pubkey::create_program_address(&s, program).ok().map(|a| (a, b))
        })
    }

    #[test]
    fn a_second_marker_for_the_same_order_is_refused() {
        // The attack: fill an order once with an odd bump (another valid address), then again
        // with the canonical one. Only the canonical marker is accepted, so the second fill of the
        // same order meets an existing marker and fails.
        let program = Pubkey::new_unique();
        let id = [7u8; 32];
        let seeds: &[&[u8]] = &[MARKER_SEED, &id];
        let (canonical, bump) = Pubkey::find_program_address(seeds, &program);
        assert_eq!(check_canonical(seeds, &program, &canonical).unwrap(), bump);
        let (odd, odd_bump) = other_bump(seeds, &program).expect("some other valid bump exists");
        assert_ne!(odd, canonical);
        assert!(odd_bump < bump);
        assert_eq!(check_canonical(seeds, &program, &odd), Err(ProgramError::InvalidSeeds));
    }

    #[test]
    fn only_the_canonical_shard_signs() {
        let program = Pubkey::new_unique();
        let seeds: &[&[u8]] = &[SHARD_SEED, &[3]];
        let (canonical, _) = Pubkey::find_program_address(seeds, &program);
        assert!(check_canonical(seeds, &program, &canonical).is_ok());
        let (odd, _) = other_bump(seeds, &program).expect("some other valid bump exists");
        assert!(check_canonical(seeds, &program, &odd).is_err());
    }
}

#[cfg(test)]
mod id_tests {
    use super::*;

    #[test]
    fn the_token_program_constant_is_the_spl_token_id() {
        assert_eq!(TOKEN_PROGRAM.to_string(), "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
    }
}

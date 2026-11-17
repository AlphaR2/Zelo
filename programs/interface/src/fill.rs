//! The fill instruction's data, the order id, and the message the escrow reads.
//!
//! Order id (must equal `Escrow.orderId`, a `keccak256(abi.encodePacked(...))`):
//! `keccak256( chain_id u64 BE | escrow [20] | user [20] | nonce u64 BE | merchant [32] | mint [32]
//!             | amount_out u64 BE | deadline u64 BE | amount_in [32] BE )`.
//!
//! Instruction data (132 bytes; integers little-endian, except `amount_in`, the EVM's 32-byte
//! big-endian word): `chain_id u64 | escrow [20] | user [20] | nonce u64 | amount_out u64 |
//! deadline u64 | amount_in [32] | solver_evm [20] | shard u8 | decimals u8 | state_bump u8 |
//! reserved [5] (zero)`. The merchant comes from the destination token account, the mint from
//! the mint account.
//!
//! Message (124 bytes): `order_id | solver_evm [20] | amount_out u64 BE | mint [32] | merchant [32]`.

use pinocchio::error::ProgramError;

/// How many emitter shards the fill program signs for. Spreading fills over shards keeps them
/// from all writing one account (Solana runs transactions that write the same account in turn).
/// The escrow lists each shard's emitter account.
pub const SHARDS: u8 = 16;
pub const SHARD_SEED: &[u8] = b"shard";
pub const MARKER_SEED: &[u8] = b"filled";
pub const DATA_LEN: usize = 132;
pub const PAYLOAD_LEN: usize = 124;
pub const TOKEN_ACCOUNT_LEN: usize = 165;
pub const MINT_LEN: usize = 82;
/// Offset of `decimals` in an SPL Token mint.
pub const MINT_DECIMALS: usize = 44;

#[derive(Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum FillError {
    BadData = 1,
    ShardOutOfRange = 2,
    PastDeadline = 3,
    NotTheTokenProgram = 4,
    /// The destination is not an initialized token account of this mint.
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

/// An order, as the fill instruction carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    pub chain_id: u64,
    pub escrow: [u8; 20],
    pub user: [u8; 20],
    pub nonce: u64,
    pub amount_out: u64,
    pub deadline: u64,
    /// What the user locked on the EVM chain, as the escrow hashes it (32-byte big-endian).
    pub amount_in: [u8; 32],
    pub solver_evm: [u8; 20],
    pub shard: u8,
    pub decimals: u8,
    /// The bump of the shard's emitter account in the outbox.
    pub state_bump: u8,
}

impl Order {
    pub fn parse(d: &[u8]) -> Result<Order, ProgramError> {
        if d.len() != DATA_LEN {
            return Err(FillError::BadData.into());
        }
        let u64_at = |i: usize| u64::from_le_bytes(d[i..i + 8].try_into().unwrap());
        let order = Order {
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
        if d[127..].iter().any(|b| *b != 0) {
            return Err(FillError::BadData.into());
        }
        if order.shard >= SHARDS {
            return Err(FillError::ShardOutOfRange.into());
        }
        Ok(order)
    }

    /// The instruction data for this order.
    pub fn encode(&self) -> [u8; DATA_LEN] {
        let mut d = [0u8; DATA_LEN];
        d[0..8].copy_from_slice(&self.chain_id.to_le_bytes());
        d[8..28].copy_from_slice(&self.escrow);
        d[28..48].copy_from_slice(&self.user);
        d[48..56].copy_from_slice(&self.nonce.to_le_bytes());
        d[56..64].copy_from_slice(&self.amount_out.to_le_bytes());
        d[64..72].copy_from_slice(&self.deadline.to_le_bytes());
        d[72..104].copy_from_slice(&self.amount_in);
        d[104..124].copy_from_slice(&self.solver_evm);
        d[124] = self.shard;
        d[125] = self.decimals;
        d[126] = self.state_bump;
        d
    }

    /// The order id: `keccak256` over the packed layout the escrow uses.
    pub fn id(&self, merchant: &[u8; 32], mint: &[u8; 32]) -> [u8; 32] {
        solana_keccak_hasher::hashv(&[
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
    pub fn payload(
        &self,
        id: &[u8; 32],
        merchant: &[u8; 32],
        mint: &[u8; 32],
    ) -> [u8; PAYLOAD_LEN] {
        let mut p = [0u8; PAYLOAD_LEN];
        p[..32].copy_from_slice(id);
        p[32..52].copy_from_slice(&self.solver_evm);
        p[52..60].copy_from_slice(&self.amount_out.to_be_bytes());
        p[60..92].copy_from_slice(mint);
        p[92..124].copy_from_slice(merchant);
        p
    }
}

/// The owner (the merchant) of an initialized SPL Token account of `mint`.
pub fn token_account_owner(data: &[u8], mint: &[u8; 32]) -> Result<[u8; 32], ProgramError> {
    // mint [0..32] | owner [32..64] | amount | delegate | state u8 at 108 (1 = initialized)
    if data.len() != TOKEN_ACCOUNT_LEN || data[..32] != mint[..] || data[108] != 1 {
        return Err(FillError::BadDestination.into());
    }
    Ok(data[32..64].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference values: `cast keccak` over the packed layout the Solidity escrow uses.
    const ORDER_ID: &str = "784041714c70f811a1a12cdbf70ccfff036a5463c7dce02864f1624e51f79de4";
    const PAYLOAD: &str = "784041714c70f811a1a12cdbf70ccfff036a5463c7dce02864f1624e51f79de455555555555555555555555555555555555555550000000002faf08044444444444444444444444444444444444444444444444444444444444444443333333333333333333333333333333333333333333333333333333333333333";
    const MERCHANT: [u8; 32] = [0x33; 32];
    const MINT: [u8; 32] = [0x44; 32];

    fn amount_in(v: u128) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[16..].copy_from_slice(&v.to_be_bytes());
        w
    }

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

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn the_order_id_matches_the_escrows_packed_layout() {
        assert_eq!(hex(&order().id(&MERCHANT, &MINT)), ORDER_ID);
    }

    #[test]
    fn the_payload_is_the_124_bytes_the_escrow_reads() {
        let o = order();
        let p = o.payload(&o.id(&MERCHANT, &MINT), &MERCHANT, &MINT);
        assert_eq!(hex(&p), PAYLOAD);
    }

    #[test]
    fn every_order_field_changes_the_id() {
        let base = order().id(&MERCHANT, &MINT);
        let changed: [fn(&mut Order); 7] = [
            |o| o.chain_id += 1,
            |o| o.escrow[0] ^= 1,
            |o| o.user[0] ^= 1,
            |o| o.nonce += 1,
            |o| o.amount_out += 1,
            |o| o.deadline += 1,
            |o| o.amount_in = amount_in(99_000_000),
        ];
        for change in changed {
            let mut o = order();
            change(&mut o);
            assert_ne!(o.id(&MERCHANT, &MINT), base);
        }
        assert_ne!(order().id(&[0x34; 32], &MINT), base, "merchant");
        assert_ne!(order().id(&MERCHANT, &[0x45; 32]), base, "mint");
        // Not in the id, so any solver may fill, on any shard.
        let mut o = order();
        o.solver_evm[0] ^= 1;
        o.shard = 9;
        o.state_bump = 1;
        assert_eq!(o.id(&MERCHANT, &MINT), base);
    }

    #[test]
    fn instruction_data_round_trips_and_is_strict() {
        let d = order().encode();
        assert_eq!(Order::parse(&d).unwrap(), order());
        let bad_data = Err(FillError::BadData.into());
        assert_eq!(Order::parse(&d[..DATA_LEN - 1]), bad_data);
        assert_eq!(Order::parse(&[&d[..], &[0]].concat()), bad_data);
        let mut reserved = d;
        reserved[DATA_LEN - 1] = 1;
        assert_eq!(Order::parse(&reserved), bad_data);
        let mut shard = d;
        shard[124] = SHARDS;
        assert_eq!(Order::parse(&shard), Err(FillError::ShardOutOfRange.into()));
    }

    #[test]
    fn a_token_account_must_be_initialized_for_the_mint() {
        let mut acct = [0u8; TOKEN_ACCOUNT_LEN];
        acct[..32].copy_from_slice(&MINT);
        acct[32..64].copy_from_slice(&MERCHANT);
        acct[108] = 1;
        assert_eq!(token_account_owner(&acct, &MINT), Ok(MERCHANT));
        assert!(
            token_account_owner(&acct, &[0x45; 32]).is_err(),
            "another mint"
        );
        for state in [0, 2] {
            let mut a = acct;
            a[108] = state;
            assert!(
                token_account_owner(&a, &MINT).is_err(),
                "uninitialized or frozen"
            );
        }
        assert!(
            token_account_owner(&acct[..164], &MINT).is_err(),
            "wrong size"
        );
    }
}

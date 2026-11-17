//! The bank hash and the accounts lattice hash: what a block's footer commits to about the
//! chain's state (explained in `docs/how-it-works.md`, "Proving state").
//!
//! Reimplemented from Agave 4.4 (`runtime/src/bank.rs::hash_internal_state`,
//! `accounts-db/src/accounts_db.rs::lt_hash_account`, SIMD-0215) so it needs nothing beyond
//! SHA-256 and BLAKE3, and runs in the zkVM.
//!
//! ```text
//! inner     = sha256( parent_bank_hash | signature_count u64 LE | last_blockhash )
//! bank_hash = sha256( inner | accounts_lt_hash (2,048 bytes) )
//! h(acct)   = 0^2048 if lamports == 0, else
//!             BLAKE3-XOF-2048( lamports u64 LE | data | executable u8 | owner | pubkey )
//! L         = sum of h(acct) over all accounts, element-wise on 1,024 u16, mod 2^16
//! ```
//!
//! A hard-fork slot adds one more hashing step; it is not supported (the caller's check
//! against the footer's bank hash simply fails there).

use sha2::{Digest, Sha256};

pub type Hash = [u8; 32];

/// Number of u16 elements in a lattice hash (2,048 bytes).
pub const LT_ELEMENTS: usize = 1024;
pub const LT_BYTES: usize = LT_ELEMENTS * 2;

/// A lattice hash: 1,024 u16 values. Addition and subtraction are element-wise, wrapping.
#[derive(Clone, PartialEq, Eq)]
pub struct LtHash(pub [u16; LT_ELEMENTS]);

impl core::fmt::Debug for LtHash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "LtHash(checksum {:02x?})", &self.checksum()[..8])
    }
}

impl LtHash {
    pub const IDENTITY: LtHash = LtHash([0; LT_ELEMENTS]);

    pub fn add(&mut self, other: &LtHash) {
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a = a.wrapping_add(*b);
        }
    }

    pub fn sub(&mut self, other: &LtHash) {
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a = a.wrapping_sub(*b);
        }
    }

    /// The 2,048 bytes the bank hash covers: each element little-endian.
    pub fn to_bytes(&self) -> [u8; LT_BYTES] {
        let mut out = [0u8; LT_BYTES];
        for (i, v) in self.0.iter().enumerate() {
            out[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
        }
        out
    }

    pub fn from_bytes(b: &[u8; LT_BYTES]) -> LtHash {
        let mut out = [0u16; LT_ELEMENTS];
        for (i, v) in out.iter_mut().enumerate() {
            *v = u16::from_le_bytes([b[2 * i], b[2 * i + 1]]);
        }
        LtHash(out)
    }

    /// BLAKE3 of the 2,048 bytes: what Agave logs as "accounts_lt_hash checksum".
    pub fn checksum(&self) -> Hash {
        *blake3::hash(&self.to_bytes()).as_bytes()
    }
}

/// One account's lattice hash. An account with no lamports contributes nothing.
pub fn account_lt_hash(
    lamports: u64,
    data: &[u8],
    executable: bool,
    owner: &[u8; 32],
    pubkey: &[u8; 32],
) -> LtHash {
    if lamports == 0 {
        return LtHash::IDENTITY;
    }
    let mut h = blake3::Hasher::new();
    h.update(&lamports.to_le_bytes());
    h.update(data);
    h.update(&[u8::from(executable)]);
    h.update(owner);
    h.update(pubkey);
    let mut out = [0u8; LT_BYTES];
    h.finalize_xof().fill(&mut out);
    LtHash::from_bytes(&out)
}

/// The first of the two hashes: the parent's bank hash, this block's signature count and its
/// last blockhash.
pub fn inner_hash(parent_bank_hash: &Hash, signature_count: u64, last_blockhash: &Hash) -> Hash {
    let mut h = Sha256::new();
    h.update(parent_bank_hash);
    h.update(signature_count.to_le_bytes());
    h.update(last_blockhash);
    h.finalize().into()
}

/// The bank hash from the inner hash and the accounts lattice hash after the block.
pub fn bank_hash(inner: &Hash, lt: &LtHash) -> Hash {
    let mut h = Sha256::new();
    h.update(inner);
    h.update(lt.to_bytes());
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_lamports_is_the_identity() {
        let h = account_lt_hash(0, b"data", true, &[1; 32], &[2; 32]);
        assert_eq!(h, LtHash::IDENTITY);
    }

    #[test]
    fn add_then_sub_cancels_and_order_does_not_matter() {
        let a = account_lt_hash(5, b"a", false, &[1; 32], &[2; 32]);
        let b = account_lt_hash(7, b"bb", true, &[3; 32], &[4; 32]);
        let mut ab = LtHash::IDENTITY;
        ab.add(&a);
        ab.add(&b);
        let mut ba = LtHash::IDENTITY;
        ba.add(&b);
        ba.add(&a);
        assert_eq!(ab, ba);
        ab.sub(&b);
        assert_eq!(ab, a);
        ab.sub(&a);
        assert_eq!(ab, LtHash::IDENTITY);
    }

    #[test]
    fn every_field_and_the_address_change_the_hash() {
        let base = account_lt_hash(5, b"a", false, &[1; 32], &[2; 32]);
        assert_ne!(base, account_lt_hash(6, b"a", false, &[1; 32], &[2; 32]));
        assert_ne!(base, account_lt_hash(5, b"b", false, &[1; 32], &[2; 32]));
        assert_ne!(base, account_lt_hash(5, b"a", true, &[1; 32], &[2; 32]));
        assert_ne!(base, account_lt_hash(5, b"a", false, &[9; 32], &[2; 32]));
        assert_ne!(base, account_lt_hash(5, b"a", false, &[1; 32], &[9; 32]));
    }

    /// Agave's own test vector (`lattice-hash/src/lt_hash.rs::test_hello_world`): the XOF
    /// bytes read as little-endian u16s, and the checksum of those 2,048 bytes.
    #[test]
    fn matches_agaves_published_checksum_for_hello() {
        let mut h = blake3::Hasher::new();
        h.update(b"hello");
        let mut out = [0u8; LT_BYTES];
        h.finalize_xof().fill(&mut out);
        let lt = LtHash::from_bytes(&out);
        assert_eq!(lt.0[0], u16::from_le_bytes([out[0], out[1]]));
        assert_eq!(
            lt.checksum(),
            [
                79, 156, 26, 184, 156, 205, 94, 208, 182, 235, 33, 147, 111, 153, 229, 152, 207,
                133, 75, 109, 182, 198, 119, 61, 11, 81, 41, 70, 24, 87, 100, 85
            ]
        );
    }

    #[test]
    fn bytes_round_trip() {
        let a = account_lt_hash(5, b"a", false, &[1; 32], &[2; 32]);
        assert_eq!(LtHash::from_bytes(&a.to_bytes()), a);
    }
}

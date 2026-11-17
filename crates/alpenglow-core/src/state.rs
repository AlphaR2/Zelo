//! Proving what a Solana block changed (explained in `docs/how-it-works.md`, "Proving state").
//!
//! The statement: "block B is final, and these accounts changed in it, from these values to
//! these values." Solana signs the block (the certificate), the block's footer carries its bank
//! hash, and the bank hash commits to the accounts lattice hash after the block. So:
//!
//! 1. B is final: a certificate for B or a descendant D, and the parent links between them
//!    (`proof::verify_ancestry`).
//! 2. B's footer is read from B's own shreds, at the only position replay accepts: the
//!    second-to-last component, followed by the one tick that ends the slot.
//! 3. The bank hash opens: the footer's hash is `sha256(inner_B | L_B)` and `inner_B` commits
//!    to the parent's bank hash, which commits to `L_P`.
//! 4. The change set opens: `L_P` plus every changed account's new hash minus its old hash
//!    equals `L_B`. A false change set would be a collision in the lattice hash.
//! 5. The change set is committed as a Merkle tree, the *diff root*, so one proof serves
//!    every account the block changed, and "X did not change" is provable too.
//! 6. Optionally the footers of descendants of B are opened as well. B's own footer
//!    sits behind the 20-byte Merkle nodes of the shred tree (about 2^80 to forge by the
//!    block's own leader); a descendant's bank hash commits to B's with full 32-byte hashes,
//!    so a forger would have to lead every block up to the highest footer opened.

use {
    crate::{
        bank::{LT_ELEMENTS, LtHash, bank_hash, inner_hash},
        block::{self, Component, Marker},
        proof::{AncestryLink, MAX_ANCESTRY},
        shred::{DATA_SHREDS_PER_FEC_SET, Hash, Kind, Shred},
    },
    serde::{Deserialize, Serialize},
    sha2::{Digest, Sha256},
};

/// The most accounts one proof may list as changed.
pub const MAX_CHANGES: usize = 1 << 16;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("footer run: {0}")]
    FooterRun(&'static str),
    #[error(transparent)]
    Shred(#[from] crate::shred::Error),
    #[error(transparent)]
    Block(#[from] block::Error),
    #[error("the block's own FEC roots and parent do not rebuild its block_id")]
    BlockIdMismatch,
    #[error("a lattice hash must have {LT_ELEMENTS} elements, got {0}")]
    LtLength(usize),
    #[error("the bank hash rebuilt from the witness is not the one in the footer (block {0})")]
    BankHashMismatch(u64),
    #[error("the changed accounts do not turn the parent's lattice hash into the block's")]
    DeltaMismatch,
    #[error("changed accounts must be listed with strictly increasing keys (entry {0})")]
    Unsorted(usize),
    #[error("{0} changed accounts is more than the {MAX_CHANGES} a proof may list")]
    TooManyChanges(usize),
    #[error("{0} confirmations, but only {1} blocks lie between the certified block and this one")]
    TooManyConfirmations(usize, usize),
    #[error("the statement needs a footer for slot {0}, no ancestry link describes it")]
    NoLinkFor(u64),
    #[error("an ancestry path of {0} blocks does not fit {1} links")]
    PathMismatch(usize, usize),
}

/// An account's value (`None` in a change means absent; zero lamports counts as absent).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub lamports: u64,
    pub data: Vec<u8>,
    pub executable: bool,
    pub owner: Hash,
}

fn live(a: &Option<Account>) -> Option<&Account> {
    a.as_ref().filter(|a| a.lamports != 0)
}

impl Account {
    /// The part of the diff leaf that describes this value (the address is in the leaf).
    pub fn digest(&self) -> Hash {
        let mut h = Sha256::new();
        h.update(self.lamports.to_le_bytes());
        h.update(&self.data);
        h.update([u8::from(self.executable)]);
        h.update(self.owner);
        h.finalize().into()
    }
}

/// `sha256(lamports | data | executable | owner)`, or zeros for an absent account.
pub fn account_digest(a: &Option<Account>) -> Hash {
    live(a).map_or([0; 32], Account::digest)
}

fn lt_of(pubkey: &Hash, a: &Option<Account>) -> LtHash {
    live(a).map_or(LtHash::IDENTITY, |a| {
        crate::bank::account_lt_hash(a.lamports, &a.data, a.executable, &a.owner, pubkey)
    })
}

/// One account that changed in the block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountChange {
    pub pubkey: Hash,
    pub pre: Option<Account>,
    pub post: Option<Account>,
}

/// The block's last two components as shreds (the footer, then the tick that ends the slot),
/// and the shred just before them, which must close the component before.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FooterRun {
    pub boundary_shred: Vec<u8>,
    pub shreds: Vec<Vec<u8>>,
}

/// What a bank hash commits to besides its parent: the block's signature count and last
/// blockhash, and the accounts lattice hash after the block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BankParts {
    pub signature_count: u64,
    pub last_blockhash: Hash,
    /// 1,024 u16 values.
    pub lt_hash: Vec<u16>,
}

impl BankParts {
    fn lt(&self) -> Result<LtHash, Error> {
        let a: [u16; LT_ELEMENTS] = self
            .lt_hash
            .clone()
            .try_into()
            .map_err(|v: Vec<u16>| Error::LtLength(v.len()))?;
        Ok(LtHash(a))
    }
}

/// A descendant of B whose footer is opened too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Confirmation {
    pub footer: FooterRun,
    pub parts: BankParts,
}

/// Everything the program needs to prove B's change set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateWitness {
    /// B's own FEC roots and parent: rebuild B's block_id, and place the footer shreds.
    pub fec_roots: Vec<Hash>,
    pub parent_slot: u64,
    pub parent_block_id: Hash,
    pub footer: FooterRun,
    /// The inner hash of B's parent bank, and the parent's lattice hash.
    pub inner_parent: Hash,
    pub lt_parent: Vec<u16>,
    pub block: BankParts,
    /// Every account whose value differs at B's two boundaries, strictly increasing by key.
    pub changes: Vec<AccountChange>,
    /// Footers of B's descendants, nearest first: the first is B's child. At most as many as
    /// the ancestry has links.
    pub confirmations: Vec<Confirmation>,
}

/// What a verified state witness establishes about block B.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateClaim {
    pub bank_hash: Hash,
    pub diff_root: Hash,
    pub leaf_count: u32,
    /// Descendant footers opened, the number of distinct later leader windows they cover, and
    /// the slot of the highest one (B's own slot when none).
    pub confirmations: u16,
    pub windows: u16,
    pub top_slot: u64,
}

fn sha(parts: &[&[u8]]) -> Hash {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

// ---------------------------------------------------------------------------------------
// the diff root

/// `sha256(0x00 | pubkey | digest(pre) | digest(post))`.
pub fn diff_leaf(pubkey: &Hash, pre: &Option<Account>, post: &Option<Account>) -> Hash {
    sha(&[&[0x00], pubkey, &account_digest(pre), &account_digest(post)])
}

fn node(l: &Hash, r: &Hash) -> Hash {
    sha(&[&[0x01], l, r])
}

fn level_up(level: &[Hash]) -> Vec<Hash> {
    level
        .chunks(2)
        .map(|p| {
            if p.len() == 2 {
                node(&p[0], &p[1])
            } else {
                p[0]
            }
        })
        .collect()
}

/// The Merkle root over leaves (an odd last node is promoted unchanged), all zeros if empty.
pub fn merkle_root(leaves: &[Hash]) -> Hash {
    let mut level = leaves.to_vec();
    if level.is_empty() {
        return [0; 32];
    }
    while level.len() > 1 {
        level = level_up(&level);
    }
    level[0]
}

/// `sha256(0x02 | leaf_count u32 LE | merkle_root)`: what goes in the public values.
pub fn diff_root(leaves: &[Hash]) -> Hash {
    sha(&[
        &[0x02],
        &(leaves.len() as u32).to_le_bytes(),
        &merkle_root(leaves),
    ])
}

/// The sibling hashes from leaf `index` to the root, bottom first. A level's last node with
/// no sibling contributes none (it is promoted).
pub fn merkle_path(leaves: &[Hash], mut index: usize) -> Vec<Hash> {
    let mut level = leaves.to_vec();
    let mut path = Vec::new();
    while level.len() > 1 {
        let sib = index ^ 1;
        if sib < level.len() {
            path.push(level[sib]);
        }
        level = level_up(&level);
        index /= 2;
    }
    path
}

/// Recomputes `diff_root` from a leaf and its path, for a tree of `count` leaves.
pub fn root_from_path(leaf: &Hash, mut index: usize, count: usize, path: &[Hash]) -> Option<Hash> {
    if index >= count {
        return None;
    }
    let (mut cur, mut width) = (*leaf, count);
    let mut it = path.iter();
    while width > 1 {
        if (index ^ 1) < width {
            let sib = it.next()?;
            cur = if index.is_multiple_of(2) {
                node(&cur, sib)
            } else {
                node(sib, &cur)
            };
        }
        index /= 2;
        width = width.div_ceil(2);
    }
    if it.next().is_some() {
        return None;
    }
    Some(sha(&[&[0x02], &(count as u32).to_le_bytes(), &cur]))
}

// ---------------------------------------------------------------------------------------
// the footer

fn in_block(s: &Shred, slot: u64, version: u16, roots: &[Hash]) -> Result<(), Error> {
    let set = s.fec_set_index() as usize / DATA_SHREDS_PER_FEC_SET;
    let ok = s.kind() == Kind::Data
        && s.slot() == slot
        && s.version() == version
        && (s.fec_set_index() as usize).is_multiple_of(DATA_SHREDS_PER_FEC_SET)
        && s.erasure_index() < DATA_SHREDS_PER_FEC_SET
        && roots.get(set) == Some(&s.merkle_root()?);
    ok.then_some(())
        .ok_or(Error::FooterRun("a shred is not in the block"))
}

/// The bank hash in the footer of the block `(slot, fec_roots)`: found in its last two
/// components, footer then alpentick, and nowhere else.
fn footer_bank_hash(
    slot: u64,
    version: u16,
    roots: &[Hash],
    run: &FooterRun,
) -> Result<Hash, Error> {
    let boundary = Shred::parse(&run.boundary_shred)?;
    in_block(&boundary, slot, version, roots)?;
    if !boundary.data_complete() {
        return Err(Error::FooterRun(
            "the shred before the run does not end a component",
        ));
    }
    let shreds = run
        .shreds
        .iter()
        .map(|b| Shred::parse(b))
        .collect::<Result<Vec<_>, _>>()?;
    let first = shreds.first().ok_or(Error::FooterRun("no shreds"))?;
    if boundary.index().checked_add(1) != Some(first.index()) {
        return Err(Error::FooterRun(
            "the run does not start right after its boundary",
        ));
    }
    for s in &shreds {
        in_block(s, slot, version, roots)?;
    }
    if !shreds.last().is_some_and(Shred::last_in_slot) {
        return Err(Error::FooterRun("the run does not end the slot"));
    }
    // `deshred` checks the run is consecutive and ends on a DATA_COMPLETE shred.
    let components = block::deshred(&shreds)?;
    let [footer, tick] = components.as_slice() else {
        return Err(Error::FooterRun(
            "expected exactly a footer and the closing tick",
        ));
    };
    let Component::Marker(Marker::Footer(bytes)) = block::parse_component(footer)? else {
        return Err(Error::FooterRun("the first component is not a footer"));
    };
    match block::parse_component(tick)? {
        Component::Entries(e) if matches!(e.as_slice(), [t] if t.num_hashes == 1 && t.transactions.is_empty()) =>
            {}
        _ => {
            return Err(Error::FooterRun(
                "the last component is not the single closing tick",
            ));
        }
    }
    Ok(block::parse_footer(bytes)?.bank_hash)
}

// ---------------------------------------------------------------------------------------
// the statement

/// Leader windows are four consecutive slots with one leader.
const WINDOW: u64 = 4;

impl StateWitness {
    /// Verifies the witness for block `slot_b` with block_id `id_b` (already known to be final),
    /// given the ancestry `path` from the certified block D down to B (`path[0]` is D, the last
    /// is B) and the links that built it.
    pub fn verify(
        &self,
        slot_b: u64,
        id_b: &Hash,
        shred_version: u16,
        links: &[AncestryLink],
        path: &[(u64, Hash)],
    ) -> Result<StateClaim, Error> {
        // B's own roots and parent rebuild its block_id.
        if crate::shred::block_id(&self.fec_roots, self.parent_slot, &self.parent_block_id)?
            != *id_b
        {
            return Err(Error::BlockIdMismatch);
        }
        // 5.2: the footer, and the bank hash it carries.
        let footer_hash = footer_bank_hash(slot_b, shred_version, &self.fec_roots, &self.footer)?;

        // 5.3: open the bank hash down to the parent's lattice hash.
        let lt_parent = LtHash(
            self.lt_parent
                .clone()
                .try_into()
                .map_err(|v: Vec<u16>| Error::LtLength(v.len()))?,
        );
        let lt_block = self.block.lt()?;
        let bank_hash_parent = bank_hash(&self.inner_parent, &lt_parent);
        let inner_block = inner_hash(
            &bank_hash_parent,
            self.block.signature_count,
            &self.block.last_blockhash,
        );
        if bank_hash(&inner_block, &lt_block) != footer_hash {
            return Err(Error::BankHashMismatch(slot_b));
        }

        // 5.4: the change set turns the parent's lattice hash into the block's.
        if self.changes.len() > MAX_CHANGES {
            return Err(Error::TooManyChanges(self.changes.len()));
        }
        let mut sum = lt_parent;
        let mut leaves = Vec::new();
        for (i, c) in self.changes.iter().enumerate() {
            if i > 0 && self.changes[i - 1].pubkey >= c.pubkey {
                return Err(Error::Unsorted(i));
            }
            let (old, new) = (lt_of(&c.pubkey, &c.pre), lt_of(&c.pubkey, &c.post));
            sum.sub(&old);
            sum.add(&new);
            // 5.5: only accounts whose value really differs become leaves.
            if old != new {
                leaves.push(diff_leaf(&c.pubkey, &c.pre, &c.post));
            }
        }
        if sum != lt_block {
            return Err(Error::DeltaMismatch);
        }

        // 5.1b: descendants' footers, each committing to the one below by a full-width hash.
        let k = links.len();
        if self.confirmations.len() > k {
            return Err(Error::TooManyConfirmations(self.confirmations.len(), k));
        }
        // A real check, not a debug assertion: assertions are compiled out of the zkVM build.
        if k > MAX_ANCESTRY || path.len() != k + 1 {
            return Err(Error::PathMismatch(path.len(), k));
        }
        let mut below = footer_hash;
        let mut seen: Vec<u64> = Vec::new();
        let mut top = slot_b;
        for (i, c) in self.confirmations.iter().enumerate() {
            // C_{i+1} is the block `i + 1` above B: path[k - (i + 1)], described by links[k - (i + 1)].
            let j = k - (i + 1);
            let (slot_c, _) = path[j];
            let link = links.get(j).ok_or(Error::NoLinkFor(slot_c))?;
            let want = footer_bank_hash(slot_c, shred_version, &link.fec_roots, &c.footer)?;
            let inner = inner_hash(&below, c.parts.signature_count, &c.parts.last_blockhash);
            if bank_hash(&inner, &c.parts.lt()?) != want {
                return Err(Error::BankHashMismatch(slot_c));
            }
            below = want;
            top = slot_c;
            let w = slot_c / WINDOW;
            if w != slot_b / WINDOW && !seen.contains(&w) {
                seen.push(w);
            }
        }

        Ok(StateClaim {
            bank_hash: footer_hash,
            diff_root: diff_root(&leaves),
            leaf_count: leaves.len() as u32,
            confirmations: self.confirmations.len() as u16,
            windows: seen.len() as u16,
            top_slot: top,
        })
    }
}

//! What the SP1 program proves, as plain functions. Native tests call the same functions,
//! so the zkVM and the tests run identical code.
//!
//! Three statements, each building on the last:
//! - [`FinalityInput::verify`]: "block `block_id` at `slot` is final under this validator set".
//! - [`BlockInput::verify`]: plus "these data shreds are that whole block".
//! - [`TxInput::verify`]: plus "this transaction is in that block". Needs only the shreds of
//!   the transaction's own component and every FEC set root, not the whole block.

use {
    crate::{
        block,
        finality::{self, CertType},
        shred::{self, DATA_SHREDS_PER_FEC_SET, Hash, Shred},
        state::{self, StateClaim, StateWitness},
        tx,
    },
    serde::{Deserialize, Serialize},
    sha2::{Digest, Sha256},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validator {
    /// 48-byte compressed BLS key (Vec, as serde has no `[u8; 48]`).
    pub bls_pubkey: Vec<u8>,
    pub stake: u64,
}

/// A compressed (96-byte) aggregate signature and the signer bitmap it covers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Aggregate {
    pub signature: Vec<u8>,
    pub bitmap: Vec<u8>,
}

/// A block footer's finality certificate plus the set to check it against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalityInput {
    /// The certificate epoch's ranked set (rank = index), as committed by [`validator_set_hash`].
    pub validators: Vec<Validator>,
    pub shred_version: u16,
    pub slot: u64,
    pub block_id: Hash,
    /// FinalizeFast when `notar_aggregate` is None; else the Finalize part of the slow path.
    pub final_aggregate: Aggregate,
    pub notar_aggregate: Option<Aggregate>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Path {
    /// One FinalizeFast certificate: 80% notarized the block.
    Fast,
    /// Notarize(block) at 60% plus Finalize(slot) at 60%.
    Slow,
}

/// What a verified finality certificate establishes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finality {
    pub validator_set_hash: Hash,
    pub shred_version: u16,
    pub slot: u64,
    pub block_id: Hash,
    pub path: Path,
    /// Stake behind the deciding certificate (the Finalize part on the slow path).
    pub signed_stake: u64,
    pub total_stake: u64,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("validator {0}: key must be 48 bytes")]
    KeyLength(usize),
    #[error("signature must be 96 bytes")]
    SignatureLength,
    #[error("validator set has zero total stake")]
    EmptySet,
    #[error(transparent)]
    Finality(#[from] finality::Error),
    #[error(transparent)]
    Shred(#[from] shred::Error),
    #[error(transparent)]
    Block(#[from] block::Error),
    #[error("shreds rebuild block_id {computed:02x?}, cert signed {certified:02x?}")]
    BlockIdMismatch { computed: Hash, certified: Hash },
    #[error("shred has version {0}, certificate uses {1}")]
    ShredVersion(u16, u16),
    #[error("shred {0} does not belong to the block (its FEC root is not the committed one)")]
    NotInBlock(u32),
    #[error("shred run does not start at a component boundary")]
    NotAtBoundary,
    #[error("transaction signature must be 64 bytes")]
    TxSignatureLength,
    #[error("transaction not found in the given shreds")]
    TxNotFound,
    #[error("ancestry has {0} links, at most {max} are allowed", max = MAX_ANCESTRY)]
    AncestryTooLong(usize),
    #[error("the transaction's signature {0} does not verify")]
    TxSignature(usize),
    #[error(transparent)]
    State(#[from] state::Error),
    #[error("a proof must make at least one claim")]
    NoClaims,
    #[error("ancestry link: parent slot {parent_slot} is not below slot {slot}")]
    AncestryOrder { slot: u64, parent_slot: u64 },
}

/// The validator set commitment. A destination chain stores this per epoch.
///
/// `sha256("huginn-validator-set-v1" | count u32 LE | for each, in rank order: bls_pubkey[48] | stake u64 LE)`
///
/// Rank order is part of the commitment, because certificate bitmaps index by rank.
pub fn validator_set_hash(validators: &[Validator]) -> Hash {
    let mut h = Sha256::new();
    h.update(b"huginn-validator-set-v1");
    h.update((validators.len() as u32).to_le_bytes());
    for v in validators {
        h.update(&v.bls_pubkey);
        h.update(v.stake.to_le_bytes());
    }
    h.finalize().into()
}

fn signature(a: &Aggregate) -> Result<bls12_381::G2Affine, Error> {
    let bytes: &[u8; 96] = a
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| Error::SignatureLength)?;
    Ok(finality::signature_from_compressed(bytes)?)
}

impl FinalityInput {
    /// Checks the certificate. Fast path: FinalizeFast(block) at 80%. Slow path:
    /// Notarize(block) at 60% *and* Finalize(slot) at 60%; Finalize names no block, so the
    /// Notarize part is what ties finality to `block_id`.
    pub fn verify(&self) -> Result<Finality, Error> {
        if let Some(i) = self
            .validators
            .iter()
            .position(|v| v.bls_pubkey.len() != 48)
        {
            return Err(Error::KeyLength(i));
        }
        let total_stake = self
            .validators
            .iter()
            .try_fold(0u64, |a, v| a.checked_add(v.stake))
            .ok_or(Error::EmptySet)?;
        if total_stake == 0 {
            return Err(Error::EmptySet);
        }
        // Keys are decompressed only for ranks that signed, and at most once each (the slow
        // path checks two certificates over mostly the same signers): decompression is the
        // costliest per-validator step in the zkVM. A key that fails to decode is reported
        // as a missing rank; Agave never ranks such a key (validator_set drops invalid keys).
        let mut decoded: Vec<Option<bls12_381::G1Affine>> = vec![None; self.validators.len()];
        let mut check = |t: CertType, a: &Aggregate| {
            let rank_map = |r: usize| {
                let v = self.validators.get(r)?;
                let key = match decoded[r] {
                    Some(k) => k,
                    None => *decoded[r].insert(
                        finality::pubkey_from_compressed(v.bls_pubkey.as_slice().try_into().ok()?)
                            .ok()?,
                    ),
                };
                Some((v.stake, key))
            };
            finality::verify_cert(
                &t,
                self.shred_version,
                &signature(a)?,
                &a.bitmap,
                self.validators.len(),
                total_stake,
                rank_map,
            )
            .map_err(Error::from)
        };
        let (slot, block_id) = (self.slot, self.block_id);
        let (path, signed_stake) = match &self.notar_aggregate {
            None => (
                Path::Fast,
                check(
                    CertType::FinalizeFast { slot, block_id },
                    &self.final_aggregate,
                )?,
            ),
            Some(notar) => {
                check(CertType::Notarize { slot, block_id }, notar)?;
                (
                    Path::Slow,
                    check(CertType::Finalize { slot }, &self.final_aggregate)?,
                )
            }
        };
        Ok(Finality {
            validator_set_hash: validator_set_hash(&self.validators),
            shred_version: self.shred_version,
            slot,
            block_id,
            path,
            signed_stake,
            total_stake,
        })
    }
}

/// A whole block: finality plus every data shred.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockInput {
    pub finality: FinalityInput,
    /// The parent goes into the block_id's last leaf, so a matching block_id proves it too.
    pub parent_slot: u64,
    pub parent_block_id: Hash,
    pub data_shreds: Vec<Vec<u8>>,
}

impl BlockInput {
    pub fn verify(&self) -> Result<Finality, Error> {
        let finality = self.finality.verify()?;
        self.verify_shreds()?;
        Ok(finality)
    }

    /// The data shreds rebuild exactly the certified `block_id`.
    pub fn verify_shreds(&self) -> Result<(), Error> {
        let f = &self.finality;
        let shreds = self
            .data_shreds
            .iter()
            .map(|b| Shred::parse(b))
            .collect::<Result<Vec<_>, _>>()?;
        check_versions(&shreds, f.shred_version)?;
        let roots = shred::fec_roots(f.slot, &shreds)?;
        let computed = shred::block_id(&roots, self.parent_slot, &self.parent_block_id)?;
        if computed != f.block_id {
            return Err(Error::BlockIdMismatch {
                computed,
                certified: f.block_id,
            });
        }
        Ok(())
    }
}

fn check_versions(shreds: &[Shred], v: u16) -> Result<(), Error> {
    match shreds.iter().find(|s| s.version() != v) {
        Some(s) => Err(Error::ShredVersion(s.version(), v)),
        None => Ok(()),
    }
}

/// Proof that one transaction is in a block, without the whole block.
///
/// - `fec_roots` + parent rebuild the block_id (a few dozen hashes, even for big blocks).
/// - `shreds` is a consecutive run of data shreds holding whole components, one of which
///   contains the transaction. Each shred proves itself against `fec_roots` with the
///   Merkle proof it carries.
/// - `boundary_shred` is the shred just before the run (absent when the run starts at
///   index 0). It must be in the block and flagged DATA_COMPLETE, so the run starts at a
///   real component boundary and can't be read out of frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxInclusion {
    pub fec_roots: Vec<Hash>,
    pub parent_slot: u64,
    pub parent_block_id: Hash,
    pub boundary_shred: Option<Vec<u8>>,
    pub shreds: Vec<Vec<u8>>,
    /// The transaction's first signature (its id).
    pub signature: Vec<u8>,
}

/// Where the transaction was found, and its exact bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncludedTx {
    /// Index of the first data shred of the component holding the transaction, then the
    /// entry and transaction index inside that component.
    pub component_start_shred: u32,
    pub entry_index: u32,
    pub tx_index: u32,
    pub raw: Vec<u8>,
}

impl TxInclusion {
    /// Checks the inclusion against a block_id that is already known to be final.
    pub fn verify(
        &self,
        slot: u64,
        block_id: &Hash,
        shred_version: u16,
    ) -> Result<IncludedTx, Error> {
        let computed = shred::block_id(&self.fec_roots, self.parent_slot, &self.parent_block_id)?;
        if computed != *block_id {
            return Err(Error::BlockIdMismatch {
                computed,
                certified: *block_id,
            });
        }
        let sig: [u8; 64] = self
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| Error::TxSignatureLength)?;
        let shreds = self
            .shreds
            .iter()
            .map(|b| Shred::parse(b))
            .collect::<Result<Vec<_>, _>>()?;
        let first = shreds.first().ok_or(Error::TxNotFound)?;
        let in_block = |s: &Shred| -> Result<(), Error> {
            let set = s.fec_set_index() as usize / DATA_SHREDS_PER_FEC_SET;
            let ok = s.kind() == shred::Kind::Data
                && s.slot() == slot
                && s.version() == shred_version
                && (s.fec_set_index() as usize).is_multiple_of(DATA_SHREDS_PER_FEC_SET)
                && s.erasure_index() < DATA_SHREDS_PER_FEC_SET
                && self.fec_roots.get(set) == Some(&s.merkle_root()?);
            ok.then_some(()).ok_or(Error::NotInBlock(s.index()))
        };
        for s in &shreds {
            in_block(s)?;
        }
        match &self.boundary_shred {
            None if first.index() == 0 => {}
            Some(b) => {
                let b = Shred::parse(b)?;
                in_block(&b)?;
                if !b.data_complete() || b.index().checked_add(1) != Some(first.index()) {
                    return Err(Error::NotAtBoundary);
                }
            }
            None => return Err(Error::NotAtBoundary),
        }
        // `deshred` checks the run is consecutive and ends on a DATA_COMPLETE shred.
        let components = block::deshred(&shreds)?;
        let mut start = first.index();
        let mut unsigned = None;
        for bytes in &components {
            // Only the bytes up to the transaction are parsed (`find_in_component`). A
            // component that fails to parse before reaching it simply doesn't count:
            // components are framed by the shreds' DATA_COMPLETE flags, not by each other.
            if let Ok(Some((e, t, tx))) = block::find_in_component(bytes, &sig) {
                // Replay would have refused a transaction with a bad signature, but not every
                // component is replayed (the prefix before an UpdateParent marker is not), so
                // the proof checks the signatures itself. A transaction that fails does not
                // count, like one that fails to parse: another component may hold the real one.
                match tx.verify_signatures() {
                    Ok(()) => {
                        return Ok(IncludedTx {
                            component_start_shred: start,
                            entry_index: e as u32,
                            tx_index: t as u32,
                            raw: tx.raw.to_vec(),
                        });
                    }
                    Err(tx::Error::BadSignature(i)) => unsigned = Some(i),
                    Err(_) => {}
                }
            }
            start += shreds_in(bytes, &shreds, start)?;
        }
        Err(unsigned.map_or(Error::TxNotFound, Error::TxSignature))
    }

    /// Builds the smallest inclusion for `signature` from a whole block's data shreds.
    pub fn from_block(
        data_shreds: &[Vec<u8>],
        parent_slot: u64,
        parent_block_id: Hash,
        signature: [u8; 64],
    ) -> Result<Self, Error> {
        let shreds = data_shreds
            .iter()
            .map(|b| Shred::parse(b))
            .collect::<Result<Vec<_>, _>>()?;
        let slot = shreds.first().ok_or(Error::TxNotFound)?.slot();
        let fec_roots = shred::fec_roots(slot, &shreds)?;
        // Walk components, tracking which shreds each one spans.
        let mut start = 0usize;
        for (i, s) in shreds.iter().enumerate() {
            if !s.data_complete() {
                continue;
            }
            let run = &shreds[start..=i];
            let bytes = block::deshred(run)?;
            if let Ok(Some(_)) = block::find_in_component(&bytes[0], &signature) {
                return Ok(TxInclusion {
                    fec_roots,
                    parent_slot,
                    parent_block_id,
                    boundary_shred: start.checked_sub(1).map(|b| data_shreds[b].clone()),
                    shreds: data_shreds[start..=i].to_vec(),
                    signature: signature.to_vec(),
                });
            }
            start = i + 1;
        }
        Err(Error::TxNotFound)
    }
}

/// How many shreds of the run (from shred index `start`) the component `bytes` used.
fn shreds_in(bytes: &[u8], shreds: &[Shred], start: u32) -> Result<u32, Error> {
    let mut len = 0usize;
    for s in shreds.iter().skip_while(|s| s.index() < start) {
        len += block::shred_data(s)?.len();
        if s.data_complete() {
            debug_assert_eq!(len, bytes.len());
            return Ok(s.index() - start + 1);
        }
    }
    Err(Error::Block(block::Error::Unterminated))
}

/// The most blocks the proof may walk back from the certified block to the proven one.
/// Certificates trail the block they certify by a few slots; 64 leaves a wide margin.
pub const MAX_ANCESTRY: usize = 64;

/// One step back along parent links: the FEC set roots of a block and the parent its
/// block_id names. Rebuilding the block_id from these proves the parent, with no shred.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AncestryLink {
    pub fec_roots: Vec<Hash>,
    pub parent_slot: u64,
    pub parent_block_id: Hash,
}

/// Walks from the certified block `(slot, block_id)` back through `links`, each rebuilt
/// against the id the previous step named, and returns the block the walk ends at.
///
/// A block_id commits to its parent's slot and block_id (the Merkle tree's last leaf), so a
/// certified block certifies every ancestor along this chain: finality covers all that came
/// before. No link says which slot it describes; the slot comes from the child's parent leaf,
/// and a later check against the shreds' own slot ties the two together.
pub fn verify_ancestry(
    slot: u64,
    block_id: &Hash,
    links: &[AncestryLink],
) -> Result<(u64, Hash), Error> {
    let path = ancestry_path(slot, block_id, links)?;
    Ok(*path
        .last()
        .expect("the path holds at least the certified block"))
}

/// Like [`verify_ancestry`], but returns every block on the way: `path[0]` is the certified
/// block, `path[i]` is the block `links[i - 1]` names as parent, and the last is the one the
/// walk ends at. `links[i]` describes the block `path[i]`.
pub fn ancestry_path(
    slot: u64,
    block_id: &Hash,
    links: &[AncestryLink],
) -> Result<Vec<(u64, Hash)>, Error> {
    if links.len() > MAX_ANCESTRY {
        return Err(Error::AncestryTooLong(links.len()));
    }
    let mut path = vec![(slot, *block_id)];
    let (mut slot, mut id) = (slot, *block_id);
    for link in links {
        let computed = shred::block_id(&link.fec_roots, link.parent_slot, &link.parent_block_id)?;
        if computed != id {
            return Err(Error::BlockIdMismatch {
                computed,
                certified: id,
            });
        }
        if link.parent_slot >= slot {
            return Err(Error::AncestryOrder {
                slot,
                parent_slot: link.parent_slot,
            });
        }
        (slot, id) = (link.parent_slot, link.parent_block_id);
        path.push((slot, id));
    }
    Ok(path)
}

/// Finality plus one transaction: the statement a destination chain consumes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxInput {
    /// The certified block D. The transaction may be in D itself (`ancestry` empty) or in an
    /// ancestor of it.
    pub finality: FinalityInput,
    /// Links from D back to the block holding the transaction, nearest first.
    pub ancestry: Vec<AncestryLink>,
    pub inclusion: TxInclusion,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxOutput {
    /// What the certificate covers: the certified block D.
    pub finality: Finality,
    /// The block that holds the transaction (D itself when no ancestry was walked).
    pub tx_slot: u64,
    pub tx_block_id: Hash,
    pub tx: IncludedTx,
}

impl TxInput {
    pub fn verify(&self) -> Result<TxOutput, Error> {
        let finality = self.finality.verify()?;
        self.verify_block(finality)
    }

    /// Everything after the certificate: walk the ancestry to the transaction's block, then
    /// find the transaction in it. `finality` must be this input's own `finality.verify()`
    /// (the zkVM program calls the two halves separately to count their cycles).
    pub fn verify_block(&self, finality: Finality) -> Result<TxOutput, Error> {
        let (tx_slot, tx_block_id) =
            verify_ancestry(finality.slot, &finality.block_id, &self.ancestry)?;
        let tx = self
            .inclusion
            .verify(tx_slot, &tx_block_id, finality.shred_version)?;
        // Parse once more for structural validity of the committed bytes.
        let mut r = block::Reader::new(&tx.raw);
        tx::parse(&mut r).map_err(block::Error::from)?;
        Ok(TxOutput {
            finality,
            tx_slot,
            tx_block_id,
            tx,
        })
    }
}

impl TxOutput {
    /// Public values, version 2: a fixed big-endian byte layout any chain can slice. A
    /// header says which block is final and which block holds the claims, then a list of
    /// tagged claims. This output has one claim, a transaction (tag 1); [`ClaimOutput::encode`]
    /// adds a change set (tag 2).
    ///
    /// ```text
    /// 0     u8   layout version (= 2)
    /// 1     [32] validator_set_hash
    /// 33    u16  shred_version
    /// 35    u64  certified slot (D, the block the certificate names)
    /// 43    [32] certified block_id
    /// 75    u8   path (0 fast, 1 slow)
    /// 76    u64  signed_stake
    /// 84    u64  total_stake
    /// 92    u64  slot_B (the block holding the claims; = D when no ancestry was walked)
    /// 100   [32] block_id of B
    /// 132   u16  descendant footers opened (0 without a change-set claim)
    /// 134   u16  later leader windows covered (0 without a change-set claim)
    /// 136   u64  slot of the highest opened footer (= slot_B when none)
    /// 144   u8   claim count
    /// 145   u8   claim tag (= 1, transaction)
    /// 146   u32  component_start_shred
    /// 150   u32  entry_index
    /// 154   u32  tx_index
    /// 158   u16  tx length
    /// 160   ..   transaction bytes (signatures + message, exactly as in the block)
    /// ```
    pub fn encode(&self) -> Vec<u8> {
        // One encoder for every proof: a transaction-only output is a claims output with one claim.
        ClaimOutput {
            finality: self.finality.clone(),
            slot: self.tx_slot,
            block_id: self.tx_block_id,
            tx: Some(self.tx.clone()),
            state: None,
        }
        .encode()
    }
}

/// What one proof can claim about block B: any of a transaction in it and its change set.
/// The zkVM program runs this; [`TxInput`] is the transaction-only special case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimInput {
    /// The certified block D.
    pub finality: FinalityInput,
    /// Links from D down to B, nearest D first.
    pub ancestry: Vec<AncestryLink>,
    /// "This transaction, signed by these keys, is in B."
    pub tx: Option<TxInclusion>,
    /// "These accounts changed in B, from these values to these values."
    pub state: Option<StateWitness>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimOutput {
    pub finality: Finality,
    /// The block the claims are about (D itself when no ancestry was walked).
    pub slot: u64,
    pub block_id: Hash,
    pub tx: Option<IncludedTx>,
    pub state: Option<StateClaim>,
}

impl ClaimInput {
    pub fn verify(&self) -> Result<ClaimOutput, Error> {
        let finality = self.finality.verify()?;
        self.verify_block(finality)
    }

    /// Everything after the certificate (the program calls the two halves separately to count
    /// their cycles).
    pub fn verify_block(&self, finality: Finality) -> Result<ClaimOutput, Error> {
        if self.tx.is_none() && self.state.is_none() {
            return Err(Error::NoClaims);
        }
        let path = ancestry_path(finality.slot, &finality.block_id, &self.ancestry)?;
        let (slot, block_id) = *path.last().expect("path");
        let tx = match &self.tx {
            Some(t) => {
                let tx = t.verify(slot, &block_id, finality.shred_version)?;
                let mut r = block::Reader::new(&tx.raw);
                tx::parse(&mut r).map_err(block::Error::from)?;
                Some(tx)
            }
            None => None,
        };
        let state = match &self.state {
            Some(w) => Some(w.verify(
                slot,
                &block_id,
                finality.shred_version,
                &self.ancestry,
                &path,
            )?),
            None => None,
        };
        Ok(ClaimOutput {
            finality,
            slot,
            block_id,
            tx,
            state,
        })
    }
}

impl ClaimOutput {
    /// Public values, version 2 (layout on [`TxOutput::encode`]): the header, then each claim
    /// with its tag. Claim 1 is a transaction, claim 2 is a change set:
    ///
    /// ```text
    /// tag 2  [32] bank_hash of B | [32] diff_root | u32 leaf_count
    /// ```
    pub fn encode(&self) -> Vec<u8> {
        let f = &self.finality;
        let (n, windows, top) = self.state.as_ref().map_or((0, 0, self.slot), |s| {
            (s.confirmations, s.windows, s.top_slot)
        });
        let mut out = Vec::new();
        out.push(2);
        out.extend_from_slice(&f.validator_set_hash);
        out.extend_from_slice(&f.shred_version.to_be_bytes());
        out.extend_from_slice(&f.slot.to_be_bytes());
        out.extend_from_slice(&f.block_id);
        out.push(match f.path {
            Path::Fast => 0,
            Path::Slow => 1,
        });
        out.extend_from_slice(&f.signed_stake.to_be_bytes());
        out.extend_from_slice(&f.total_stake.to_be_bytes());
        out.extend_from_slice(&self.slot.to_be_bytes());
        out.extend_from_slice(&self.block_id);
        out.extend_from_slice(&n.to_be_bytes());
        out.extend_from_slice(&windows.to_be_bytes());
        out.extend_from_slice(&top.to_be_bytes());
        out.push(u8::from(self.tx.is_some()) + u8::from(self.state.is_some()));
        if let Some(tx) = &self.tx {
            out.push(1);
            out.extend_from_slice(&tx.component_start_shred.to_be_bytes());
            out.extend_from_slice(&tx.entry_index.to_be_bytes());
            out.extend_from_slice(&tx.tx_index.to_be_bytes());
            out.extend_from_slice(&(tx.raw.len() as u16).to_be_bytes());
            out.extend_from_slice(&tx.raw);
        }
        if let Some(s) = &self.state {
            out.push(2);
            out.extend_from_slice(&s.bank_hash);
            out.extend_from_slice(&s.diff_root);
            out.extend_from_slice(&s.leaf_count.to_be_bytes());
        }
        out
    }
}

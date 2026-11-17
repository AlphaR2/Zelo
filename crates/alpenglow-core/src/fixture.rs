//! Builds proof inputs from `ledger-dump` block fixtures, or synthetic ones with many
//! validators (the local chain has only one). Feature `fixture`; host side only, never in
//! the zkVM.

use {
    crate::{
        finality::{hash_to_g2, CertType},
        proof::{AncestryLink, Aggregate, BlockInput, FinalityInput, TxInclusion, TxInput, Validator},
        shred::Hash,
        state::{Account, AccountChange, BankParts, Confirmation, FooterRun, StateWitness},
    },
    bls12_381::{G1Affine, G1Projective, G2Affine, Scalar},
    serde_json::Value,
};

fn bytes(v: &Value) -> Vec<u8> {
    hex::decode(v.as_str().expect("hex string")).expect("hex")
}

fn hash(v: &Value) -> Hash {
    bytes(v).try_into().expect("32 bytes")
}

fn aggregate(v: &Value) -> Aggregate {
    Aggregate {
        signature: bytes(&v["signature"]),
        bitmap: bytes(&v["bitmap"]),
    }
}

/// From `fixtures/local/block_<slot>.json`.
pub fn block_input(f: &Value) -> BlockInput {
    let cert = &f["block_final_cert"];
    BlockInput {
        finality: FinalityInput {
            validators: f["validator_set"]["entries"]
                .as_array()
                .expect("entries")
                .iter()
                .map(|e| Validator {
                    bls_pubkey: bytes(&e["bls_pubkey"]),
                    stake: e["stake"].as_u64().expect("stake"),
                })
                .collect(),
            shred_version: f["shred_version"].as_u64().expect("shred_version") as u16,
            slot: cert["slot"].as_u64().expect("slot"),
            block_id: hash(&cert["block_id"]),
            final_aggregate: aggregate(&cert["final_aggregate"]),
            notar_aggregate: (!cert["notar_aggregate"].is_null())
                .then(|| aggregate(&cert["notar_aggregate"])),
        },
        parent_slot: f["parent_slot"].as_u64().expect("parent_slot"),
        // Newer fixtures name the parent's block id directly; older ones carry it inside
        // the parent's certificate.
        parent_block_id: if f["parent_block_id"].is_null() {
            hash(&f["parent_block_final_cert"]["block_id"])
        } else {
            hash(&f["parent_block_id"])
        },
        data_shreds: f["data_shreds"]
            .as_array()
            .expect("data_shreds")
            .iter()
            .map(bytes)
            .collect(),
    }
}

/// The parent links in a fixture (`ancestry`), nearest the certified block first. Empty when
/// the fixture's certificate is the block's own, and for fixtures that predate the field.
pub fn ancestry(f: &Value) -> Vec<AncestryLink> {
    f["ancestry"]
        .as_array()
        .map(|links| {
            links
                .iter()
                .map(|l| AncestryLink {
                    fec_roots: l["fec_roots"]
                        .as_array()
                        .expect("fec_roots")
                        .iter()
                        .map(hash)
                        .collect(),
                    parent_slot: l["parent_slot"].as_u64().expect("parent_slot"),
                    parent_block_id: hash(&l["parent_block_id"]),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Finality for the fixture's block plus the smallest inclusion of transaction `signature`.
pub fn tx_input(f: &Value, signature: [u8; 64]) -> TxInput {
    let b = block_input(f);
    let inclusion =
        TxInclusion::from_block(&b.data_shreds, b.parent_slot, b.parent_block_id, signature)
            .expect("tx in block");
    TxInput {
        finality: b.finality,
        ancestry: ancestry(f),
        inclusion,
    }
}

fn base2(ranks: &[usize], bits: usize) -> Vec<u8> {
    let mut out = vec![0u8, bits as u8, (bits >> 8) as u8];
    out.resize(3 + bits.div_ceil(8), 0);
    for &r in ranks {
        out[3 + r / 8] |= 1 << (r % 8);
    }
    out
}

/// `n` deterministic test validators, ranked (stake descending). Secret keys are `i + 2`:
/// test material, never real keys.
pub fn synthetic_set(n: usize) -> (Vec<Scalar>, Vec<Validator>) {
    assert!((1..=u16::MAX as usize).contains(&n));
    let sks: Vec<Scalar> = (0..n as u64).map(|i| Scalar::from(i + 2)).collect();
    let validators = sks
        .iter()
        .enumerate()
        .map(|(i, sk)| Validator {
            bls_pubkey: G1Affine::from(G1Projective::generator() * sk)
                .to_compressed()
                .to_vec(),
            stake: 1_000_000 + ((n - i) as u64) * 1_000,
        })
        .collect();
    (sks, validators)
}

/// A real aggregate over `signers`: H(m) * sum(sk), which equals the sum of their signatures.
pub fn sign(sks: &[Scalar], signers: &[usize], cert: CertType, shred_version: u16) -> Aggregate {
    let sk_sum: Scalar = signers.iter().map(|&i| sks[i]).sum();
    let sig = G2Affine::from(hash_to_g2(&cert.payload(shred_version)) * sk_sum);
    Aggregate {
        signature: sig.to_compressed().to_vec(),
        bitmap: base2(signers, sks.len()),
    }
}

/// ~90% of `n` validators: comfortably over every threshold.
pub fn most(n: usize) -> Vec<usize> {
    (0..n).filter(|i| (i * 7919) % 10 != 3 || n == 1).collect()
}

/// The same block, finalized instead by `n` synthetic validators on the fast path.
pub fn synthetic(base: &FinalityInput, n: usize) -> FinalityInput {
    let (sks, validators) = synthetic_set(n);
    let cert = CertType::FinalizeFast {
        slot: base.slot,
        block_id: base.block_id,
    };
    FinalityInput {
        final_aggregate: sign(&sks, &most(n), cert, base.shred_version),
        notar_aggregate: None,
        validators,
        ..base.clone()
    }
}

/// The same block, finalized by `n` synthetic validators on the slow path.
pub fn synthetic_slow(base: &FinalityInput, n: usize) -> FinalityInput {
    let (sks, validators) = synthetic_set(n);
    let signers = most(n);
    let notar = CertType::Notarize {
        slot: base.slot,
        block_id: base.block_id,
    };
    FinalityInput {
        final_aggregate: sign(
            &sks,
            &signers,
            CertType::Finalize { slot: base.slot },
            base.shred_version,
        ),
        notar_aggregate: Some(sign(&sks, &signers, notar, base.shred_version)),
        validators,
        ..base.clone()
    }
}


// ---- state proofs ---------------------------------------------------------------------------

fn lt(v: &Value) -> Vec<u16> {
    bytes(v).chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}

fn parts(v: &Value) -> BankParts {
    BankParts {
        signature_count: v["signature_count"].as_u64().expect("signature_count"),
        last_blockhash: hash(&v["last_blockhash"]),
        lt_hash: lt(&v["lt_hash"]),
    }
}

fn footer(v: &Value) -> FooterRun {
    FooterRun {
        boundary_shred: bytes(&v["boundary_shred"]),
        shreds: v["shreds"].as_array().expect("shreds").iter().map(bytes).collect(),
    }
}

fn account(v: &Value) -> Option<Account> {
    (!v.is_null()).then(|| Account {
        lamports: v["lamports"].as_u64().expect("lamports"),
        data: bytes(&v["data"]),
        executable: v["executable"].as_bool().expect("executable"),
        owner: hash(&v["owner"]),
    })
}

/// A real block's change set, from `examples/make_state_fixture` (a local ledger replayed with
/// `agave-ledger-tool`), and the parent links and footers of its descendants.
pub struct StateFixture {
    pub slot: u64,
    pub block_id: Hash,
    pub shred_version: u16,
    pub witness: StateWitness,
    /// Links from the certified block down to B (nearest the certified block first). Empty when
    /// B is itself the certified block.
    pub links: Vec<AncestryLink>,
    /// The certified block: B, or the `depth`-th descendant.
    pub certified: (u64, Hash),
}

/// `depth` descendants form the ancestry (0 = B is the certified block); the witness opens
/// the footers of the first `confirm` of them (at most `depth`).
pub fn state_fixture(f: &Value, depth: usize, confirm: usize) -> StateFixture {
    let desc = f["descendants"].as_array().expect("descendants");
    assert!(depth <= desc.len() && confirm <= depth);
    let link = |d: &Value| AncestryLink {
        fec_roots: d["fec_roots"].as_array().expect("fec_roots").iter().map(hash).collect(),
        parent_slot: d["parent_slot"].as_u64().expect("parent_slot"),
        parent_block_id: hash(&d["parent_block_id"]),
    };
    let links: Vec<AncestryLink> = desc[..depth].iter().rev().map(link).collect();
    let certified = match depth {
        0 => (f["slot"].as_u64().unwrap(), hash(&f["block_id"])),
        d => (desc[d - 1]["slot"].as_u64().unwrap(), hash(&desc[d - 1]["block_id"])),
    };
    let witness = StateWitness {
        fec_roots: f["fec_roots"].as_array().expect("fec_roots").iter().map(hash).collect(),
        parent_slot: f["parent_slot"].as_u64().expect("parent_slot"),
        parent_block_id: hash(&f["parent_block_id"]),
        footer: footer(&f["footer"]),
        inner_parent: hash(&f["inner_parent"]),
        lt_parent: lt(&f["lt_parent"]),
        block: parts(&f["block"]),
        changes: f["changes"]
            .as_array()
            .expect("changes")
            .iter()
            .map(|c| AccountChange {
                pubkey: hash(&c["pubkey"]),
                pre: account(&c["pre"]),
                post: account(&c["post"]),
            })
            .collect(),
        confirmations: desc[..confirm]
            .iter()
            .map(|d| Confirmation { footer: footer(&d["footer"]), parts: parts(&d["parts"]) })
            .collect(),
    };
    let shred_version = crate::shred::Shred::parse(&witness.footer.boundary_shred)
        .expect("boundary shred")
        .version();
    StateFixture {
        slot: f["slot"].as_u64().unwrap(),
        block_id: hash(&f["block_id"]),
        shred_version,
        witness,
        links,
        certified,
    }
}

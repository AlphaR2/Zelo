//! Block fixtures from any **shred source**.
//!
//! Shreds are all Huginn needs: block S's shreds hold its transactions, and its child's
//! footer holds the certificate that finalized it (item 6.5). So a data source only has to
//! answer "the shreds of slot S" ([`ShredSource`]); building and checking the fixture is
//! the same for every source ([`build_fixture`]).
//!
//! Sources here:
//! - [`Ledger`]: an Agave ledger's RocksDB, read-only (your local validator, or any node's
//!   ledger on devnet or mainnet);
//! - [`ShredFiles`]: `shreds_<slot>.json` files written by [`export`] (offline, tests).
//!
//! A ShredStream receiver or a repair-protocol client would be one more `ShredSource`.

use {
    alpenglow_core::{
        block::{self, Component, Marker},
        proof::MAX_ANCESTRY,
    },
    alpenglow_lc::{
        cert::{verify_footer, FooterCert},
        shred::{self, Kind, Shred},
        validator_set::ValidatorSet,
    },
    anyhow::{bail, Context, Result},
    rocksdb::{IteratorMode, Options, DB},
    serde_json::{json, Value},
    std::{
        fs,
        path::{Path, PathBuf},
    },
};

/// Where shreds come from.
pub trait ShredSource {
    /// For messages: "ledger .local-ledger", "files fixtures/local/shreds".
    fn describe(&self) -> String;
    /// First and last slot with data shreds.
    fn range(&self) -> Result<(u64, u64)>;
    /// Raw data shreds of `slot`, in index order. Empty if the source has none.
    fn data_shreds(&self, slot: u64) -> Result<Vec<Vec<u8>>>;
    /// Raw coding shreds of `slot` (only used for an extra consistency check; may be empty).
    fn code_shreds(&self, slot: u64) -> Result<Vec<Vec<u8>>>;
}

// ---- sources -------------------------------------------------------------------------

/// An Agave ledger, opened read-only (safe while the validator runs). The `data_shred` and
/// `code_shred` columns are keyed by `slot BE u64 | index BE u64` and hold the raw shred
/// bytes (Agave v4.4.0 `ledger/src/blockstore/column.rs`).
pub struct Ledger {
    path: PathBuf,
    db: DB,
}

impl Ledger {
    pub fn open(ledger: &Path) -> Result<Ledger> {
        let path = ledger.join("rocksdb");
        // Read-only may open a subset of column families; we only need the two shred columns.
        let db = DB::open_cf_for_read_only(
            &Options::default(),
            &path,
            ["data_shred", "code_shred"],
            false,
        )
        .with_context(|| format!("open {}", path.display()))?;
        Ok(Ledger {
            path: ledger.to_path_buf(),
            db,
        })
    }

    fn read(&self, column: &str, slot: u64) -> Result<Vec<Vec<u8>>> {
        let cf = self.db.cf_handle(column).context(column.to_string())?;
        let start = [slot.to_be_bytes(), 0u64.to_be_bytes()].concat();
        let mut out = Vec::new();
        for kv in self
            .db
            .iterator_cf(cf, IteratorMode::From(&start, rocksdb::Direction::Forward))
        {
            let (k, v) = kv?;
            if k[..8] != slot.to_be_bytes() {
                break;
            }
            out.push(v.to_vec());
        }
        Ok(out)
    }
}

impl ShredSource for Ledger {
    fn describe(&self) -> String {
        format!("ledger {}", self.path.display())
    }

    fn range(&self) -> Result<(u64, u64)> {
        let cf = self.db.cf_handle("data_shred").context("data_shred")?;
        let slot_of = |k: &[u8]| u64::from_be_bytes(k[..8].try_into().unwrap());
        let first = self
            .db
            .iterator_cf(cf, IteratorMode::Start)
            .next()
            .transpose()?
            .context("empty ledger")?;
        let last = self
            .db
            .iterator_cf(cf, IteratorMode::End)
            .next()
            .transpose()?
            .context("empty ledger")?;
        Ok((slot_of(&first.0), slot_of(&last.0)))
    }

    fn data_shreds(&self, slot: u64) -> Result<Vec<Vec<u8>>> {
        self.read("data_shred", slot)
    }

    fn code_shreds(&self, slot: u64) -> Result<Vec<Vec<u8>>> {
        self.read("code_shred", slot)
    }
}

/// A directory of `shreds_<slot>.json` files: `{ "slot", "data_shreds": [hex], "code_shreds": [hex] }`.
pub struct ShredFiles {
    dir: PathBuf,
}

impl ShredFiles {
    pub fn new(dir: &Path) -> ShredFiles {
        ShredFiles {
            dir: dir.to_path_buf(),
        }
    }

    fn file(&self, slot: u64) -> PathBuf {
        self.dir.join(format!("shreds_{slot}.json"))
    }

    fn read(&self, slot: u64, key: &str) -> Result<Vec<Vec<u8>>> {
        let path = self.file(slot);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let v = read_json(&path)?;
        v[key]
            .as_array()
            .with_context(|| format!("{}: {key}", path.display()))?
            .iter()
            .map(|h| Ok(hex::decode(h.as_str().context("hex")?)?))
            .collect()
    }
}

impl ShredSource for ShredFiles {
    fn describe(&self) -> String {
        format!("files {}", self.dir.display())
    }

    fn range(&self) -> Result<(u64, u64)> {
        let mut slots = Vec::new();
        for e in fs::read_dir(&self.dir).with_context(|| format!("read {}", self.dir.display()))? {
            let name = e?.file_name().to_string_lossy().to_string();
            if let Some(slot) = name
                .strip_prefix("shreds_")
                .and_then(|n| n.strip_suffix(".json"))
                .and_then(|n| n.parse::<u64>().ok())
            {
                slots.push(slot);
            }
        }
        Ok((
            *slots.iter().min().context("no shreds_<slot>.json files")?,
            *slots.iter().max().unwrap(),
        ))
    }

    fn data_shreds(&self, slot: u64) -> Result<Vec<Vec<u8>>> {
        self.read(slot, "data_shreds")
    }

    fn code_shreds(&self, slot: u64) -> Result<Vec<Vec<u8>>> {
        self.read(slot, "code_shreds")
    }
}

/// Writes `shreds_<slot>.json` for `slot` from any source, for [`ShredFiles`].
pub fn export(src: &dyn ShredSource, slot: u64, dir: &Path) -> Result<PathBuf> {
    let data = src.data_shreds(slot)?;
    if data.is_empty() {
        bail!("no data shreds for slot {slot} in {}", src.describe());
    }
    let code = src.code_shreds(slot)?;
    fs::create_dir_all(dir)?;
    let out = dir.join(format!("shreds_{slot}.json"));
    let v = json!({
        "slot": slot,
        "data_shreds": data.iter().map(hex::encode).collect::<Vec<_>>(),
        "code_shreds": code.iter().map(hex::encode).collect::<Vec<_>>(),
    });
    fs::write(&out, serde_json::to_string(&v)? + "\n")?;
    Ok(out)
}

// ---- certificates ----------------------------------------------------------------------

/// Where the finality certificates come from.
pub enum Certs {
    /// The child block's footer, from the same shred source (default).
    Shreds,
    /// footer-dump plugin output: every non-null `block_final_cert`, as written.
    Plugin(Vec<Value>),
}

impl Certs {
    pub fn plugin(footers: &Path) -> Result<Certs> {
        let text =
            fs::read_to_string(footers).with_context(|| format!("read {}", footers.display()))?;
        // The plugin appends while we read: skip a last line it hasn't finished writing.
        let complete = &text[..text.rfind('\n').map_or(0, |i| i + 1)];
        let mut certs = Vec::new();
        for line in complete.lines() {
            let v: Value = serde_json::from_str(line)?;
            if !v["block_final_cert"].is_null() {
                certs.push(v["block_final_cert"].clone());
            }
        }
        Ok(Certs::Plugin(certs))
    }

    pub fn name(&self) -> &'static str {
        match self {
            Certs::Shreds => "shreds",
            Certs::Plugin(_) => "plugin",
        }
    }

    /// The certificate for `slot`, in the plugin's JSON shape.
    fn for_slot(&self, src: &dyn ShredSource, slot: u64, last: u64) -> Result<Value> {
        match self {
            Certs::Shreds => child_cert(src, slot, last),
            Certs::Plugin(certs) => certs
                .iter()
                .find(|c| c["slot"] == slot)
                .cloned()
                .with_context(|| format!("no footer cert for slot {slot}")),
        }
    }
}

impl Certs {
    /// Certificates for blocks *after* `slot`, within the ancestry limit, nearest block first.
    /// A footer carries the certificate of the newest block final when it was written, so a
    /// block that was skipped over never gets its own: it is final because a descendant is.
    fn later(&self, src: &dyn ShredSource, slot: u64, last: u64) -> Result<Vec<Value>> {
        let limit = slot + MAX_ANCESTRY as u64;
        let mut found: Vec<Value> = Vec::new();
        let mut add = |c: Value| {
            let cs = c["slot"].as_u64().unwrap_or(0);
            if cs > slot && cs <= limit && !found.iter().any(|f| f["slot"] == c["slot"]) {
                found.push(c);
            }
        };
        match self {
            Certs::Shreds => {
                for later in slot + 1..=last.min(limit) {
                    let data = src.data_shreds(later)?;
                    if data.is_empty() {
                        continue;
                    }
                    if let Some(c) = footer_cert(&data)? {
                        add(c);
                    }
                }
            }
            Certs::Plugin(certs) => certs.iter().cloned().for_each(&mut add),
        }
        found.sort_by_key(|c| c["slot"].as_u64());
        Ok(found)
    }
}

/// The parents a block names for itself: its header, and any update-parent marker. The
/// right one is the one that rebuilds the block's certified id.
fn parent_candidates(shreds: &[Shred]) -> Vec<(u64, [u8; 32])> {
    let mut out = Vec::new();
    if let Ok(components) = block::deshred(shreds) {
        for c in &components {
            match block::parse_component(c) {
                Ok(Component::Marker(Marker::Header {
                    parent_slot: ps,
                    parent_block_id: pb,
                }))
                | Ok(Component::Marker(Marker::UpdateParent {
                    parent_slot: ps,
                    parent_block_id: pb,
                })) => out.push((ps, pb)),
                _ => {}
            }
        }
    }
    out
}

/// A certified descendant of `slot` and the parent links back to it.
pub struct Ancestry {
    /// The certificate of the descendant D (the plugin's JSON shape).
    pub cert: Value,
    /// One link per block from D down to `slot`'s child, nearest D first: each block's FEC
    /// roots and the parent its block_id names. The last link's parent is `slot` itself.
    pub links: Vec<Value>,
    /// The block_id of `slot`, as named by its child's parent leaf.
    pub block_id: [u8; 32],
}

/// Finds a certificate for a later block D and walks parent links from D down to `slot`,
/// rebuilding each block's id from its shreds in the source. `None` reasons are reported.
fn find_ancestry(src: &dyn ShredSource, certs: &Certs, slot: u64, last: u64, only: Option<u64>) -> Result<Ancestry> {
    let mut candidates = certs.later(src, slot, last)?;
    if let Some(d) = only {
        candidates.retain(|c| c["slot"].as_u64() == Some(d));
    }
    if candidates.is_empty() {
        bail!("no later block in {} carries a certificate yet", src.describe());
    }
    let mut why = String::new();
    for raw in candidates {
        let cert: FooterCert = serde_json::from_value(raw.clone())?;
        let (mut cur_slot, mut cur_id) = (cert.slot, cert.block_id);
        let mut links: Vec<Value> = Vec::new();
        let outcome = loop {
            let Some(data) = src.data_shreds(cur_slot).ok().filter(|d| !d.is_empty()) else {
                break Err(format!("the source has no shreds for slot {cur_slot}"));
            };
            let shreds = data
                .iter()
                .map(|b| Shred::parse(b))
                .collect::<Result<Vec<_>, _>>()?;
            let roots = shred::fec_roots(cur_slot, &shreds)?;
            let Some((ps, pb)) = parent_candidates(&shreds)
                .into_iter()
                .find(|(ps, pb)| shred::block_id(&roots, *ps, pb).is_ok_and(|id| id == cur_id))
            else {
                break Err(format!("slot {cur_slot}: no parent in its header rebuilds its block id"));
            };
            links.push(json!({
                "fec_roots": roots.iter().map(hex::encode).collect::<Vec<_>>(),
                "parent_slot": ps,
                "parent_block_id": hex::encode(pb),
            }));
            if ps == slot {
                break Ok(pb);
            }
            if ps < slot || links.len() >= MAX_ANCESTRY {
                break Err(format!("the chain from slot {} passes {slot} without meeting it", cert.slot));
            }
            (cur_slot, cur_id) = (ps, pb);
        };
        match outcome {
            Ok(block_id) => {
                return Ok(Ancestry {
                    cert: raw,
                    links,
                    block_id,
                })
            }
            Err(e) => why = format!("certificate for {}: {e}", cert.slot),
        }
    }
    bail!("no certified descendant of {slot} could be linked back to it ({why})")
}

/// The finalization certificate in a block's footer, in the plugin's JSON shape (so both
/// sources feed the same checks and the same fixture format). `None` if the block has no
/// footer yet or the footer carries no certificate.
pub fn footer_cert(data: &[Vec<u8>]) -> Result<Option<Value>> {
    let shreds = data
        .iter()
        .map(|b| Shred::parse(b))
        .collect::<Result<Vec<_>, _>>()?;
    let Ok(components) = block::deshred(&shreds) else {
        return Ok(None);
    };
    for c in &components {
        if let Component::Marker(Marker::Footer(bytes)) = block::parse_component(c)? {
            let Some(cert) = block::parse_footer(bytes)?.final_cert else {
                return Ok(None);
            };
            let agg = |a: &alpenglow_core::proof::Aggregate| json!({ "signature": hex::encode(&a.signature), "bitmap": hex::encode(&a.bitmap) });
            return Ok(Some(json!({
                "slot": cert.slot,
                "block_id": hex::encode(cert.block_id),
                "final_aggregate": agg(&cert.final_aggregate),
                "notar_aggregate": cert.notar_aggregate.as_ref().map(agg),
            })));
        }
    }
    Ok(None)
}

/// The certificate for `slot`, from the footer of the first later block that names it as
/// parent. Usually slot + 1; skipped slots push it further.
fn child_cert(src: &dyn ShredSource, slot: u64, last: u64) -> Result<Value> {
    // A footer carries the certificate of the newest block that was final when it was
    // written: often the parent, but under load a few blocks later. The certificate proves
    // itself (BLS over the block's id), so any later block's footer will do.
    let mut seen = 0;
    for later in slot + 1..=last.min(slot + 64) {
        let data = src.data_shreds(later)?;
        if data.is_empty() {
            continue;
        }
        seen += 1;
        if let Some(c) = footer_cert(&data)?.filter(|c| c["slot"] == slot) {
            return Ok(c);
        }
    }
    if seen == 0 {
        bail!(
            "no block after {slot} in {} yet: it isn't finalized, or wait a slot",
            src.describe()
        )
    }
    bail!(
        "none of the {seen} block(s) after {slot} in {} carries its certificate yet: fetch a few more",
        src.describe()
    )
}

/// The newest slot a certificate can be found for.
pub fn newest_certified(src: &dyn ShredSource, certs: &Certs) -> Result<u64> {
    let (first, last) = src.range()?;
    match certs {
        // The parent of the newest block whose footer is there.
        Certs::Shreds => {
            for s in (first..=last).rev().take(64) {
                if let Some(c) = footer_cert(&src.data_shreds(s)?)? {
                    return c["slot"].as_u64().context("slot");
                }
            }
            bail!(
                "no footer with a certificate in the last 64 slots of {}",
                src.describe()
            )
        }
        Certs::Plugin(c) => c
            .iter()
            .rev()
            .filter_map(|c| c["slot"].as_u64())
            .find(|s| (first..=last).contains(s))
            .context("no certified slot in the source's range"),
    }
}

// ---- the fixture -----------------------------------------------------------------------

/// The validator sets and shred version, written by `fetch` (`cluster_info.json`,
/// `validator_set_<epoch>.json`).
pub struct Sets {
    pub dir: PathBuf,
    pub slots_per_epoch: u64,
}

/// A checked, self-contained block fixture, and what was checked (for printing).
pub struct Built {
    pub slot: u64,
    pub fixture: Value,
    pub report: Vec<String>,
}

/// Reads slot `slot` and its certificates, and returns the fixture only if:
/// - every coding shred agrees with its FEC set's root,
/// - every shred carries the cluster's shred version,
/// - both certificates (slot and parent) verify against the epoch's validator set,
/// - the block_id rebuilt from the shreds equals the certified one.
pub fn build_fixture(
    src: &dyn ShredSource,
    slot: u64,
    sets: &Sets,
    certs: &Certs,
) -> Result<Built> {
    build_fixture_via(src, slot, sets, certs, None)
}

/// [`build_fixture`], but with `via = Some(d)` the block's own certificate is ignored and the
/// certificate of the later block `d` is used, with the parent links back. For exercising
/// long ancestry chains on real data; a normal run uses the block's own certificate when there is one.
pub fn build_fixture_via(
    src: &dyn ShredSource,
    slot: u64,
    sets: &Sets,
    certs: &Certs,
    via: Option<u64>,
) -> Result<Built> {
    let (first, last) = src.range()?;
    let data = src.data_shreds(slot)?;
    let code = src.code_shreds(slot)?;
    if data.is_empty() {
        bail!(
            "no data shreds for slot {slot} ({} holds {first} ..= {last})",
            src.describe()
        );
    }
    let shreds = data
        .iter()
        .map(|b| Shred::parse(b))
        .collect::<Result<Vec<_>, _>>()?;
    for b in &code {
        let s = Shred::parse(b)?;
        if s.kind() != Kind::Code
            || s.merkle_root()? != shreds[s.fec_set_index() as usize].merkle_root()?
        {
            bail!("coding shred {} disagrees with its FEC set root", s.index());
        }
    }
    let roots = shred::fec_roots(slot, &shreds)?;
    let parent_slot = shreds[0].parent_slot().context("parent slot")?;

    // The shred version every vote signs: the cluster's, and the one in the shreds.
    let cluster = read_json(&sets.dir.join("cluster_info.json"))?;
    let shred_version = cluster["shred_version"].as_u64().context("shred_version")? as u16;
    if let Some(s) = shreds.iter().find(|s| s.version() != shred_version) {
        bail!(
            "shred {} has version {}, cluster has {shred_version}",
            s.index(),
            s.version()
        );
    }

    // The certificate: the block's own (a later footer names it), or, when the block was
    // skipped over and never got one, a later block's, with the parent links back to it.
    let direct = match via {
        Some(d) => Err(anyhow::anyhow!("asked to certify {slot} through {d}")),
        None => certs.for_slot(src, slot, last),
    };
    let (raw_cert, ancestry, target_id) = match direct {
        Ok(c) => {
            let id = serde_json::from_value::<FooterCert>(c.clone())?.block_id;
            (c, None, id)
        }
        Err(direct) => match find_ancestry(src, certs, slot, last, via) {
            Ok(a) => {
                let id = a.block_id;
                (a.cert.clone(), Some(a), id)
            }
            Err(e) => bail!("{direct:#}\n  and {e:#}"),
        },
    };
    let cert: FooterCert = serde_json::from_value(raw_cert.clone())?;
    // The set that checks the certificate is the certified block's epoch, which can be a
    // later epoch than the transaction's block.
    let epoch = cert.slot / sets.slots_per_epoch;

    // The validator set of that epoch. RPC only serves the current epoch's stakes,
    // so the set must have been fetched during that epoch.
    let set_path = sets.dir.join(format!("validator_set_{epoch}.json"));
    let set_json = read_json(&set_path).with_context(|| {
        format!(
            "no set for epoch {epoch}: run `fetch validator-set --cluster {}` during it",
            cluster["cluster"].as_str().unwrap_or("<cluster>")
        )
    })?;
    let set: ValidatorSet = serde_json::from_value(set_json["validator_set"].clone())?;
    if set.epoch != epoch {
        bail!(
            "{} holds epoch {}, the certified slot {} is in epoch {epoch}",
            set_path.display(),
            set.epoch,
            cert.slot
        );
    }
    // A parent in an earlier epoch would need that epoch's set too; not handled yet.
    if ancestry.is_none() && parent_slot / sets.slots_per_epoch != epoch {
        bail!("parent {parent_slot} is in a different epoch than {slot}");
    }

    // The parent's own certificate is optional: a footer carries the cert of the newest
    // block that was final when it was written, which on a busy cluster is often not the
    // parent, and finalizing a block finalizes its ancestors, so a parent may never get one.
    // When it is there (a local chain, mostly) it is checked too.
    let raw_parent = match (certs, &ancestry) {
        (_, Some(_)) => None,
        (Certs::Shreds, None) => footer_cert(&data)?.filter(|c| c["slot"] == parent_slot),
        (Certs::Plugin(_), None) => certs.for_slot(src, parent_slot, last).ok(),
    };
    verify_footer(&cert, shred_version, &set)
        .with_context(|| format!("cert for {}", cert.slot))?;
    let parent_cert: Option<FooterCert> = match &raw_parent {
        Some(raw) => {
            let c: FooterCert = serde_json::from_value(raw.clone())?;
            verify_footer(&c, shred_version, &set)
                .with_context(|| format!("cert for {parent_slot}"))?;
            Some(c)
        }
        None => None,
    };

    // The parent's block id is written in the block itself (its header, and any
    // update-parent marker) and bound into the block_id the certificate signs. So any
    // candidate is fine as an input: only the right one rebuilds the certified block_id.
    let mut candidates: Vec<(u64, [u8; 32])> = Vec::new();
    if let Some(c) = &parent_cert {
        candidates.push((parent_slot, c.block_id));
    }
    candidates.extend(parent_candidates(&shreds));
    let (parent_slot_used, parent_block_id) = candidates
        .iter()
        .copied()
        .find(|(ps, pb)| shred::block_id(&roots, *ps, pb).is_ok_and(|id| id == target_id))
        .or(candidates.first().copied())
        .with_context(|| format!("slot {slot}: no parent block id in its header or footer"))?;
    let parent_source = if parent_cert.is_some() {
        "its certificate"
    } else {
        "the block's header"
    };
    let computed = shred::block_id(&roots, parent_slot_used, &parent_block_id)?;
    let report = vec![
        format!("slot {slot} (epoch {epoch}): {} data + {} coding shreds, {} FEC set(s), parent {parent_slot}", data.len(), code.len(), roots.len()),
        format!(
            "  cert for {}{} verifies (shred version {shred_version}, from {}); parent block id from {parent_source}",
            cert.slot,
            if parent_cert.is_some() { format!(" and for {parent_slot}") } else { String::new() },
            match certs {
                Certs::Shreds => "the shreds' footers",
                Certs::Plugin(_) => "the plugin",
            }
        ),
        match &ancestry {
            Some(a) if via.is_some() => format!(
                "  slot {slot} certified through slot {} as asked (--via), {} parent link(s) back",
                cert.slot,
                a.links.len()
            ),
            Some(a) => format!(
                "  slot {slot} has no certificate of its own: certified through slot {}, {} parent link(s) back",
                cert.slot,
                a.links.len()
            ),
            None => format!("  slot {slot} carries its own certificate"),
        },
        format!("  computed block_id  {}", hex::encode(computed)),
        format!("  certified block_id {}", hex::encode(target_id)),
    ];
    if computed != target_id {
        bail!(
            "{}\nblock_id mismatch: not writing a fixture",
            report.join("\n")
        );
    }

    let fixture = json!({
        "cluster": cluster["cluster"],
        "slot": slot,
        // The bank hash this block's own footer claims (None if the footer is not there).
        "footer_bank_hash": footer_bank_hash(&data)?.map(hex::encode),
        "parent_slot": parent_slot_used,
        "epoch": epoch,
        "shred_version": shred_version,
        "validator_set": set_json["validator_set"],
        "cert_source": certs.name(),
        // The certificate of the certified block: this one, or a descendant (see `ancestry`).
        "block_final_cert": raw_cert,
        // Parent links from the certified block down to this one, nearest the certificate
        // first; empty when the certificate is this block's own.
        "ancestry": ancestry.as_ref().map(|a| a.links.clone()).unwrap_or_default(),
        "parent_block_id": hex::encode(parent_block_id),
        "parent_block_final_cert": raw_parent,
        // Coding shreds aren't needed to rebuild the block_id (checked above, not stored).
        "data_shreds": data.iter().map(hex::encode).collect::<Vec<_>>(),
    });
    Ok(Built {
        slot,
        fixture,
        report,
    })
}

pub fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_str(
        &fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
    )?)
}

/// The footer's own `bank_hash` (the bank hash of the block the footer belongs to).
/// `None` if the block has no footer yet.
pub fn footer_bank_hash(data: &[Vec<u8>]) -> Result<Option<[u8; 32]>> {
    let shreds = data
        .iter()
        .map(|b| Shred::parse(b))
        .collect::<Result<Vec<_>, _>>()?;
    let Ok(components) = block::deshred(&shreds) else {
        return Ok(None);
    };
    for c in &components {
        if let Component::Marker(Marker::Footer(bytes)) = block::parse_component(c)? {
            return Ok(Some(block::parse_footer(bytes)?.bank_hash));
        }
    }
    Ok(None)
}

/// Checks a saved validator set against real certificates. **Never changes the set.**
///
/// The set for epoch N is a snapshot, but RPC only shows live data, so a fetched set can be
/// wrong. The only honest test is whether it verifies real certificates: a wrong set cannot.
/// (An earlier version tried to repair the set by dropping validators until certificates
/// passed; that fitting could accept a wrong set and is gone.)
///
/// Each of `slots` is a block whose footer certifies an earlier block; every distinct
/// certificate that belongs to the set's epoch is verified. Returns a JSON report with
/// `"pass"`; the caller writes it and exits non-zero on a fail. Passing needs at least
/// `min_certs` certificates, all verifying.
pub fn check_set(src: &dyn ShredSource, slots: &[u64], sets: &Sets, min_certs: usize) -> Result<Value> {
    let cluster = read_json(&sets.dir.join("cluster_info.json"))?;
    let shred_version = cluster["shred_version"].as_u64().context("shred_version")? as u16;
    let first = *slots.first().context("no slots to check against")?;
    let epoch = first / sets.slots_per_epoch;
    let path = sets.dir.join(format!("validator_set_{epoch}.json"));
    let file = read_json(&path)?;
    let set: ValidatorSet = serde_json::from_value(file["validator_set"].clone())?;

    let mut checks: Vec<Value> = Vec::new();
    let mut seen: Vec<u64> = Vec::new();
    let (mut passed, mut failed, mut skipped) = (0usize, 0usize, 0usize);
    for &slot in slots {
        let Ok(data) = src.data_shreds(slot) else { continue };
        let Ok(Some(raw)) = footer_cert(&data) else { continue };
        let cert: FooterCert = serde_json::from_value(raw)?;
        if seen.contains(&cert.slot) {
            continue;
        }
        seen.push(cert.slot);
        if cert.slot / sets.slots_per_epoch != epoch {
            skipped += 1;
            checks.push(json!({"certified_slot": cert.slot, "found_in_block": slot, "result": "skipped", "why": "certificate is from another epoch"}));
            continue;
        }
        let result = verify_footer(&cert, shred_version, &set);
        let (res, err) = match &result {
            Ok(_) => ("pass", None),
            Err(e) => ("fail", Some(e.to_string())),
        };
        if result.is_ok() { passed += 1 } else { failed += 1 }
        checks.push(json!({"certified_slot": cert.slot, "found_in_block": slot, "result": res, "error": err,
            "has_notar_aggregate": cert.notar_aggregate.is_some()}));
    }
    let pass = failed == 0 && passed >= min_certs && set.epoch == epoch;
    Ok(json!({
        "pass": pass,
        "cluster": cluster["cluster"],
        "epoch": epoch,
        "set_file_epoch": set.epoch,
        "validators": set.entries.len(),
        "total_stake": set.total_stake,
        "commitment": hex::encode(set.commitment()),
        "stake_label": file["stake_label"],
        "shred_version": shred_version,
        "certificates": {"verified": passed, "failed": failed, "skipped_other_epoch": skipped, "required_at_least": min_certs},
        "checks": checks,
        "set_was_modified": false,
        // A pass means: this set verifies these certificates. It does NOT prove the stakes equal
        // Solana's frozen epoch stakes: certificates only test the 60%/80% thresholds, so
        // slightly wrong stakes (e.g. RPC's live numbers) can still pass.
        "stakes_exact": false,
        "meaning": "consistent with the checked certificates; stake values not proven exact",
    }))
}

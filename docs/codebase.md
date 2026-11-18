# Huginn codebase guide

Every folder, in the order it appears in the repo: what it does, which functions to read first, and which tests show it works. The ideas behind the code are in [`how-it-works.md`](how-it-works.md); byte layouts are in [`reference.md`](reference.md).

---

## The whole thing in one picture

```
 validator set (epoch E)      who may vote, with what stake; its hash is stored on the destination chain
        │  BLS signature check
        ▼
 certificate                  ≥80% of stake signed "block_id X at slot S" (or 60% + 60% on the slow path)
        │  ancestry links (when the block has no certificate of its own)
        ▼
 block_id                     a Merkle root committing to every shred of the block, and to its parent
        │  Merkle paths
        ▼
 shreds                       the packets carrying a transaction, or the block's footer
        │                                           │
        ▼                                           ▼
 a transaction (bytes, signatures checked)   the footer's bank hash → lattice hash → change set → diff root
```

Every arrow is code in `crates/alpenglow-core`. The SP1 program in `zk/program` runs that same code inside a zkVM, so the destination chain checks one proof instead of re-running it.

**One rule runs through the repo:** `alpenglow-core` never trusts itself. It is checked against Agave's own code (`alpenglow-lc`) and against real chain data (`fixtures/`).

### Reading order

1. `crates/alpenglow-core/src/finality.rs`: checking one certificate.
2. `shred.rs`: rebuilding a block_id from shreds.
3. `block.rs` and `tx.rs`: turning shreds back into components and transactions.
4. `proof.rs`: how the pieces combine into what a proof states.
5. `bank.rs` and `state.rs`: the state proof.
6. `zk/program/src/main.rs`: the few lines that run it all in SP1.
7. The data tools, in data-flow order: `fetch` → `shred-repair` → `ledger-dump` → `zk/prover`.
8. `programs/`: the Solana side of the example application.

---

## Top level

| File | What it is |
|---|---|
| `Cargo.toml` | The main workspace. It shares dependency versions (each pin says why) and patches in SP1's `bls12_381`, so native tests compile the same curve code as the zkVM. Its header says which folders build on their own, and why. |
| `Cargo.lock` | Exact versions, committed for reproducible builds. |
| `rust-toolchain.toml`, `rustfmt.toml` | The Rust version, and the formatting rules. |
| `LICENSE` | Apache-2.0. |

---

## `crates/alpenglow-core`: the verifier

Everything a proof proves, in plain Rust with no Agave crates and no C, so it compiles for SP1's RISC-V target. Agave's BLS library is C and assembly, which SP1 can't accelerate; SP1 *does* accelerate `bls12_381` 0.8, so the checks are rebuilt on it.

| File | What it does |
|---|---|
| `src/finality.rs` | `verify_cert`: decode the signer bitmap, look up each signer by rank, add their keys, check the BLS signature (`e(pk, H(m)) = e(g1, sig)`, as one multi-pairing), check the stake threshold with exact integers. `CertType::payload` builds the exact vote bytes. |
| `src/shred.rs` | `Shred::parse` reads a shred and computes its FEC root from its own Merkle path. `block_id(fec_roots, parent_slot, parent_block_id)` is Alpenglow's double Merkle root. Agave's tree keeps only 20 bytes of each child; an odd node pairs with itself. |
| `src/block.rs` | `deshred` cuts shreds into components at each DATA_COMPLETE flag. `parse_component` reads entry batches and markers. `find_in_component` finds a transaction while parsing only up to it. `parse_footer` reads the footer and its certificate. |
| `src/tx.rs` | Parses legacy, v0 and v1 transactions with Agave's rules, without Solana crates. `verify_signatures` checks each Ed25519 signature strictly. |
| `src/proof.rs` | The statements: `FinalityInput::verify` (fast or slow path), `verify_ancestry` (from a certified descendant down to the block, at most 64 links), `TxInclusion::verify`, and `ClaimInput`/`ClaimOutput`, which carry a transaction and/or a change set. `ClaimOutput::encode` writes the public values (version 2). `validator_set_hash` is the set commitment. |
| `src/bank.rs` | `LtHash` (the 2,048-byte accounts lattice hash) and `bank_hash`. It includes Agave's published test vector. |
| `src/state.rs` | The state proof. `StateWitness::verify` reads B's footer at its fixed position, opens the bank hash, checks that the change set turns the parent's lattice hash into B's, builds the diff root, and opens descendants' footers for depth. |
| `src/fixture.rs` | *Host only (feature `fixture`).* Builds proof inputs from fixture files, and synthetic validator sets with real signatures. Mainnet-sized sets need hundreds of validators; a local chain has one. |
| `src/accounts_dump.rs` | *Host only.* Parses `agave-ledger-tool accounts` output. Balances are printed as f64, which loses precision on huge balances; it recovers that one account from Agave's checksum. |

**Examples** (`cargo run --release -p alpenglow-core --example <name>`):
- `check_bank_hash`: rebuilds a replayed slot's lattice hash and bank hash, and compares them with Agave's values and with the block's footer.
- `make_state_fixture`: builds `fixtures/local/state_156070.json` from replayed slots, after checking every hash it uses.
- `make_state_pv`: writes the Solidity test fixture for the state registry.
- `verify_block`: the native check of one transaction in a block file.

**Tests:**

| File | Tests | What they show |
|---|---:|---|
| `tests/proof.rs` | 18 | finality on both paths; the attacks of section "What the tests refuse" |
| `tests/adversarial.rs` | 24 | an outside reviewer's attacks; `DEMONSTRATES` tests show trust assumptions |
| `tests/ancestry.rs` | 12 | descendant chains, including real devnet slot 522,347,550, which no footer ever certified |
| `tests/tx_signatures.rs` | 6 | changed or invented signatures, another message, another key, an off-curve key: all refused |
| `tests/block_contents.rs` | 6 | a real block parses; the memo transaction equals RPC's copy byte for byte |
| `tests/state.rs` | 22 | a real replayed block: the real witness passes; every tampered piece fails |
| unit tests | 7 | lattice-hash vectors, Merkle rules |

---

## `crates/alpenglow-lc`: the reference

The same checks done with **Agave's own crates**, the yardstick for `alpenglow-core`. It can't run in SP1 (C code inside), and doesn't need to.

| File | What it does |
|---|---|
| `src/validator_set.rs` | Agave's ranking rules (drop zero-stake and duplicated keys, sort by stake then key), plus `commitment()`. |
| `src/cert.rs` | Calls Agave's `verify_certificate` on genesis and footer certificates. |
| `examples/find_genesis_signers.rs` | Finds which of today's validators signed the devnet genesis certificate. It was the first proof that Huginn's vote encoding is right. |
| `tests/core_matches_agave.rs` | **The most important test file.** `alpenglow-core` and Agave must agree on vote bytes, real certificates and 400 random valid or tampered ones. |
| `tests/tx_matches_agave.rs` | 300,000 random and mutated transactions: Huginn's parser and Agave's agree on every one. |
| `tests/devnet_genesis.rs`, `local_footers.rs`, `local_block.rs` | Real certificates and blocks, with tamper tests. |

---

## Data tools (your machine only)

### `crates/fetch`
A CLI for RPC data: `genesis-cert`, `cluster-info` (the shred version) and `validator-set`. The last reads every vote account's BLS key (`src/vote_account.rs`), ranks them, and writes `validator_set_<epoch>.json` with its commitment. RPC serves only *current* stakes, so it must run during the epoch you need.

### `crates/shred-repair`
Downloads a block's shreds from validators with Agave's **repair protocol**, the way validators fill gaps in their own ledgers: no node, no paid feed, and past blocks work too.
- `src/wire.rs`: the bytes on the wire.
- `src/client.rs`: one UDP socket and a throwaway identity.
- `src/lib.rs`:
  - `fetch_shape`: a block's FEC roots, each proven against its block_id, without downloading shreds;
  - `fetch_block` and `fetch_slot`: the shreds themselves.
- `src/main.rs`: the CLI. `fetch` walks forward until a footer certifies the target or a later block.

Peers are untrusted: everything they send must reproduce the certified block_id.

### `crates/ledger-dump`
Builds a **checked** block file for the prover from any `ShredSource`: a validator's RocksDB ledger (read-only, safe while it runs) or a folder of shred files. `build_fixture` takes certificates from the next block's footer. It checks:
- every certificate, against the epoch's validator set;
- the shred version;
- the rebuilt block_id.

It writes nothing unless all pass. When no footer names the block, `find_ancestry` builds the links from a later certified block. `check-set` tests a validator set against real certificates, and never changes it.

### `crates/redact`
RPC URLs can carry API keys. Every binary prints URLs through `redact::Host`, which shows only the host.

### `plugins/footer-dump`
An optional Geyser plugin (a library the validator loads) that writes each block footer's certificates to `fixtures/local/footers.jsonl`. It has its own workspace and Rust 1.98.1, because it runs inside the validator process and must match its compiler.

---

## `zk/`: the zero-knowledge part

### `zk/program`
`src/main.rs` reads a `ClaimInput`, verifies the certificate and then the claims, and commits `ClaimOutput::encode()` as the public values. Any failed check panics, and then no proof can exist. `Cargo.toml` patches `bls12_381` and `sha2` to SP1's accelerated versions. The committed `Cargo.lock` and stripped symbols keep the program key reproducible.

### `zk/prover` (crate `huginn-prover`, CLI `huginn`)
- `build.rs` builds the program inside SP1's Docker image, pinned by digest, so every machine produces the same bytes and the same program key.
- `src/lib.rs` is what destination apps use:
  - `tx_input_from_block_file`;
  - `Huginn::from_env` (local CPU, mock, or Succinct's network with the `network` feature);
  - `execute`, which counts cycles without proving;
  - `prove`. Every proof is verified, and must match native Rust, before it is returned.
- `src/main.rs` runs the real local transaction, synthetic validator sets (`--synthetic`, `--slow`) or a state claim (`--state`). With `--prove core|compressed|groth16`, it writes `fixtures/proofs/<run>.<mode>.json`.

`zk/check-program-key.sh` builds the program twice, at two different paths, and compares the bytes.

---

## `programs/`: the Solana side of the example application

A workspace of its own, built for SBF with `cargo build-sbf` (Solana's program format, version 3) on [Pinocchio](https://github.com/anza-xyz/pinocchio), a dependency-free program library. Both programs are meant to be deployed immutable.

| Crate | What it is |
|---|---|
| `interface` (`huginn-programs`) | What the programs share with each other and with off-chain code. `outbox`: emitter account layout, `next_chain`, `append`. `fill`: `Order` (parse, encode, `id`, `payload`), `token_account_owner`. `pda`: `check_canonical` (refuse non-canonical bumps) and `create_pda` (works even if someone pre-funded the address). |
| `outbox` | `emit`: appends a message to the sender's emitter account, creating it at the canonical address the first time. |
| `fill` | Pays the merchant (`TransferChecked`), creates the "filled" marker, and emits the order's message from a shard PDA. The outbox id is fixed at build time (`HUGINN_OUTBOX_ID`). |
| `tests` | Mollusk tests: the compiled `.so` files in Agave's runtime with the real SPL Token program. The `shards` tool prints the escrow's shard list; the `live` example fills real orders on a local validator and checks the results. |

**Tests:**
- 11 unit tests in `interface`. They share reference values with the Solidity escrow's tests, including the order id `784041714c…`.
- 10 runtime tests:
  - a fill pays, marks and emits;
  - an order fills once;
  - a marker at another bump is refused;
  - without enough tokens nothing changes;
  - each wrong account or field is refused;
  - pre-funded addresses don't block;
  - two fills on one shard both enter the chain;
  - plus the outbox's own cases.

A fill costs about 20,500 compute units.

---

## `fixtures/`: real data the tests run on

| Path | Contents |
|---|---|
| `mainnet/genesis_cert.json`, `devnet/genesis_cert.json` | The real Alpenglow genesis certificates |
| `devnet/validator_set_1198.json`, `_1202.json`, `cluster_info.json` | Devnet's ranked sets and shred version (7016) |
| `devnet/shreds/`, `devnet/block_522347550.json` | A real devnet block fetched by repair; a block final only through its descendant |
| `devnet-1206/` | Five devnet blocks' shreds, the correct 20-validator set and a wrong 23-validator one (`check-set` tests) |
| `local/validator_set_0.json`, `cluster_info.json`, `footers_sample.jsonl` | A local chain's set, shred version and sample footers |
| `local/block_44000.json`, `block_46718.json`, `tx_46718.json` | Whole local blocks, and RPC's copy of the memo transaction in 46718 |
| `local/shreds/` | Two local blocks' raw shreds: enough to prove a memo with nothing else |
| `local/state_156070.json` | The state witness of a real replayed block (1.1 MB) |
| `tx_v1_agave.json` | A v1 transaction written by Agave's SDK |
| `proofs/` | Where the prover writes proof summaries (not committed) |

---

## What the tests refuse

| Attack | Refused because | Where |
|---|---|---|
| Forged signature, edited bitmap | BLS check / aggregate key changes | `core_matches_agave`, `proof.rs` |
| Too little stake | exact threshold check | `proof.rs` |
| *Finalize(slot)* passed off as a block's finality | the slow path needs *Notarize(block)* too | `proof.rs` |
| Another cluster's votes, another epoch's set, inflated stakes | shred version is signed; the set hash changes | `proof.rs` |
| A changed shred byte or Merkle sibling, a fake FEC root or parent | block_id mismatch | `proof.rs`, `local_block.rs` |
| Reading a component out of frame | the shred before it must be DATA_COMPLETE | `proof.rs` |
| A broken or overlong ancestry chain | each link must rebuild the block_id; slots strictly decrease; at most 64 | `ancestry.rs` |
| An unsigned or wrongly signed transaction | strict Ed25519 check | `tx_signatures.rs` |
| A fake footer, a false or missing account change, repeated or unsorted accounts | footer position; lattice-hash sum; strict ordering | `state.rs` |
| Malformed input crashing the verifier | every read is bounds-checked | `proof.rs`, `adversarial.rs` |
| Filling an order twice, another bump, a wrong account | canonical PDAs, marker, account checks | `programs/tests` |

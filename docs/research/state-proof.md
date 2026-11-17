# Proving Solana account state on another chain (design for adversarial review)

**Status:** design (written 15 Nov 2026 for outside review; version 5 after reviews 1 to 4, section 13b). Partly built: see section 13c for what exists and what is untested.
**Project:** Huginn, which proves facts about Solana to EVM chains using only Solana's own validators and math.
**Code:** the `huginn` (Rust) and `huginn-evm` (Solidity) repositories. File paths below are relative to `huginn/`.

## What we want from you

Please try to break this. We most want to hear about:

1. A way to make the proof program accept a **false statement about an account** (section 6 lists the attacks we already considered).
2. A **wrong fact** about Solana or Agave in section 4. Agave is the main Solana validator client, the software that runs the network. Each fact is marked *verified* or *unverified*.
3. Anything about the **lattice hash** (section 4.5) that makes our use of it weaker than Solana's own use of it. Question 1 in section 13 is the one we are least sure of.
4. Gaps in the **application** (section 8): ways a solver could get paid without delivering, or a user could lose money.

Every technical term is explained in one sentence where it first appears, and there is a glossary at the end.

---

## 0. What already works (context for reviewers and agents)

This proposal builds on a working system. Everything below runs today against live Solana devnet. Links point to the `main` branch on GitHub:

- [AlphaR2/Huginn](https://github.com/AlphaR2/Huginn) (Rust: verification core, fetchers, zkVM program);
- [AlphaR2/huginn-evm](https://github.com/AlphaR2/huginn-evm) (Solidity contracts and the operator app).

### 0.1 What Huginn proves today ("Layer 1": inclusion)

**Statement:** "Solana transaction T is in block B, and block B was finalized by validator set V." This is the base the state proof extends: sections 5.1 and 5.2 reuse its certificate and shred checks unchanged.

| Piece | What it does | Where |
|---|---|---|
| Certificate check | Verifies Alpenglow BLS finalization certificates against a ranked validator set: fast path 80%, slow path 60% + 60%. Tested against Agave's own implementation. | [`alpenglow-core/src/finality.rs`](https://github.com/AlphaR2/Huginn/blob/main/crates/alpenglow-core/src/finality.rs), [`alpenglow-lc`](https://github.com/AlphaR2/Huginn/tree/main/crates/alpenglow-lc) (tests `core_matches_agave.rs`, `devnet_genesis.rs`) |
| Shreds and block_id | Parses Merkle shreds and rebuilds the certified block_id (fact 4.1) | [`alpenglow-core/src/shred.rs`](https://github.com/AlphaR2/Huginn/blob/main/crates/alpenglow-core/src/shred.rs) |
| Block format | Deshreds components, parses entries, transactions, markers and the footer (fact 4.2) | [`alpenglow-core/src/block.rs`](https://github.com/AlphaR2/Huginn/blob/main/crates/alpenglow-core/src/block.rs), [`tx.rs`](https://github.com/AlphaR2/Huginn/blob/main/crates/alpenglow-core/src/tx.rs) |
| Proof statements | `FinalityInput`, `BlockInput`, `TxInclusion` (inclusion from one component plus all FEC roots, with the boundary rule 5.2 reuses), public values v1 | [`alpenglow-core/src/proof.rs`](https://github.com/AlphaR2/Huginn/blob/main/crates/alpenglow-core/src/proof.rs) |
| zkVM program | SP1 program that runs exactly those checks and commits the public values | [`zk/program/src/main.rs`](https://github.com/AlphaR2/Huginn/blob/main/zk/program/src/main.rs), prover in [`zk/prover`](https://github.com/AlphaR2/Huginn/tree/main/zk/prover) |
| Getting shreds without a node | Agave's repair protocol, asking public devnet validators directly; no paid feed | [`crates/shred-repair`](https://github.com/AlphaR2/Huginn/tree/main/crates/shred-repair) |
| Validator sets | Builds the ranked BLS set from RPC; `check-set` verifies it against recent footer certificates | [`crates/fetch`](https://github.com/AlphaR2/Huginn/tree/main/crates/fetch), [`crates/ledger-dump`](https://github.com/AlphaR2/Huginn/tree/main/crates/ledger-dump) |
| Geyser plugin | Dumps block footers from a validator. The witness plugin in section 7 extends it. | [`plugins/footer-dump`](https://github.com/AlphaR2/Huginn/tree/main/plugins/footer-dump) |
| EVM light client | Verifies the SP1 Groth16 proof, pins the program key and per-epoch validator set hashes, records verified transactions | [`src/SolanaLightClient.sol`](https://github.com/AlphaR2/huginn-evm/blob/main/src/SolanaLightClient.sol), [`src/HuginnOutput.sol`](https://github.com/AlphaR2/huginn-evm/blob/main/src/HuginnOutput.sol), [`src/SolanaTx.sol`](https://github.com/AlphaR2/huginn-evm/blob/main/src/SolanaTx.sol) |
| Example app | A contract controlled by a Solana key through Memo transactions | [`src/SolanaControlled.sol`](https://github.com/AlphaR2/huginn-evm/blob/main/src/SolanaControlled.sol), [`src/examples/SolanaGreeter.sol`](https://github.com/AlphaR2/huginn-evm/blob/main/src/examples/SolanaGreeter.sol) |
| Operator app | One command runs every step, from sending a devnet transaction to the on-chain check | [`app/src/steps.rs`](https://github.com/AlphaR2/huginn-evm/blob/main/app/src/steps.rs) |

### 0.2 Milestones reached

1. **7 Nov 2026: the first proof of a live devnet transaction.**
   - Shreds came from public validators, with no node and no paid feed.
   - A fast-path certificate: 17 of 21 validators.
   - 5.9M cycles.
   - Record: [`docs/research/devnet-proof.md`](https://github.com/AlphaR2/Huginn/blob/main/docs/research/devnet-proof.md).
2. **14 Nov 2026: the first full run, Solana devnet → Succinct prover network → Base Sepolia.** A real Groth16 proof was accepted on-chain and used to run a command, and a one-byte forgery was rejected (`ProofInvalid`).

   | | |
   |---|---|
   | Solana transaction | `3C4JrEb4UPAuHCHaFBpsivx2SPNeoGYBusiKnppBqXbiY3SRNYf4RwwY57C2twAskhrVy3Ji2He6d6uP8wu9CYc4` (slot 522,257,403, epoch 1208) |
   | Certificate | fast path, 10 of 21 validators, 97.27% of stake |
   | Proof | 5,563,604 cycles; 53 s on the prover network; 356-byte Groth16 |
   | SolanaLightClient (Base Sepolia) | [`0xcceFF16fA1fa8De5000b4bc18097b131971faFB3`](https://sepolia.basescan.org/address/0xcceFF16fA1fa8De5000b4bc18097b131971faFB3) |
   | SolanaGreeter (Base Sepolia) | [`0xBb303814fd9CB068aa30684DF6c37e4dC2BE99B4`](https://sepolia.basescan.org/address/0xBb303814fd9CB068aa30684DF6c37e4dC2BE99B4) |
   | Proof submitted | [`0x2b3e48cc…74af3`](https://sepolia.basescan.org/tx/0x2b3e48cc7bdc303915e540da49665c05953aa5ae5a1c0b444f0f479ab3074af3), 388,297 gas |
   | Command executed | [`0x2c9aab6f…211d0`](https://sepolia.basescan.org/tx/0x2c9aab6f016c76e888459b5873bcf968f94419ec52e86438c10d6c83de7211d0), 405,391 gas |
   | Program key | `0x006a5709f825d075af9540231ccd6add1464431b74d4deef30d5c3bc76454c19` |

   Caveat from that run: the validator set's stakes came from live RPC values (`stakes_exact: false`). Every certificate checked passes with that set, but the stake numbers themselves are not proven.
3. **An earlier outside adversarial review** of both repositories: [`docs/research/adversarial-report.md`](https://github.com/AlphaR2/Huginn/blob/main/docs/research/adversarial-report.md). The attack tests that came out of it live in [`crates/alpenglow-core/tests/adversarial.rs`](https://github.com/AlphaR2/Huginn/blob/main/crates/alpenglow-core/tests/adversarial.rs) and [`test/Adversarial.t.sol`](https://github.com/AlphaR2/huginn-evm/blob/main/test/Adversarial.t.sol) / [`Adversarial2.t.sol`](https://github.com/AlphaR2/huginn-evm/blob/main/test/Adversarial2.t.sol).
4. **15 Nov 2026: the gap this document starts from.** A second devnet run reached fetch and failed: the transaction's block (slot 522,347,550) never got a certificate of its own (fact 4.3). Step S0 fixes this.

### 0.3 Background reading

- [`docs/architecture.md`](https://github.com/AlphaR2/Huginn/blob/main/docs/architecture.md): how the pieces fit.
- [`docs/codebase.md`](https://github.com/AlphaR2/Huginn/blob/main/docs/codebase.md): a file-by-file guide.
- [`docs/security.md`](https://github.com/AlphaR2/Huginn/blob/main/docs/security.md): the threat model for Layer 1.
- [`docs/research/findings.md`](https://github.com/AlphaR2/Huginn/blob/main/docs/research/findings.md): facts about Alpenglow's block format and certificates, checked against Agave's source and real data.
- [`docs/research/plan.md`](https://github.com/AlphaR2/Huginn/blob/main/docs/research/plan.md): the project plan. Its "Rung 4" is this document.

### 0.4 Not public yet

The following exist locally but are not pushed:
- the council contract (a Solana multisig controlling an EVM contract) and the vault demo;
- the local control-room GUI;
- the raw run records with logs.

None of them is needed to review this proposal.

---

## 1. The problem in one paragraph

Huginn can already prove to an EVM contract that a Solana transaction is **in a finalized block**: Solana's validators signed off on the block, and the transaction's bytes are inside it. That isn't enough for most real uses, because **failed transactions are in blocks too**. A failed transaction still pays its fee, so it gets included. A contract that pays out because "this transfer is in a finalized block" can be fooled by a transfer that failed. What we need is proof of **what the block changed**: "after block B, account X holds exactly these bytes." This document proposes a way to prove that from data Solana's validators already sign today, with no change to Solana.

## 2. Background (plain terms)

- **Slot:** a numbered time window (a few hundred milliseconds) in which one validator, the *leader*, may produce a block.
- **Validator:** a computer that runs Solana, checks blocks and votes on them. Its voting power is its **stake**, the SOL delegated to it.
- **Alpenglow:** Solana's consensus protocol since 21 Oct 2026 (mainnet slot 459,653,000). Validators send signed votes to each other, and votes are combined into **certificates**.
- **Certificate:** one aggregated BLS signature plus a bitmap of which validators signed. BLS is a signature scheme where many signatures combine into one. A **finalization certificate** says the block is final and will never be reverted:
  - **fast path:** 80% of stake notarized the block;
  - **slow path:** 60% notarized and 60% finalized.
- **Shred:** a piece of a block, at most about 1.2 KB, the unit the network sends around. Shreds come in **FEC sets** of 32 data shreds; FEC (forward error correction) adds coding shreds so lost pieces can be rebuilt. Each FEC set has a **Merkle root**, a single hash that commits to every shred in the set, and every shred carries a Merkle proof up to it.
- **block_id:** the fingerprint the votes sign. It is a Merkle root over the FEC set roots plus one extra leaf naming the **parent block** (section 4.1).
- **Component:** the block's content is a sequence of components. Most are *entry batches*, which hold the transactions. A few are *markers*: a header, an optional update-parent, and the **footer**, which comes just before one final tick entry (the "alpentick") that ends the block.
- **Footer:** the second-to-last component of an Alpenglow block (only the alpentick follows it). It carries:
  - the block's **bank hash**;
  - the producer's clock time;
  - its software name;
  - usually a finalization certificate for some *earlier* block.
- **Bank hash:** a 32-byte fingerprint of Solana's entire state after a block (section 4.4).
- **Account:** Solana's unit of state: an address (32-byte public key) holding lamports (1 SOL = 10⁹ lamports), a data byte array, an owner program, and an executable flag. Only the owner program can change an account's data.
- **PDA (program-derived address):** an account address computed from a program ID and seeds, for example an order ID. Only that program can create an account there.
- **Accounts lattice hash (SIMD-0215):** a 2,048-byte value equal to the *sum* of a 2,048-byte hash of every account (section 4.5). A SIMD (Solana Improvement Document) is Solana's formal protocol change proposal.
- **SP1 / zkVM:** SP1 is a zero-knowledge virtual machine. You run an ordinary Rust program inside it and get a short proof that the program ran to the end on some inputs. Here, "ran to the end" means every check passed. An EVM contract verifies the proof for about 300k gas.

## 3. The statement we want to prove

> **STATE(B, X, pre, post):** Block B, at slot `slot_B` with block id `id_B`, is final under Solana validator set V. During block B, account X changed from `pre` to `post`. Here `pre` and `post` are full account values (lamports, data, owner, executable), or "absent", which means zero lamports.

From this, an application gets what it needs. For example: "the outbox account of the fill program's emitter advanced in block B, by a message saying order O was filled" (section 8).

The statement is about **one block's change**. It does not claim the account still holds `post` today. For append-only records like the outbox chain, which section 8 uses, that is all we need.

## 4. Facts the design relies on

Each fact gives its source and how sure we are. "Verified" means checked against real devnet data by our code. "Source-read" means read in Agave's code but not yet checked against live data.

### 4.1 block_id formula (verified)

```
leaves   = [fec_root_0, ..., fec_root_{n-1},
            sha256(parent_slot u64 LE | parent_block_id [32] | n u32 LE)]
block_id = merkle_root(leaves)
```

- Agave's Merkle tree pairs nodes left to right, and a last odd node is paired with itself.
- Leaf hashes and inner nodes use different domain prefixes (`\x00SOLANA_MERKLE_SHREDS_LEAF`, `\x01SOLANA_MERKLE_SHREDS_NODE`), so a leaf can't pose as an inner node.
- The leaf count `n` is bound inside the parent leaf.

Source: `crates/alpenglow-core/src/shred.rs`, reimplemented from Agave v4.4.0 (`ledger/src/shred/merkle*.rs`, `blockstore.rs::build_double_merkle_tree`). Our code rebuilds certified block_ids from devnet shreds on every run.

**Consequence:** a block_id commits to its parent's block_id. Block ids therefore form a hash chain, like Git commits. A block's own slot is not a separate field, but every shred's signed bytes include its slot, and the FEC roots commit to those bytes.

### 4.2 Footer layout and certificate (verified)

```
footer = tag u8 (=1) | bank_hash [32] | producer_time_nanos u64 | user_agent (u8 len + bytes)
       | Option<cert> | Option<skip reward cert> | Option<notar reward cert>
cert   = slot u64 | block_id [32] | final aggregate | Option<notar aggregate>
```

Source: `crates/alpenglow-core/src/block.rs` (`parse_footer`), checked on devnet blocks.

### 4.3 Footers certify only the latest finalized block (verified 15 Nov 2026)

A footer carries the certificate of the **most recently finalized slot** the leader knows, not of every slot. A devnet survey found:

| Block | Its footer certifies |
|---|---|
| 522,347,549 | 547 |
| 550 | 548 |
| 551 | 549 |
| 552 | 551 |
| 553 | 551 |
| 554 | 552 |
| 555 | 554 |

Slots 550 and 553 **never got a certificate of their own**. They are final only because a descendant was finalized: finalizing a block finalizes all its ancestors, since the descendant's block_id chain commits to them (4.1). Huginn's current fetch fails on such slots, which is roughly 25% of this sample. Section 5.1 fixes this, and the state proof needs the fix anyway.

### 4.4 Bank hash formula (source-read, NOT yet verified against live data)

From Agave `runtime/src/bank.rs`, `hash_internal_state` (master, read 15 Nov 2026):

```
inner     = sha256( parent_bank_hash [32] | signature_count u64 LE | last_blockhash [32] )
bank_hash = sha256( inner [32] | accounts_lt_hash [2048] )        // the full 2,048 bytes
if a hard fork is scheduled at this slot:
    bank_hash = sha256( bank_hash | hard_fork_data )
```

- `accounts_lt_hash` is the block's accounts lattice hash after the block: 1,024 u16 values cast to bytes (little-endian).
- `signature_count` counts signatures of the transactions the block committed.
- `last_blockhash` is the block's last PoH entry hash. PoH (Proof of History) is the hash chain that orders entries in a block.
- Agave logs every frozen bank as `bank frozen: <slot> hash: <bank_hash> signature_count: <n> last_blockhash: <h> ... accounts_lt_hash checksum: <blake3 of L>`. That gives us an independent check.

**Status of F-BH:** review 1 confirmed this formula matches the Agave v4.4.1 source exactly. Devnet now runs 4.5.0-beta.0, so S1 must still check it against live data.

### 4.5 The accounts lattice hash (source-read)

From SIMD-0215 and Agave `accounts-db/src/accounts_db.rs` (`lt_hash_account`, `write_account_hash_input`).

**Per-account hash:**
```
h(acct) = 0^2048                                        if lamports == 0
        = BLAKE3-XOF-2048( lamports u64 LE | data | executable u8 | owner [32] | pubkey [32] )
```
- BLAKE3 is a hash function, and XOF ("extendable output") means it is asked for 2,048 output bytes instead of 32.
- The account's own address (`pubkey`) is inside its hash, so the same data at a different address hashes differently.

**Lattice hash of a state:** `L = Σ h(acct)` over all accounts, where Σ adds element-wise over 1,024 u16 values mod 2¹⁶.

**Per-block update:**
```
L_B = L_P + Σ_{accounts changed in B} ( h(end value) − h(start value) )
```
Here P is B's parent bank. Order doesn't matter, and an account written several times in a block telescopes to start → end.

**Security claim (SIMD-0215):**
- The parameters target 128-bit collision resistance.
- A later analysis cited by the SIMD finds an attack costing about 2⁸¹ hash queries, but it needs colliding sets of at least about 2⁷²·⁸³ items.
- The SIMD argues this can't apply to real states, because real states have fewer than 2⁶⁵ funded accounts.
- The SIMD notes the lattice hash **does not support inclusion proofs**: you can't open one account from L alone.

Our design never opens a single account against L. It opens a **whole block's change set** (section 5.4).

**Status of F-LT:** confirmed in the v4.4.1 source (review 1); the live check is in S1.

### 4.6 Failed transactions change almost nothing (Solana semantics, to confirm)

A failed transaction keeps only two effects: the fee debit on the fee payer, and the advance of its durable nonce account (if it uses one). Every other write is discarded. A failed CPI (one program calling another) aborts the whole transaction; a program can't catch it and carry on.

**Consequence:** if a fill program emits its message only in the same transaction as a successful token transfer, a message exists **if and only if** the transfer succeeded.

### 4.7 Where the bank hash's components come from (to confirm in S1)

The footer of B carries `bank_hash(B)` (SIMD-0298). Validators check it during replay, so the certificate over `id_B`, through the footer's shreds, covers it.

**Status of F-FOOT** (the footer of block B carries the bank hash of B itself, not of its parent): confirmed in source by review 1. The leader writes the footer's hash after its bank freezes (`poh_recorder.rs` ~659), and replay compares it. The live check is in S1.

SIMD-0301 (in review) would change the footer to carry the parent's bank hash. Section 11 shows the design adapts by reading the child block's footer instead.

---

## 5. The construction

The proof program (SP1) receives a **witness**: all the data needed to check the statement. It runs checks 5.1 to 5.5 and, if all pass, outputs the public values in 5.6. Accounts are then opened on the destination (5.7). Any failed check aborts the program, and then no proof exists.

### 5.1 B is final: direct or through descendants

**Input:**
- the finality certificate for some block D (slot `slot_D`, id `id_D`) and the validator set V for D's epoch;
- an **ancestry chain**: links `[L_1, ..., L_k]`, `k ≤ 64`, each `L_i = (fec_roots_i, parent_slot_i, parent_block_id_i)`.

**Checks:**
1. The certificate verifies against V. This is the existing `FinalityInput::verify`:
   - BLS aggregate signature;
   - bitmap;
   - stake thresholds (fast 80%, or slow: notarize 60% plus finalize 60%);
   - the shred version is part of the signed vote.
2. Start with `(s, id) = (slot_D, id_D)`. For each link `i = 1..k`:
   - `block_id(fec_roots_i, parent_slot_i, parent_block_id_i) == id`;
   - `parent_slot_i < s`;
   - set `(s, id) = (parent_slot_i, parent_block_id_i)`.
3. When the chain ends, `s == slot_B`, so `id_B = id`. With `k = 0`, B = D.

**Why it's sound:**
- Each step rebuilds a certified block_id from the claimed parent. Finality of D implies finality of all its ancestors under Alpenglow safety.
- Faking a parent needs a collision in the block_id's Merkle tree.
- **Limit:** Agave's Merkle tree hashes only the **first 20 bytes** of each child (`join` in `shred.rs`, matching Agave). A collision therefore costs about **2⁸⁰** work (the birthday bound for 160 bits), not full SHA-256 strength.
- The ancestry chain adds a new place this bites. A leader producing two consecutive blocks could grind a parent-leaf collision between its real parent and a fabricated one, then use the fabricated parent to carry a fake footer.
- 2⁸⁰ is expensive but not out of reach for a well-funded attacker over time. This is a bound on value: keep a **per-proof value cap** on the destination, as the plan already says for Layer 1.

*(Corrected after review: v1 of this document claimed full SHA-256 strength here.)*

**Cost:** each link is a few dozen hashes. A link needs only the block's FEC roots, which shred-repair's `shape` command already downloads and verifies without fetching any shreds.

### 5.1b Descendant confirmations (mitigates the 2⁸⁰ limit; added in review 2)

**The attack 5.1 leaves open.** B's leader builds two footer shreds whose Merkle leaf hashes agree in the 20 bytes the tree keeps. Validators see and certify the honest one; the attacker proves the fake one, with any bank hash. That costs about 2⁸⁰, a birthday collision, which only works when one party chooses *both* colliding inputs.

**The mitigation.** The bank-hash chain uses **full 32-byte hashes**, and every block's bank hash commits to its parent's. So the program also opens the footers of `N` blocks descending from B, `C_1 … C_N`. These come from the ancestry chain, so their block_ids are already certified. For each one:

```
footer(C_i).bank_hash == sha256( sha256( bank_hash(C_{i-1}) | signature_count_{C_i} LE | last_blockhash_{C_i} ) | L_{C_i} )
```

with `C_0 = B`. The inputs `signature_count`, `last_blockhash` and `L` (2 KB) for each `C_i` are free preimage material, as in 5.3.

**Why it works.**
- A fake footer for B must now also agree with an honest footer further up the chain.
- The birthday trick needs control of both colliding inputs. A block produced by an honest leader can only be attacked by a second preimage on 160 bits (about 2¹⁶⁰), which is out of reach.
- So the attacker must have produced **every** block from B up to the highest footer opened.

**Where the descendants come from.**
- `C_1 … C_N` are the first `N` links of the ancestry chain above B, so `N ≤ k`. To open more, certify a later D.
- Each `C_i`'s footer is located by exactly the position rule of 5.2: footer, then alpentick, at the end of the slot.

**Leader windows.**
- A Solana leader produces **4 consecutive slots** (one "leader window", aligned so that slots `4w … 4w+3` share a leader). `N` must reach past B's window to reach a different leader.
- The program can't see the leader schedule, so it can't prove the leaders were different validators.
- Skipped slots mean `N` and the top slot alone don't tell an app how many windows were covered. So the program **counts directly**: it outputs `windows`, the number of distinct values `⌊slot(C_i) / 4⌋` among the opened footers that differ from `⌊slot_B / 4⌋` *(review 3)*.
- Apps set their own requirement, for example `windows ≥ 2`.
- A validator with a large stake may own several windows. Requiring more windows makes that rarer, and the value cap still applies.

**Cost:** one footer, one 2 KB SHA-256 and a few shreds per descendant, so negligible.

### 5.2 The footer of B

**Block shape (Agave v4.4.1, `runtime/src/block_component_processor.rs`):**

```
Header | optional Genesis marker | entries* | optional UpdateParent | entries* | Footer | Alpentick
```

The **Alpentick** is one final tick entry (`num_hashes == 1`); a tick is an entry that carries no transactions. Replay rejects any block that doesn't end footer → alpentick, so there is **exactly one footer, and it is the second-to-last component**. *(Corrected after review: v1 of this document wrongly said the footer is the last component.)*

**Input:** B's FEC roots, B's parent (slot, id), and the shreds holding B's last two components (the footer and the alpentick), plus the shred just before them.

**Checks:**
1. `block_id(B's fec_roots, B's parent_slot, B's parent_block_id) == id_B`.
2. Every given shred:
   - is a data shred of slot `slot_B` with the certified shred version;
   - verifies by its own Merkle proof against the FEC root of its set.
3. The shred before the run is flagged DATA_COMPLETE, so the run starts at a component boundary and can't be read out of frame.
4. The run is consecutive and deshreds into **exactly two components**:
   - the first parses as a **footer marker** (version 1, variant 0);
   - the second is an entry batch of **exactly one tick entry with `num_hashes == 1`** (the alpentick);
   - the run's **last shred is flagged LAST_SHRED_IN_SLOT**.

   This position, the component just before the end of the slot, is the only one replay accepts for the footer. It is what makes a footer-shaped component anywhere else in the block (including the never-replayed prefix before an UpdateParent, section 11) useless to an attacker.
5. Output: `footer.bank_hash`.

This reuses the logic Huginn already uses for transaction inclusion (`TxInclusion::verify`), plus the end-of-slot rule.

### 5.3 Opening the bank hash

**Input:**
- `inner_P` [32], the inner hash of B's parent bank;
- `L_P` [2048];
- `signature_count_B` u64;
- `last_blockhash_B` [32];
- `L_B` [2048].

**Checks:**
```
bank_hash_P = sha256( inner_P | L_P )
inner_B     = sha256( bank_hash_P | signature_count_B LE | last_blockhash_B )
sha256( inner_B | L_B ) == footer.bank_hash          (from 5.2)
```

**Why it's sound:**
- `L_B` is bound to the certified bank hash. Any other `L_B` needs a SHA-256 second preimage. The inputs have fixed lengths, so there is no length ambiguity.
- `L_P` is bound one level deeper, through `bank_hash_P` inside `inner_B`.
- We never need the parent's footer, or the meaning of `inner_P`, `signature_count_B` or `last_blockhash_B`. They are only preimage material, and wrong values make the hash check fail.

**Hard-fork slots:** the extra hashing step is not supported, so the check simply fails.

### 5.4 Opening the block's change set (the "delta opening")

**Input:** the change set `W = [(pubkey_j, pre_j, post_j)]` for `j = 1..m`:
- `pre_j` and `post_j` are each `None` or a full account (lamports, data, executable, owner);
- `m ≤ M_MAX` (proposed: 2¹⁶).

**Checks:**
1. The pubkeys are **strictly increasing**, so there are no duplicates (attack A5).
2. An account with lamports 0 counts as `None`. Its hash is zero either way.
3. Compute `Δ = Σ_j ( h(pubkey_j, post_j) − h(pubkey_j, pre_j) )`, with `h(·, None) = 0`.
4. Require `L_P + Δ == L_B`.

**What this establishes** (argued in 6.1): unless someone has found a lattice-hash collision, every entry with `h(pre_j) ≠ h(post_j)` is the true start and end value of that account in block B.

### 5.5 Committing the whole change set: the diff root (changed in review 2)

Hashing the change set is the expensive part, so one proof should serve **every** account the block changed, not one. *(Version 2 of this document proved one target account per proof; review 2 pointed out that this wastes the expensive part.)*

**Leaves.** Keep only entries with `h(pre_j) ≠ h(post_j)`; entries that changed nothing are dropped. For each kept entry:

```
d(acct)  = sha256( lamports u64 LE | data | executable u8 | owner [32] )     or 0^32 for None
leaf_j   = sha256( 0x00 | pubkey_j [32] | d(pre_j) | d(post_j) )
```

- SHA-256 is used here because both SP1 and the EVM have a cheap precompile for it.
- `d` repeats the lattice hash's field order without the pubkey, since the leaf already carries it.

**Tree.**
- Leaves stay in pubkey order (already strictly increasing, 5.4).
- Binary Merkle tree with **full 32-byte nodes** `sha256(0x01 | left | right)`, so none of the 20-byte truncation of 5.1.
- An odd last node is promoted unchanged.
- `diff_root = sha256( 0x02 | leaf_count u32 LE | merkle_root )`.

**What this gives** (argued in 6.1, which is unchanged):
- **Membership:** a leaf `(X, d(pre), d(post))` under `diff_root` means X changed from pre to post in B.
- **Non-membership:** two adjacent leaves (indices `i`, `i+1`) with `pubkey_i < X < pubkey_{i+1}`, or the first/last leaf for the edges, prove X has **the same value at both block boundaries** of B. That includes an account changed and then restored within B; the proof is about the boundaries, not the writes. Leaving out an account whose boundary values differ would break the lattice sum: a collision.
- **Empty tree:** if no account's boundary values differ, `leaf_count = 0`, `merkle_root = 0^32`, and every account is a non-member.

### 5.6 Public values: one layout for both kinds of claim (layout version 2, big-endian)

*(Changed in review 3: version 3 of this document had a state-only layout, which contradicted the "one program, tagged claims" design in 8b.)*

A shared header says which block is final and how deeply it is confirmed. A list of tagged claims about B follows.

```
header
0    u8    layout version (= 2)
1    [32]  validator_set_hash        (V, as the destination stores it for D's epoch)
33   u16   shred_version
35   u64   certified slot (D)
43   [32]  certified block_id (D)
75   u8    path (0 fast, 1 slow)
76   u64   signed_stake
84   u64   total_stake
92   u64   slot_B                    (= slot D when the ancestry chain is empty)
100  [32]  id_B
132  u16   N, descendant footers opened (5.1b)
134  u16   windows, distinct later leader windows covered (5.1b)
136  u64   slot of the highest opened footer (= slot_B when N = 0)
144  u8    claim count

claims, each starting with a u8 tag
tag 1  transaction  u32 component_start_shred | u32 entry_index | u32 tx_index | u16 tx length | tx bytes
                    (signatures verified in the proof, section 11)
tag 2  state        [32] bank_hash_B | [32] diff_root | u32 leaf_count
```

Descendant confirmations live inside the change-set claim (they need B's bank hash, which only the state witness opens). A transaction-only proof always has `N = 0`. *(Corrected after review 5: an earlier version said B's footer is opened for transaction-only proofs too; the code does not do that.)* A proof may carry both claims, transaction first: the light client reads the transaction and skips the change set, the registry reads the change set and skips the transaction.

### 5.7 On the destination: store once, open many times

A `StateRegistry` contract verifies a proof carrying a state claim once, then serves openings.

**Keyed like the light client.**
- Records are stored as `roots[validator_set_hash][slot_B] = { id_B, bank_hash_B, diff_root, leaf_count, windows, top_slot }`.
- Keying by set hash is deliberate (review 3). `SolanaLightClient` does the same, so that a period with a bad validator set can't poison proofs made under a good one.
- The registry checks `validator_set_hash` against the set it trusts for **D's** epoch.

**Rules.**
- **A different root for an existing key** (another `id_B` or `diff_root` for the same set hash and slot): **refused with a revert** (`ConflictingRecord` / `ConflictingChangeSet`). Finality says this can't happen honestly, so it is evidence of a broken set or a broken assumption. *(Built as a revert, not an event: events in a reverting transaction are lost. Watchers see the failed transaction's error.)* The registry also refuses a block id that disagrees with the one the light client already holds for the slot under the same set.
- **The same root with greater depth** (higher `windows` or `top_slot`): the depth is **raised, never lowered**.

**Opening.**
- `open(setHash, slot_B, pubkey, preFields, postFields, merkleProof)` recomputes `d(pre)`, `d(post)` and the leaf with the EVM's SHA-256 precompile, checks the path, and returns the fields.
- The caller (the app) checks that `setHash` is one it trusts, and that the recorded depth meets its policy.

---

## 6. Soundness

**Claim.** If the program accepts a diff root for B, and a leaf `(X, d(pre), d(post))` opens under it (5.7), then block B is final and X changed from `pre` to `post` in B, unless one of the following holds:

- (a) more than the Alpenglow safety bound of stake was malicious, or V is not the real validator set;
- (b) someone found a collision in the shred / block_id Merkle trees. These use 20-byte truncated nodes, so the cost is about 2⁸⁰ (see 5.1). Or someone found a full SHA-256 second preimage in the bank-hash chain (5.3), which uses untruncated 32-byte hashes;
- (c) someone found a lattice-hash multiset collision with at most `2(M_MAX + |T|)` items, where T is the block's true change set;
- (d) a fact in section 4 is wrong.

### 6.1 The change-set argument, step by step

Let `T = {(k, a_k, b_k)}` be block B's **true** change set, with start values `a` and end values `b`. T holds only accounts with `h(a_k) ≠ h(b_k)`; writes that change nothing cancel out of the sum and can be ignored. Let `W = {(k, w_k, v_k)}` be the witness. Keys are unique within each.

1. Agave computes `L_B = L_P + Σ_T (h(b) − h(a))` (fact 4.5). The program checks `L_B = L_P + Σ_W (h(v) − h(w))`. Since `L_P` and `L_B` are the true values (5.3), the two sums are equal.
2. Rearranging gives `Σ_W h(v) + Σ_T h(a) = Σ_W h(w) + Σ_T h(b)`. Write the left side's items as multiset M1 and the right side's as M2. A multiset is a set that may repeat items. Each item is an `(address, account)` pair, and zero-lamport items are dropped since they hash to zero.
3. Suppose the claim about the target X is false: `v_X` is not X's true end value. X's end value is `b_X` if `X ∈ T`, otherwise its unchanged start value.
   - M1 contains the item `(X, v_X)`.
   - In M2, the only items with address X are `w_X` (from W) and `b_X` (from T, if `X ∈ T`). Each key appears at most once in each set, and the address is inside every item's hash.
   - `v_X ≠ w_X` because only entries with `h(pre) ≠ h(post)` become leaves (5.5), and `v_X ≠ b_X` by assumption. If `X ∉ T`, the only X item in M2 is `w_X`.
4. So `(X, v_X)` occurs more often in M1 than in M2, and M1 ≠ M2. Yet `Σ h(M1) = Σ h(M2)`. That is a multiset collision of the lattice hash with at most `2(m + |T|)` items, which is case (c).
5. The same argument with the roles of `v` and `w` swapped shows the claimed `pre` is also correct.
6. If the claimed `post` is "absent" (`v_X = None`), it is not an item, so step 3 doesn't apply directly. But then `w_X` exists (only entries with `h(pre) ≠ h(post) = 0` become leaves, 5.5), and step 5 runs on `w_X`:
   - a false `w_X` gives a collision;
   - a true `w_X` means X really started at `w_X`. Then its true end is either absent (the claim is true) or some `b_X ≠ w_X`. In that second case `(X, b_X)` is in M2 but nowhere in M1, which again gives a collision.

### 6.2 Attacks considered

| # | Attack | Stopped by |
|---|---|---|
| A1 | Prove a block that isn't final | BLS certificate check against V, plus the ancestry chain (5.1) |
| A2 | Swap in a different block's footer, a fake footer in transaction data, or a footer-shaped component in the never-replayed prefix before an UpdateParent | Shreds checked against B's certified FEC roots; starts at a DATA_COMPLETE boundary; must be footer then alpentick, ending at LAST_SHRED_IN_SLOT, the only position replay accepts (5.2) |
| A3 | Choose `L_P := L_B − Δ'` for a fake change set Δ' | `L_P` is bound by `bank_hash_P` inside `inner_B` (5.3); breaking it needs a SHA-256 second preimage |
| A4 | Claim an account that didn't change in B, with `pre = post = fake` (cancels out of the sum) | Only entries with `h(pre) ≠ h(post)` become leaves of the diff root (5.5) |
| A5 | List X twice, `(X: real_pre → fake)` and `(X: fake → real_post)`, which telescopes to the true change | Strictly increasing pubkeys (5.4) |
| A6 | Leave out some changed accounts | The sum no longer matches `L_B`: a collision |
| A7 | Add no-op or invented accounts to balance a fake | Needs a multiset collision (6.1) |
| A8 | Zero-lamport account with data, used to fake "exists" | Lamports 0 is normalized to `None`; the output says exists = false |
| A9 | Same-looking data at another address | `pubkey` is inside `h` |
| A10 | An ancestry link with a bogus parent slot or id | The block_id is rebuilt from the parent leaf; `parent_slot` must decrease; the footer shreds' slot must equal `slot_B`. **Residual:** a leader grinding a 2⁸⁰ truncated-Merkle collision (5.1); covered by the value cap |
| A11 | A proof from another network (testnet vs devnet) | The shred version is in the signed votes and the public values; the destination stores V per network |
| A12 | Huge witness to make proving impossible (liveness only) | `M_MAX` and the data size limit; the prover simply refuses |

---

## 7. Getting the witness (the hard practical part)

Nobody serves `L_P`, `L_B` or account values before and after a block over RPC. The lattice hash is an internal value, and RPC only shows accounts as they are now. We need our own node.

### 7.1 Option W1: a Geyser plugin (no change to Agave)

Geyser is Agave's plugin interface. A shared library loaded by the validator receives every account write, slot status change, block footer and transaction. We already run one, `plugins/footer-dump`.

The plugin `state-witness` would:

1. **Start up.**
   - Agave calls `update_account(…, is_startup = true)` for every account in the snapshot it loads.
   - The plugin stores each account in a local key-value database and sums `h(acct)` into `L_root`.
2. **Follow each slot.**
   - Writes arrive as `update_account(…, slot, is_startup = false)`. For each `(slot, pubkey)`, keep the last write; that is the account's end value in this block.
   - The start value is the account's value on the parent's fork.
   - Compute `L_slot = L_parent + Σ (h(end) − h(start))` per slot, keeping separate state for each unrooted fork. Forks are competing branches; they are dropped once a branch is rooted.
3. **Self-check every slot.** This is free and strong:
   - compare `blake3(L_slot)` with the `accounts_lt_hash checksum` Agave logs for that slot;
   - rebuild `bank_hash` (4.4) and compare it with the footer's `bank_hash`, which `footer-dump` already captures.

   Any missed write (a sysvar, rewards, the footer's vote-state updates, rent, an epoch boundary) shows up at once as a mismatch.
4. **Serve.** Answer "witness for (slot B, account X)" with `inner_P, L_P, signature_count_B, last_blockhash_B, L_B, W`.

**Risks:**
- Does Geyser report *every* store? Agave stores some accounts outside transactions (`store_accounts_without_stakes_cache`, and the "off-chain" lattice-hash updates such as the footer's vote-state burst). The self-check settles this empirically.
- How large is devnet's state on disk and in memory? Unknown, measured in S2.

### 7.2 Option W2: a small Agave patch

At freeze time, Agave already has every changed account's previous and new version, because it uses them to update the lattice hash (`runtime/src/bank/accounts_lt_hash.rs`). A patch of about 50 lines could write `(slot, L, change set)` to disk.

- **Pro:** no separate copy of the state.
- **Con:** we run a modified validator. It is non-voting, so this doesn't affect consensus, but it must be rebased on every Agave release.

**Which option where (review 2):**
- W1 needs a second full copy of the state and a full rehash at startup. That is fine for a local cluster or devnet, but **not realistic for mainnet**, where W2 is the path.
- Plan: W1 locally and on devnet, W2 for mainnet.

### 7.2b A ready-made oracle for S1: Agave's bank-hash details

Agave can already dump, per slot, everything needed to check the formulas: `runtime/src/bank/bank_hash_details.rs` (v4.4.1), written by `ledger-tool` (`ledger-tool/src/main.rs` ~710). It records:
- `parent_bank_hash`, `signature_count`, `last_blockhash`, `accounts_lt_hash_checksum`;
- every written account;
- each transaction's status.

So S1 can settle facts F-BH and F-LT **offline**, by replaying a ledger with `ledger-tool`, before any plugin exists. The same dump then serves as the reference the plugin's output must match.

Two details *(corrected in review 3)*:
- **"Before" values:** the dump holds each written account's value *after* the slot. The "before" value comes from the most recent earlier slot that wrote that account, or from the snapshot or genesis if none did, not simply from the previous slot's dump.
- **A starting L:** the dump holds only the *checksum* of `L` (its BLAKE3), not `L` itself. The offline check needs a known starting `L`. A local cluster replayed **from genesis** gives one: sum `h(acct)` over the genesis accounts, then apply every slot's changes, and compare the checksum each slot.

### 7.3 Who can produce proofs

**Anyone running such a node.** Witnesses are not trusted: a wrong witness just fails the checks. So an operator can't lie, only refuse, and anyone else can run the plugin. Solana itself prunes old state, so for old blocks someone must have kept the witness. That is the same archive question as for shreds.

**Outbox payload hashes** come from RPC transaction logs (8b), which RPC nodes also prune. Claimants depend on them for liveness, not safety, since the chain check verifies them. So a relayer should archive them, or read them from the witness node's own transaction records.

---

## 8. The application: cross-chain order settlement

**User story.** Ada is on Base and wants to pay a merchant 100 USDC on Solana.

*(The main text below uses the outbox design from 8b, as S4 builds it. Earlier versions described a bespoke receipt account instead.)*

1. **Place (Base).** Ada calls `Escrow.place(order)`, locking 100 USDC plus a fee.
   ```
   order        = { merchant (Solana address), mint (Solana USDC), amount_out,
                    deadline_ts (unix seconds), amount_in, user, nonce }
   order_id     = keccak256(chain_id, escrow address, order fields, amount_in)   (amount_in included so the fill commits to what the solver is paid)
   refund_after = deadline_ts + margin (for example 24 hours)
   ```
2. **Fill (Solana).** A solver calls the fill program's `fill(order fields, solver_evm_address, shard)`. In one transaction, the program:
   - recomputes `order_id` with Solana's keccak syscall (a built-in fast function);
   - checks `Clock.unix_timestamp ≤ deadline_ts`, a **weak guard only** (see "Clocks" below);
   - checks the destination token account **belongs to the merchant**: its token-account owner equals `merchant` and its mint equals `mint`. Checking the mint alone would let a solver pay into its own account of the same mint;
   - transfers `amount_out` of `mint` from the solver to that account, using SPL Token `transfer_checked`, which also checks the mint and decimals;
   - creates a per-order marker PDA at seeds `["filled", order_id]`, which makes a second fill of the same order fail. It uses **transfer + allocate + assign** rather than `create_account`: anyone can send lamports to the PDA's address in advance, and `create_account` fails on an address that already has lamports. The program has no instruction that closes a marker;
   - emits `(order_id, solver_evm_address, amount_out, mint, merchant)` through the `outbox` (8b), from emitter shard `shard` (one of a fixed set, 8b).

   If the transfer fails, the whole transaction fails: no marker, no message (4.6).
3. **Prove.** Huginn proves B's diff root (5.5) once and stores it in the registry (5.7).
4. **Claim (Base).** `Escrow.claim(setHash, slot_B, emitterPre, emitterPost, merkleProof, payloadHashes, index, payload)` checks:
   - the opening of the emitter account against the registry, and that `setHash` and the recorded depth meet the escrow's policy;
   - `post.owner == OUTBOX`, and `post.data.emitter` is one of the fill program's emitter shards, a fixed list set at deployment (8b);
   - the chain step from pre to post through `payloadHashes`, with `sha256(payload) == payloadHashes[index]` (8b);
   - the payload's `order_id` matches an order that is open and not marked paid.

   **No rule reads lamports.** Anyone can add lamports to any account.

   Then it marks the order paid and sends `amount_in` to `solver_evm_address`, read from the payload.
5. **Refund.** If the order is still open after `refund_after`, Ada takes her money back. No proof is needed, and refunds are never paused.

**Why a failed fill can't be claimed:** a failed transaction emits no message, so there is no change to prove. The original plan needed a human arbiter for this case; here it becomes a proof.

**Why the emitter is checked by its data, not its address:**
- Re-deriving a PDA on Base would need an "is this point on the Ed25519 curve" check, which is expensive on EVM.
- Only the outbox program can write accounts it owns, and it writes the emitter key into the data itself, after checking that key signed (8b). So `owner == OUTBOX` plus `data.emitter` is enough.

**What we trust in the fill and outbox programs:**
- their code;
- that both are **immutable**: their upgrade authority is removed, so nobody can swap the code.

Reviewers should read them; they are small on purpose.

**Clocks: what protects whom** *(corrected in review 3)*
- **Under Alpenglow, Solana's clock is not a stake-weighted estimate.**
  - Each leader sets the time in its footer.
  - Replay only requires it to be later than the parent's time and at most the parent's time plus twice the elapsed slot time (`enforce_nanosecond_clock_bounds`, Agave v4.4.1 `runtime/src/block_component_processor.rs` ~715–782; SIMD-0363).
  - So the clock can **lag real time without limit** if leaders keep it low. Honest leaders can catch it up at most twice as fast as real time.
  - Transactions in B also see the clock from B's parent's footer, one block stale.
- So `Clock.unix_timestamp ≤ deadline_ts` **can pass after Ada has refunded**. No margin fixes that.
- The race is fundamental to any time-out escrow across two chains. The design puts all of its risk on the solver:
  - **Ada is always safe.** She either gets her refund, or the merchant was paid and the solver is repaid from her escrow.
  - **The solver's protection is to fill early** and to sign with a **recent blockhash, never a durable nonce**. A recent blockhash expires after about 150 slots, so a signed fill can't land long after the solver decided to send it. Even this is counted in slots, so it weakens if Solana slows down.
  - The solver must also get the proof onto Base before `refund_after`. If Huginn's proving is down, the solver can lose that race too.
  - `margin` gives the solver time for both. It doesn't make the race go away.

---

## 8b. One Huginn: one program, two kinds of claim, and an outbox (added in review 2)

Instead of a second proof type next to Layer 1, Huginn becomes **one SP1 program with one key**:

1. **A shared step `finalized(B)`:** the certificate for D, the ancestry chain down to B (5.1), and optionally `N` descendant footers (5.1b).
2. **A tagged list of claims about B, in the same proof:**
   - **Transaction claim**, meaning "these keys signed this, and it is in B." It covers inclusion (`TxInclusion`) **plus Ed25519 signature checks** (section 11). It is cheap and needs no node; shreds come from repair.
   - **State claim**, meaning "this is what B changed." It covers the bank-hash opening (5.3), the delta opening (5.4) and the diff root (5.5). It needs a witness node (section 7).

   The two meanings differ, and apps must pick the one they need. A transaction claim never means the transaction succeeded.

### The outbox: a generic Solana program instead of bespoke receipts

A small, immutable program, `outbox`, that any Solana program or key can use to send a message to other chains: `emit(payload)`. A failed transaction emits nothing (4.6). Order settlement (section 8) is one use of it, and the sequence number replaces the memo-nonce parsing the current examples use.

**Refinement of review 2's proposal: a hash chain, not a "latest message" slot.**
- Review 2 proposed that `emit` write `(emitter, sequence, payload hash)` into one account per emitter.
- But a state proof shows only each account's value at the **start and end of a block**. If an emitter sends two messages in one block, the first is overwritten and can never be proven.
- So the emitter's account holds a running chain instead:
  ```
  seq'   = seq + 1
  chain' = sha256( chain | seq' u64 LE | sha256(payload) )
  ```
**Emitter account layout:** `emitter [32] | seq u64 | chain [32]`, at the PDA `["emitter", emitter]` of the outbox.

**Delivery rules** *(review 3)*:
1. **New emitters.** If the opened `pre` is absent, or **not owned by the outbox** (for example a pre-funded, system-owned address), the destination starts from `seq = 0` and `chain = 0^32`. Otherwise it starts from `pre`'s data.
2. **The emitter key is in the data.** Base can't derive PDAs (section 8), so it reads `post.data.emitter`. This is trustworthy only because `post.owner == OUTBOX` and the outbox writes that field itself after checking the emitter signed.
3. **Hashes for the rest, the payload for one.**
   - To consume message `i` of the k messages an emitter sent in block B, the claimant passes all k **payload hashes** and only payload `i` in full.
   - The destination recomputes the chain from the start to `post.chain`, checks `sha256(payload_i) == hash_i`, and checks that the sequence numbers run from `pre.seq + 1` to `post.seq`.
   - Requiring every payload in full would force one claimant to supply everyone else's messages. Payloads emitted through a CPI are not in the block's bytes at all.
   - Where the hashes come from: the outbox also logs `(seq, payload)` with `sol_log_data`, which anyone can read with RPC `getTransaction`. Logs aren't trusted; the chain check verifies them.
4. **Shard the emitters.**
   - One emitter account would make every fill take the same write lock (Solana runs transactions that write the same account one after another), so throughput would collapse.
   - Review 3 suggested one emitter per solver. *Refined here:* Base can't derive PDAs, so an escrow couldn't tell a per-solver PDA of the fill program from any other key.
   - Instead the fill program uses a **fixed set of S shard emitters**, for example `S = 16` PDAs `["shard", i]`. Their keys are computed once at deployment and stored in the escrow, where anyone can recheck them off-chain.
   - **The solver chooses the shard freely**; the program enforces only `shard < S`. This lets solvers avoid a busy shard. Either choice is safe, since only the fill program can sign for any shard (review 4).

**Who the emitter is.**
- Solana doesn't tell a program which program called it.
- So the emitter is either a signing key, or a PDA that the calling program signs for with `invoke_signed`. This is the same pattern Wormhole uses for its emitters.
- The emitter account is created with transfer + allocate + assign, so pre-funding its address can't block it.

**The fill program on top of the outbox.**
- The fill program keeps its own per-order marker, only to refuse a second fill.
- It emits `(order_id, solver_evm_address, amount_out, mint, merchant)` through `outbox`, from one of its shards.
- The escrow consumes that message (section 8).

---

## 9. Costs (estimates, to be measured)

- **Hashing in the proof program:**
  - SP1 v6.10 has no BLAKE3 precompile (a built-in fast circuit), so BLAKE3 runs as ordinary code.
  - `h(acct)` costs about `32 + ⌈len/64⌉` BLAKE3 compressions. An estimated 2–3k cycles each gives about 80k cycles for a small account.
  - Two hashes per changed account: 500 changed accounts ≈ 80M cycles.
  - A 10 MB account ≈ 160k compressions ≈ 400M cycles.
  - For comparison, today's whole inclusion proof is 5.6M cycles.
- **SHA-256 over 2,080 bytes:** cheap (SP1 has a SHA-256 precompile).
- **Witness size:** dominated by account data. Most devnet blocks should be small under Alpenglow, since votes are no longer transactions. Epoch-boundary blocks with reward payouts can be very large; avoid them.
- **Gas on Base** *(updated in review 3)*:
  - **Registering a block** (verifying the proof and storing its root) costs about what a Layer 1 submit costs today, about 390k. It is paid **once per block**, by whoever first needs that block.
  - **Each opening** costs a SHA-256 precompile call per Merkle level (about 17 levels for 100k leaves), plus `d(pre)` and `d(post)` over the account data, plus calldata. That is roughly 10–30k gas for a small account.
  - **An outbox delivery** adds one SHA-256 per message the emitter sent in that block.

If busy blocks are too big, the change set can be split across several proofs, each proving a partial sum, and combined with SP1 proof aggregation. That is future work.

**Measure on mainnet-shaped blocks, not devnet ones (review 2).**
- Devnet blocks are small. A busy mainnet block can change thousands of accounts: at 5,000 changed accounts and about 80k cycles per hash, that is about 800M cycles.
- The diff root (5.5) makes this a **once-per-block** cost shared by every account opened from it.
- S3's stop criterion is judged on a block shaped like mainnet's. Its count of written accounts can be estimated from mainnet `getBlock` without running a mainnet node.

## 10. Trust assumptions, stated plainly

1. **Solana's validators:** the Alpenglow safety bound holds, so no conflicting blocks get finalized.
2. **The validator set V on the destination is the real one.** Today an owner posts it, with a public check script. This is a known, separate gap; trustless set updates are a separate research item.
3. **Cryptography:** SHA-256, BLS12-381, BLAKE3 as an XOF, and the lattice hash as a multiset hash with bounded multiset size (section 13, Q1).
4. **Agave's rules are as described in section 4.** Firedancer, the second Solana client, must compute the same bank hash or the network would split, so the rules are consensus rules, not one implementation's choice.
5. **The Solana programs the application relies on, the `outbox` and the fill program:** small, immutable, reviewed.

## 11. Known limits and change risks

- **Fast leader handover (UpdateParent), a gap that also affects Layer 1:**
  - With the feature `alpenglow_fast_leader_handover` on, a block may contain an UpdateParent marker. Replay then starts at the marker's FEC set, and the components before it (the "prefix") are **never replayed** (Agave v4.4.1 `ledger/src/blockstore_processor.rs` ~2073, `core/src/replay_stage/dead_slots.rs`). The prefix is still covered by the block_id.
  - **State proof:** safe, because of the positional footer rule in 5.2.
  - **Layer 1 (inclusion proofs):** `TxInclusion::verify` accepts a transaction from any component, and Huginn does not verify transactions' Ed25519 signatures; it relied on replay rejecting blocks with bad signatures. A leader could put an unsigned transaction "from" a controller key in the prefix and have it proven.
  - **Status:** not exploitable today. The feature ID is a placeholder (`FastLeaderHandover11111111111111111111111111`), and the account doesn't exist on devnet (checked 15 Nov 2026 against devnet 4.5.0-beta.0).
  - **Fix now, not only before the feature ships (review 2):** verify each proven transaction's Ed25519 signatures inside SP1, which has an Ed25519 library.
    - Apps like `SolanaControlled` take *all* their authority from signatures Huginn currently never checks.
    - With the check, a 2⁸⁰ Merkle collision or an unreplayed prefix can at most replay a genuinely signed transaction. It can no longer forge a command.
  - Signatures alone don't make a prefix transaction *executed*. That's fine for Layer 1, which only ever claims inclusion, but app documentation should say so.

- **The validator set is still the root of trust, and fixing it needs the state proof.**
  - Today an owner posts V, and stakes come from live RPC values (`stakes_exact: false`).
  - The trustless fix, `crates/handover`, reads the next epoch's keys from account changes. So the state proof is needed to *finish* Layer 1, not just to extend it.
  - **Conflict found in review 2:** `handover` planned to read BLS keys from the **epoch-boundary block's** change set, which section 9 says is too large to prove.
  - **Proposed resolution:** the planned recorder program copies each validator's BLS key and frozen stake (`sol_get_epoch_stake`) into its own account over many ordinary blocks, a few validators per transaction. The state proof then opens that account from small blocks, never the epoch-boundary block.
  - **Unsolved subtlety (review 3; must be answered before the recorder is designed):**
    - The set that signs epoch N's certificates is a **snapshot of stakes and vote accounts taken at the start of epoch N−1**. This was verified on devnet on 11 Nov 2026; `ledger-dump --fix-set` exists because of it.
    - A recorder reading *live* vote accounts during epoch N−1 can record a BLS key registered **after** the snapshot, which is not the key the certificates use.
    - `sol_get_epoch_stake` gives the frozen *stake*, but nothing gives programs the frozen *key*.
    - **It doesn't allow forgery.** A wrong recorded key affects only that validator's own signatures, and carries only its own stake.
    - **But the liveness impact is large** *(review 4)*. An aggregate certificate verifies only if every signer's key is right. On mainnet most certificates include most validators. So **one validator rotating its BLS key in one transaction makes nearly every certificate of that epoch unverifiable**, and an honest rotation does the same by accident. The light client would stall for the whole epoch.
    - **Leaving the validator out doesn't work.** Bitmaps index validators by rank, so removing one shifts every rank after it and breaks every certificate. Its slot must stay in place, and certificates it signed still can't verify without its real key. *(Version 4 listed a `--fix-set`-style exclusion as option (d); review 4 showed it can't work.)*
    - **How a key changes** (Agave v4.4.1, `programs/vote/src/vote_state/handler.rs` `set_new_authorized_voter`, ~119–151; to confirm on devnet's 4.5.0-beta.0):
      - The BLS key is changed only there.
      - The same call inserts an entry in the vote state's `authorized_voters` map for a *target epoch*, and a second change for the same target epoch is refused (`TooSoonToReauthorize`).
      - The key itself is overwritten at once, with no record of when it takes effect.
    - **Options:**
      - **(a) Flag, using the trace.** The recorder reads `authorized_voters` and flags any validator with an entry newer than the snapshot as "key may have changed." There will be false positives, because a voter change without a new key leaves the same entry.
      - **(b) Recover the old key cheaply.** For a flagged validator, the key in force is the *pre* value of its vote account in the one block that made the change, opened from that block's diff root (5.5). One opening per flagged validator, not a proof of the epoch boundary.
      - **(c) SIMD-0676**, under which the runtime publishes the whole set in one account at each epoch start. This remains the clean fix if it lands.
    - **Recommended:** (a) plus (b) until (c) exists.

- **The light client keys the epoch on one slot.** `SolanaLightClient.verify` uses `epochOf(out.slot)` for both the validator set and the conflict record. With ancestry, the certified block D and the proven block B can differ, even across an epoch boundary.
  - The fix: look up the set for **D's** epoch and record **both** D and B.
  - This is part of S0.

- **SIMD-0301** (footer carries the *parent's* bank hash):
  - To prove B, read the footer of a child C whose parent is B.
  - The ancestry chain already proves the C → B link.
  - Only the choice of footer changes.
- **Asynchronous execution** (future Solana work), if the bank hash lags several blocks behind: the same idea applies with a longer chain.
- **Bank-hash or lattice-hash formula changes** through feature gates (switches that turn a protocol change on at a set slot). The proof program must pin a version per slot range.
- **Hard-fork slots** are not supported.
- **The state is "as of block B"**, not "now." Fine for append-only records such as the outbox chain; anything mutable needs care.
- **Huge accounts** are slow to hash in the proof and costly to open on the EVM (the opening passes the account data as calldata); very large ones may be impractical.

## 12. Plan and kill criteria

| Step | Work | Done when | Stop if |
|---|---|---|---|
| **S0** | Fixes the shipped product: some slots can't be proven today (2 of 7 in the first sample, 1 of 20 transaction-bearing slots in the acceptance loop; the true rate is unknown). Ancestry chain (5.1) in the core crate, the SP1 program and `shred-repair fetch`; the light client looks up D's epoch and records D and B. | 20 devnet runs in a row pass, including skipped-certificate slots. Rejection tests: a broken link, a non-decreasing slot, a wrong slot, a cross-epoch D/B pair. | — |
| **S0b** | Ed25519 signature checks for every proven transaction, in SP1 (section 11). | A transaction with a corrupted signature in otherwise valid shreds is refused. So is an unsigned transaction placed in an UpdateParent prefix of a synthetic block (the feature isn't live, so the block is built for the test). The cycle cost is recorded. | — |
| **S1** | First **offline**: replay a local cluster's ledger with `ledger-tool` and its bank-hash details (7.2b) to settle F-BH and F-LT. Then the `state-witness` Geyser plugin, checked against the same dump every slot. | One hour, every slot matches, including empty blocks, forks and an epoch boundary. Settles F-BH, F-LT and F-FOOT. | Mismatches we can't explain from Agave's source: stop and report before building anything else. |
| **S2** | Devnet non-voting node with the plugin: RAM, disk, startup time, cost. | Numbers recorded; decide devnet or local for the demo. | Cost far beyond a demo budget: use a local cluster for the demo and say so. |
| **S3** | Sections 5.1b–5.7 in `alpenglow-core`, the SP1 program and a `StateRegistry` contract; cycle measurements. | A real account opened from a diff root on Base. Rejection tests for A2–A12, each as a unit test. Cycles measured on a **mainnet-shaped** block. | Cycles on mainnet-shaped blocks far beyond the prover network's limits: split proofs (section 9). |
| **S4** | `outbox` program, then the fill program on top of it (both immutable), plus Escrow (Base). | Four scenarios: filled; never filled (refund); **failed fill (claim impossible, no message)**; forged proof rejected. Plus: a pre-funded marker or emitter address doesn't block a fill, and two messages in one block are both delivered. | — |
| **S5** | Control-room GUI shows the flow; record. | One recording from order to payout. | — |

Each step records its evidence in a quest folder: inputs, outputs, logs, and the exact Agave version.

## 13. Open questions for reviewers

1. **Lattice hash with an adversary who picks the items.**
   - SIMD-0215's security argument partly relies on "both states are *valid* Solana states."
   - In our use, the adversary chooses W freely (any addresses and data), but W has at most `M_MAX` items, and `T` is fixed by the real block.
   - Is a multiset collision with that bounded size, where one side contains fixed items, as hard as we think?
   - Is there a better attack (for example generalized birthday / k-list attacks over (ℤ/2¹⁶)¹⁰²⁴) at our sizes?
   - Should `M_MAX` be smaller?
2. **Is `inner_P` safe as a free input?** We treat `inner_P`, `signature_count_B` and `last_blockhash_B` as preimage material only. Is there any way their freedom helps an attacker?
3. **Does any write bypass the lattice hash?** That is, can any account change without updating `accounts_lt_hash`, for example a sysvar written after freeze, or a special case for the footer's vote updates? If so, `L` would not describe the real state.
4. ~~**Footer position.**~~ **Answered in review 1:**
   - The footer is second-to-last, followed by the alpentick, and replay enforces exactly one (5.2).
   - With an UpdateParent, the block_id's parent leaf names the *updated* parent, so the ancestry chain follows the right one.
5. **Ancestry and finality.** Under Alpenglow, does a finalized block D imply that every ancestor in its block_id chain is final, including across an `UpdateParent` (fast leader handover)?
6. **Failed transactions (4.6).** Are there any account writes besides the fee payer and the durable nonce that survive a failed transaction?
7. **Application:**
   - is anything missing from the escrow's checks (section 8)?
   - is skipping the PDA-address check on Base really safe?

## 13b. Review log

**Review 1 (15 Nov 2026, internal agent review against Agave v4.4.1 source).**

Checked and confirmed:
- the on-chain milestones in section 0;
- `cargo test -p alpenglow-core -p alpenglow-lc` passes;
- facts F-BH, F-LT and F-FOOT in source.

Accepted and corrected in this version:
1. **Footer position (5.2):** the footer is followed by the alpentick, not last in the slot.
2. **UpdateParent prefix is never replayed:** documented in section 11, together with the Layer 1 signature gap it exposes.
3. **Hash strength:** the Merkle trees use 20-byte nodes, about 2⁸⁰ (5.1, 6).
4. **Section 8:** the fill program checks the destination account's owner, and receipts can't be closed.

Reviewer's view on the open questions:
- **Q1:** at 2¹⁷ items or fewer, a k-list attack is estimated around 2⁹⁰⁰ and lattice reduction is far out of reach. So `M_MAX` is bounded by proving cost, not security. This is an estimate, not a proof; outside review is still wanted.
- **Q2:** `inner_P` as a free input is safe; the fixed-length preimage chain pins it.
- **Section 6.1:** walked through with no gap found.

**Review 2 (15 Nov 2026, internal agent review: state proof plus a Layer 1 audit).**

- **Not covered:** the Foundry tests and an SP1 proof were not run; `app/`, `fetch`, shred-repair's wire code and the GUI were not reviewed.
- **Confirmed:**
  - There is no better trustless route today. The bank hash commits only to the parent hash, signature count, last blockhash and lattice hash; there is no receipt or status root. So opening a block's whole change set is the only way that needs no change to Solana.
  - The Layer 1 core (`finality.rs`, `shred.rs`, `block.rs`, `proof.rs`) shows no way to forge a certificate or an inclusion.

Accepted and written into this version:
1. **A pre-funded address can block a claim.** The claim now requires `pre` absent or not owned by the fill program; receipts are created with allocate + assign; no rule reads lamports (section 8).
2. **One clock:** the deadline is a timestamp checked on Solana, and `refund_after` is derived from it (section 8).
3. **Descendant confirmations** as a structural mitigation of the 2⁸⁰ limit (5.1b). *Refined here:* leaders hold 4-slot windows, so depth is reported as `N` plus the top slot, and apps judge how many leader windows it spans.
4. **Diff root:** one proof per block, and many cheap openings, including "X did not change" (5.5–5.7). Measure on mainnet-shaped blocks.
5. **Witness:** W1 for local and devnet, W2 for mainnet; Agave's bank-hash details are the oracle for S1 (7.2b).
6. **Handover conflict:** resolved by a recorder that writes over ordinary blocks (section 11).
7. **Ed25519 signature checks now** (S0b), and **S0 first**, since it fixes the shipped product.
8. **The light client looks up D's epoch and records D and B** (section 11, S0).
9. **One program with claims, plus an outbox** (8b). *Refined here:* the outbox keeps a hash chain, because a "latest message" account loses all but the last message emitted in a block.

**Review 3 (15 Nov 2026, internal agent review of version 3).** It found one wrong fact and six gaps, mostly in the parts added in version 3. All were accepted:

1. **Wrong fact, the clock:** under Alpenglow each leader sets the time within bounds relative to its parent, so the clock can lag without limit. Section 8 now says a timestamp check is a weak guard, and that the solver carries the race, protected by filling early with a recent blockhash.
2. **One public-values layout**, with a shared header and tagged claims (5.6).
3. **Registry** keyed by validator-set hash; a conflicting root is refused loudly; depth only rises (5.7).
4. **The program counts leader windows**; `N ≤ k`; each descendant footer is located by 5.2's position rule (5.1b).
5. **Outbox rules:** new-emitter start values, the emitter key in the data, hashes for the other messages, and shards. *Refined here:* a fixed shard set instead of per-solver emitters, because Base can't derive PDAs (8b).
6. **Handover key-snapshot subtlety** recorded as unsolved, with an impact analysis and options (section 11).
7. **7.2b precision:** where "before" values come from, and why a starting `L` from genesis is needed.

Also fixed: the main text of section 8 uses the outbox design; non-membership means "same value at both boundaries"; the empty tree is defined; the gas estimates are updated; S0b tests a transaction in an UpdateParent prefix.

**Review 4 (15 Nov 2026, internal agent review of version 4).**

- **Confirmed:** version 4 is consistent, with no new soundness gap in sections 5–8, and the fixed shard set is sound.
- **Accepted:**
  - the solver's free choice of shard is now stated (8b);
  - the handover liveness impact is corrected to "one rotation stalls the whole epoch";
  - the exclusion option is removed as unworkable;
  - options (a) and (b) are made concrete from `set_new_authorized_voter` (section 11);
  - the outbox is named in the trust assumptions;
  - log pruning is noted as a liveness dependency (7.3).
- **Reviewer's verdict:** ready for outside review; S0 depends on no open question.

## 13c. Build log

**S0 and S0b (15 Nov 2026): built and tested natively; the on-chain redeploy is pending.**

- **S0, ancestry:**
  - Section 5.1 is implemented in `crates/alpenglow-core` (`verify_ancestry`, 12 tests).
  - The real devnet slot that failed on 15 Nov (522,347,550: no footer ever certified it) now proves through slot 522,347,551, one link up, with the real certificate and the real validator set.
  - `shred-repair fetch` stops at the first footer that certifies the block or any later block; `ledger-dump` writes the links into the fixture.
  - Public values layout version 2 (section 5.6) is live in the Rust encoder and the Solidity decoder (version 1 still decodes).
  - The light client checks the set for D's epoch and records both blocks.
- **S0b, signatures:** every proven transaction's Ed25519 signatures are checked with strict verification (what Agave's sigverify applies). A changed signature, a made-up signature, another message, another key and an off-curve key are all refused (6 tests). A transaction that fails the check "doesn't count", like one that fails to parse, so another component can still hold the real one.
  - **Cost:** the real 3-signature transaction went from 5.6M to 8.2M cycles, about 0.86M cycles per signature with the unpatched `ed25519-dalek`. SP1's patched `curve25519-dalek` would cut this sharply but has to be fetched from GitHub (not done).
  - **Test coverage gap:** the UpdateParent-prefix case is covered at the signature-check level (an unsigned transaction with a real key and message is refused wherever it sits), not with a synthetic block, because building shreds with valid Merkle proofs by hand is heavy. The check is position-independent by construction.
- **Program key:** `0x0016688f7ccc114bb59741292a9d3a4ea3f40c4e1f8a0eea5a7032ffd7ef854f` (after the review-5 fixes; earlier builds were `0x00174b33…` and `0x0071837d…`), from a Docker build; `zk/check-program-key.sh` (the same-bytes-at-another-path check) has not been run on it yet.

**S1, offline (15 Nov 2026): the three facts the proof relies on hold on a real Agave replay.**
- A local Alpenglow chain's ledger (Agave 4.4.0) was replayed with `agave-ledger-tool` to slots 156,069 to 156,072, once with `--write-bank-file` (the bank-hash components) and once with `accounts --include-sysvars` (every account). `examples/check_bank_hash.rs` rebuilt the lattice hash and the bank hash from the accounts with our own code (`src/bank.rs`).
- **F-LT** (per-account hash, element-wise sum): the checksum equals Agave's, for every slot.
- **F-BH** (bank hash formula): the rebuilt hash equals Agave's, for every slot.
- **F-FOOT** (the footer of block B carries B's own bank hash): the rebuilt hash equals the footer's, **for one slot** (156,070). Slots 156,071 and 156,072 were checked against Agave's bank hash, not their footers.
- Notes: ledger-tool prints balances as f64 SOL, which loses the faucet's last lamports (the parser recovers them by matching the checksum); and `accounts` omits sysvars unless `--include-sysvars` is given.
- **Not yet done:** the live check on devnet's Agave (4.5.0-beta.0), the Geyser witness plugin, and the devnet witness node (S1 live, S2). The offline result settles the formulas for 4.4.0 only.

**S3 (15 Nov 2026): the state proof is built and tested on real data.**
- `crates/alpenglow-core/src/state.rs` implements sections 5.1b to 5.5 (footer opening, bank-hash opening, delta opening, diff root, descendant confirmations with the leader-window count). `proof.rs` carries it as `ClaimInput` / `ClaimOutput` (public values version 2, claim tag 2).
- 22 tests on the real block (slot 156,070) cover the attacks in section 6.2 that can be written as a test against a witness (the cases listed in the test file). The 2⁸⁰ truncated-Merkle bound and the lattice-hash collision resistance are assumptions, not tests. The zkVM program runs it: **27.5M cycles** for a block with 6 changed accounts (two of them 160 KB sysvars), with 2 footers opened and 2 ancestry links: 8.6M of it is reading the 1.1 MB input, 4.7M the certificate, 14.2M the claims. The read cost grows with the change set's data; storing the lattice hashes as raw bytes instead of 1,024 separate u16 values would cut part of it. That is a devnet-sized block; a mainnet-shaped block is not measured.
- `StateRegistry.sol` (5.7) with 21 tests; the Merkle paths in its fixture are computed by Rust and rechecked on the EVM side.
- **Not done:** a state proof has not been run through the prover network or verified on a chain.

**S4 (15 Nov 2026): the order-settlement contracts and programs exist; the Solana side has not run on a chain.**
- `Escrow.sol` with 20 tests (the happy path, two messages in a block, a pre-funded emitter, every refusal listed in section 8, refund timing).
- The outbox and fill programs (`solana/outbox`, `solana/fill`): host unit tests pass against reference values shared with the escrow (order id, payload, hash chain: computed independently with `cast keccak` and Python's `hashlib`). **They have not been compiled for SBF** (the SBF build needs crates that were not available offline), **nor deployed, nor run on a validator.** Everything that touches accounts and CPI (creating the PDAs, the token transfer, the outbox call) is untested.
- Deviations from section 8: the escrow stores the shard *state-account* addresses (the outbox PDAs) and checks the leaf's key against that list, instead of reading the emitter key out of the account data, which makes the key-in-data check redundant.



**Review 5 (16 Nov 2026, agent review of the built code).** Accepted and fixed:
1. **The escrow pinned one validator-set hash**, but sets change every epoch, so any fill proven under a later epoch's set could never be claimed. The registry now stores the certified slot of each record, and `StateRegistry.isCurrent(setHash, slot)` asks the light client whether that set is still the one for its epoch; the escrow uses it. Tests cross an epoch and replace a set.
2. **The escrow did not require lamports on the emitter account.** Zero lamports digest to zero whatever the owner and data say. Now required.
3. **`amount_in` was not in the order id**, so a fill did not commit to what the solver is paid. Added on both sides (Solidity, fill program), with new reference vectors.
4. **The light client refused a proof carrying both claims.** It now accepts a transaction claim followed by a change-set claim.
5. **Section 5.6 said transaction-only proofs open descendant footers**; they do not. Corrected.
6. Smaller: the ancestry-path length check in `state.rs` was a debug assertion (compiled out in the zkVM), now a real check; one public-values encoder instead of two; the escrow's message-count error no longer underflows; the fill program uses constant program ids (the outbox id is pinned at build time).
7. Log corrections: the S0 loop counts (below), the skipped-slot rate, "one slot" for the footer comparison, and that "every attack refused" means the listed tests.
8. Measured: the unexplained 8.6M cycles are reading the input.
9. Exercised: multi-link chains on real devnet blocks, forced with `ledger-dump --via`: 3, 4, 11 and 21 links, all verified.

Not taken: **removing the version-1 decoder.** The only real Groth16 fixtures in the repository are version 1; removing it would leave the on-chain verifier tests without a real proof. It goes when a version-2 Groth16 fixture is generated (needs the Docker prover).

**Review 6 (16 Nov 2026).** Review 5's suggestion to pass PDA bumps in the instruction data was implemented without requiring the canonical bump, which broke "one fill per order": a PDA has several valid bumps, so an order could have several markers (Ada self-fills with an odd bump, reclaims her escrow, and an honest solver's later fill is never repaid). Fixed before any deploy: the fill program derives the marker and the shard with `find_program_address` (canonical only); the outbox accepts only the canonical address when it creates an emitter account and checks the passed bump afterwards (an outbox-owned account holding the emitter's key can only be the canonical one). Tests: an odd-bump marker and shard are refused. The on-chain version of the test (a second fill of the same order with a different bump must fail) belongs in the validator integration script. Also: the version-1 decoder now states that no deployed program key can produce version 1.

**S0 acceptance loop, final:** 22 iterations; 20 passed (1 through a descendant), 2 failed while the machine's network was down ("Network is unreachable"; the next send failed too). The failed slot passed when fetched again afterwards. Plus 2 earlier runs in the zkVM. After devnet entered epoch 1210 (set fetched, `check-set` passed), 2 more iterations passed, one through a descendant: 22 passes in all.

## 14. Glossary

- **Ancestry chain:** a list of blocks from a certified descendant back to the target, each one's block_id rebuilt from its claimed parent.
- **Bank:** Agave's in-memory state for one slot. "Frozen" means its block is fully processed and its bank hash is computed.
- **Change set:** every account a block modified, with its value at the block's start and end.
- **CPI (cross-program invocation):** one Solana program calling another, here the fill program calling the token program.
- **Delta opening:** revealing a block's whole change set so anyone can check it against the lattice hashes before and after.
- **Escrow:** a contract holding money until conditions are met.
- **Intent / solver:** an order like "pay X here if someone delivers Y there," and the market maker who fills it with their own money and is repaid later.
- **Multiset collision:** two different collections of items whose hash sums are equal.
- **Preimage / second preimage:** an input that produces a given hash / a different input producing the same hash as a known one.
- **Public values:** the bytes a zero-knowledge proof commits to, which the verifying contract reads.
- **Witness:** the private inputs the proof program checks, here the shreds, hashes and accounts.

## 15. Sources

- SIMD-0215, accounts lattice hash: <https://github.com/solana-foundation/solana-improvement-documents/blob/main/proposals/0215-accounts-lattice-hash.md>
- Agave `runtime/src/bank.rs` (`hash_internal_state`), `runtime/src/bank/accounts_lt_hash.rs`, `accounts-db/src/accounts_db.rs` (`lt_hash_account`): <https://github.com/anza-xyz/agave>
- SIMD-0298 / SIMD-0301 status (roadmap issue): <https://github.com/solana-foundation/solana-improvement-documents/issues/324>
- SIMD-0064 transaction receipts and SIMD-0052 receipt root in the bank hash (proposed, never activated): <https://github.com/solana-foundation/solana-improvement-documents/pull/64>, <https://github.com/solana-foundation/solana-improvement-documents/pull/52>
- Huginn findings on block format, certificates and footers: `docs/research/findings.md`
- Agave Geyser plugin interface: <https://www.mintlify.com/anza-xyz/agave/api/geyser-plugin>

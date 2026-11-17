# Reference

Byte layouts, constants and pinned versions, each with where it comes from and how it is checked. Integers are little-endian (LE) unless marked big-endian (BE). `|` means concatenation.

---

## Pinned versions

| What | Version | Why pinned |
|---|---|---|
| Rust (main workspace) | 1.99.1 (`rust-toolchain.toml`) | one compiler for everyone |
| Agave crates (`agave-bls-cert-verify`, `agave-votor-messages`) | =4.4.1 | the validator release the reference checks match |
| `solana-hash`, `solana-bls-signatures`, `solana-keypair`, `solana-signer` | =4.7.0, =4.0.2, =4.0.1, =4.0.0 | as Agave 4.4 resolves them |
| `solana-transaction`, `solana-message`, `wincode` (tests) | =5.1.0, =5.1.0, =0.6.3 | the parser is compared with Agave 4.4's |
| `rocksdb` (ledger-dump) | =0.24.0 (librocksdb-sys 0.17.3+10.4.2) | reads the ledger Agave writes |
| SP1 (`sp1-zkvm`, `sp1-sdk`, `sp1-build`, `sp1-verifier`) | =6.10.0 | the program key depends on it |
| SP1 build image | `ghcr.io/succinctlabs/sp1@sha256:9363a700…99f1` (v6.10.0) | same program bytes on every machine |
| `bls12_381` | =0.8.0, patched (`sp1-patches/bls12_381` tag `patch-0.8.0-sp1-6.10.0`) | SP1's fast curve operations exist for this version |
| `sha2` | 0.10, patched in the zkVM (`patch-sha2-0.10.8-sp1-6.10.0`) | SP1's SHA-256 precompile |
| `ed25519-dalek` | 2.x | uses curve25519-dalek 4.1, the version SP1 patches (dalek 3 moved to 5.x) |
| Footer-dump plugin | Rust 1.98.1, `agave-geyser-plugin-interface` =5.0.0 | must match the validator's compiler and interface (Agave 4.4.x) |
| Solana programs | `pinocchio` 0.12, platform-tools v1.57, SBPF v3 | Solana is blocking new deployments in older program formats (SIMD-0500) |
| Program tests | `mollusk-svm` 0.17 | runs Agave 4.4's program runtime |
| Solana CLI / validator | Agave 4.4.x | `solana-test-validator --alpenglow` |

---

## Clusters

| | mainnet | devnet |
|---|---|---|
| Alpenglow genesis certificate slot | 459,652,999 (epoch 1064) | 504,148,999 (epoch 1167) |
| Shred version | n/a in fixtures | 7016 |
| Slots per epoch | 432,000 | 432,000 |

A local `solana-test-validator --alpenglow` has one validator and a new shred version each time it starts from scratch.

---

## Votes and certificates

**Vote payload** (what validators sign, wincode encoding):
```
tag u8 | slot u64 | [block_id 32] | shred_version u16        block votes: 43 bytes; slot-only votes: 11 bytes
tags: 1 Notarize, 2 Finalize, 3 Skip, 4 NotarizeFallback, 5 SkipFallback, 6 Genesis
```
FinalizeFast signs the *Notarize* payload. BLS hash-to-curve tag: `BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_`.

**Thresholds:** 60% Notarize, Finalize, Skip and NotarizeFallback; 80% FinalizeFast; 82% Genesis.

**Signer bitmap:** `version u8 | bit count u16 | bits`, version 0 = plain bits (least significant first). Only version 0 is used for finality.

**Signatures:** 96 bytes compressed in footers, 192 bytes uncompressed from `getAgGenesisCert`.

**Validator ranking** (Agave `BLSPubkeyToRankMap::new`):
1. drop vote accounts with zero stake or no BLS key;
2. drop *every* copy of a duplicated BLS key, and of a duplicated node identity;
3. sort by stake (highest first), then by compressed BLS key bytes;
4. rank = position. Total stake = the sum over this list.

**BLS key in a vote account** (VoteStateV4, version tag 3): option tag at byte 144, the 48-byte compressed key at 145..193.

**Validator set commitment:**
```
sha256( "huginn-validator-set-v1" | count u32 | for each validator in rank order: bls_pubkey [48] | stake u64 )
```

Checked by: `alpenglow-lc/tests/core_matches_agave.rs` (Huginn agrees with Agave on every captured certificate and 400 random ones) and `devnet_genesis.rs` (the real devnet genesis certificate).

---

## Shreds and block_id

**Data shred:** 1,203 bytes, an 88-byte header; payload `shred[88..size]`, `size` at bytes 86..88. Coding shreds are 1,228 bytes. 32 data shreds per FEC set.

**Flags:** DATA_COMPLETE `0b0100_0000` (last shred of a component), LAST_SHRED_IN_SLOT (last of the block).

**FEC set tree** (Agave `merkle_tree.rs`):
```
leaf = sha256( "\x00SOLANA_MERKLE_SHREDS_LEAF" | shred[64..proof_offset] )
node = sha256( "\x01SOLANA_MERKLE_SHREDS_NODE" | left[..20] | right[..20] )    only 20 bytes of each child
an odd last node pairs with itself
```
FEC set *i* chains to set *i−1*'s root; set 0 chains to the parent's last FEC root.

**block_id** (double Merkle root, same tree rules), leaves in order:
```
FEC root 0, FEC root 1, ..., sha256( parent_slot u64 | parent_block_id [32] | fec_set_count u32 )
```

Checked by: `alpenglow-lc/tests/local_block.rs` and `ledger-dump/tests/devnet_block.rs` (real blocks rebuild their certified block_id).

---

## Block contents

**Components:** consecutive data shreds up to one flagged DATA_COMPLETE form one component.
- Entry batch: `count u64 | entries`, `entry = num_hashes u64 | hash [32] | tx_count u64 | transactions`.
- Marker: `0 u64 | version u16 (=1) | variant u8 | len u16 | data`. Variants: 0 footer, 1 header, 2 update-parent, 3 genesis certificate. Header and update-parent data: `1 u8 | parent_slot u64 | parent_block_id [32]`.

**Block shape** (replay rejects anything else):
```
Header | [Genesis] | entries* | [UpdateParent] | entries* | Footer | Alpentick
```
The alpentick is one tick entry with `num_hashes == 1`; the footer is the second-to-last component and the block ends with LAST_SHRED_IN_SLOT.

**Footer** (`BlockFooterV1`): bank hash, producer time, user agent, then optional certificates. `block_final_cert` names the newest block the leader knows is final: `slot | block_id | final_aggregate | [notar_aggregate]` (the notar part only on the slow path).

**Transactions:** first byte below 0x80 = legacy or v0 (it is the signature count); 0x81 = v1 (SIMD-0385, up to 4,096 bytes, message first, signatures last). Checked by `alpenglow-lc/tests/tx_matches_agave.rs`: 300,000 random and mutated transactions, same verdicts and fields as Agave's parser.

---

## State

**Bank hash** (Agave `hash_internal_state`):
```
inner     = sha256( parent_bank_hash [32] | signature_count u64 | last_blockhash [32] )
bank_hash = sha256( inner | accounts_lattice_hash [2048] )
```
Hard-fork slots add a step Huginn does not support (the check fails).

**Accounts lattice hash** (SIMD-0215): 1,024 u16 values, added element-wise modulo 2¹⁶.
```
hash(account) = 0                                           if lamports == 0
              = BLAKE3-XOF-2048( lamports u64 | data | executable u8 | owner [32] | address [32] )
checksum      = blake3( the 2,048 bytes )                   (what Agave logs)
```

**Diff root** (the change set, in address order):
```
d(account) = sha256( lamports u64 | data | executable u8 | owner [32] ),  or 0^32 if absent
leaf       = sha256( 0x00 | address [32] | d(before) | d(after) )         only accounts whose value changed
node       = sha256( 0x01 | left | right )                                 an odd last node moves up unchanged
diff_root  = sha256( 0x02 | leaf_count u32 | merkle_root )                 empty set: merkle_root = 0^32
```

Limits: at most 2¹⁶ changed accounts, at most 64 ancestry links. A leader window is `slot / 4`.

Checked by: `alpenglow-core/examples/check_bank_hash.rs` on a real replayed ledger (Agave 4.4, slots 156,069–156,072: lattice and bank hashes equal Agave's; the footer matched for 156,070) and `alpenglow-core/tests/state.rs` (22 tests).

---

## Public values (layout version 2, big-endian)

```
0     u8    layout version (= 2)
1     [32]  validator_set_hash
33    u16   shred_version
35    u64   certified slot D
43    [32]  certified block_id
75    u8    path (0 fast, 1 slow)
76    u64   signed_stake
84    u64   total_stake
92    u64   slot B (the block the claims are about; = D without ancestry)
100   [32]  block_id of B
132   u16   descendant footers opened
134   u16   later leader windows covered
136   u64   highest opened footer slot (= B when none)
144   u8    claim count
145   claims, each starting with a tag byte:
      tag 1  transaction  u32 component_start_shred | u32 entry_index | u32 tx_index | u16 length | tx bytes
      tag 2  change set   [32] bank_hash of B | [32] diff_root | u32 leaf_count
```
A transaction claim comes first when both are present. Encoded by `ClaimOutput::encode` (`crates/alpenglow-core/src/proof.rs`) and decoded by `HuginnOutput.sol` in huginn-evm.

---

## Order settlement

**Outbox emitter account** (PDA `["emitter", emitter]` of the outbox):
```
emitter [32] | seq u64 | chain [32]                      72 bytes
on each message: seq' = seq + 1,  chain' = sha256( chain | seq' u64 | sha256(payload) )
emit instruction data: bump u8 | payload (≤ 1,024 bytes)
```

**Order id** (`Escrow.orderId`, `keccak256` over packed big-endian fields):
```
keccak256( chain_id u64 | escrow [20] | user [20] | nonce u64 | merchant [32] | mint [32]
           | amount_out u64 | deadline u64 | amount_in [32] )
```

**Fill instruction data** (132 bytes, integers LE except `amount_in`):
```
chain_id u64 | escrow [20] | user [20] | nonce u64 | amount_out u64 | deadline u64 | amount_in [32] BE
| solver_evm [20] | shard u8 | decimals u8 | state_bump u8 | reserved [5] = 0
```

**Message** (124 bytes): `order_id [32] | solver_evm [20] | amount_out u64 BE | mint [32] | merchant [32]`.

**PDAs of the fill program:** marker `["filled", order_id]` (1 byte, canonical bump only); shards `["shard", i]`, i < 16.

Checked by: `programs/interface` unit tests (reference values shared with the Solidity escrow's tests) and `programs/tests` (the compiled programs in Agave's runtime).

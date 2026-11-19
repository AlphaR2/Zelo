# How Zelo works

Zelo lets a contract on another chain (Base first, any EVM chain the same way) learn two things about Solana, trusting only Solana's validators:

1. **a transaction is in a finalized block**, with its exact bytes: who signed it, which programs it called, with what data;
2. **what a finalized block changed**: "account X went from these bytes to these bytes", or "account X did not change".

This page explains both from the ground up. Byte-level layouts are in [`reference.md`](reference.md); the code is mapped in [`codebase.md`](codebase.md).

---

## 1. Words first

- **Slot**: a numbered time window, a fraction of a second, in which one validator, the **leader**, may produce a block. Leaders produce 4 consecutive slots each (a **leader window**).
- **Validator**: a computer that runs Solana, checks blocks and votes on them. Its voting weight is its **stake**, the SOL delegated to it.
- **Epoch**: 432,000 slots (roughly a day). The set of validators and their stakes is fixed for each epoch.
- **Alpenglow**: Solana's consensus protocol since 21 Oct 2026. Validators send signed votes; votes are merged into **certificates**.
- **BLS signature**: a signature scheme in which many signatures over the same message add up into one. A certificate is one merged BLS signature plus a **bitmap** saying which validators signed.
- **Shred**: a piece of a block, about 1.2 KB, the unit the network sends. 32 data shreds form an **FEC set** (FEC is *forward error correction*: extra coding shreds let a receiver rebuild lost pieces).
- **Merkle tree / root**: a way to fingerprint a list. Pairs of hashes are hashed together, level by level, into one **root**. A **Merkle path** (the sibling hashes on the way up) proves an item belongs, without the rest of the list.
- **block_id**: the fingerprint validators vote on: a Merkle root over the block's FEC set roots plus one leaf naming the **parent block**.
- **Account**: Solana's unit of state: an address (32 bytes) holding lamports (1 SOL = 10⁹ lamports), a data byte array, an owner program and an executable flag.
- **PDA** (*program-derived address*): an address computed from a program id and some **seeds**. Only that program can sign for it. A small number, the **bump**, makes the address valid; the highest valid one is the **canonical bump**.
- **zkVM / SP1**: a *zero-knowledge virtual machine*. You run an ordinary Rust program inside it and get a short proof that it ran to the end. "Ran to the end" means every check passed, since any failed check stops the program.
- **Groth16**: the tiny final proof format (a few hundred bytes) that EVM contracts check cheaply, for about 270k gas.
- **Public values**: the bytes the zkVM program commits to as its output. The contract reads them after checking the proof.

---

## 2. Proving a block is final

Under Alpenglow, a block is **final** (it will never be reverted) when either:
- **fast path:** validators holding 80% of the stake *notarized* it, in one certificate;
- **slow path:** 60% notarized it **and** 60% *finalized* its slot, in two certificates.

The certificates are not published by RPC. They travel inside blocks: every block ends with a **footer**, and a footer carries a certificate for some earlier block. Zelo reads them straight from shreds, so it needs no special data feed.

Zelo checks a certificate the way Agave (Solana's validator software) does:
1. decode the bitmap of signers;
2. look up each signer's BLS key and stake in the **validator set** for the certified slot's epoch;
3. add up their keys and verify the merged signature over the exact vote bytes, which include the slot, the block_id and the cluster's **shred version** (a number that tells devnet from mainnet);
4. check that the signers' stake meets the threshold, with exact integer arithmetic.

On the slow path the *Finalize* vote names only a slot, not a block. Zelo requires the *Notarize* certificate too, because that is the one that binds the block_id. Skipping it is a known trap.

**Where the validator set comes from.** The destination contract stores a hash of each epoch's set: every member's BLS key and stake in rank order. The proof says which set it used; the contract compares hashes. Today the set is posted by the contract's owner, after a public delay. Proving the epoch-to-epoch handover is designed but not built (see [`security.md`](security.md)).

### Blocks that never got their own certificate

A footer certifies only the **newest** block its leader knows to be final. So some blocks never appear in any footer: about one in four in a devnet sample. They are still final, because finalizing a block finalizes all its ancestors.

Zelo proves them through a certified **descendant** D. A block_id includes its parent's slot and block_id, so given each block's FEC roots, Zelo rebuilds the block_ids from D down to the target, one link per block. The slots must strictly decrease, and there are at most 64 links. The output names both blocks: the certified one and the target.

---

## 3. Proving a transaction is in the block

A leader writes a block as a sequence of **components**: *entry batches*, which hold the transactions, and a few *markers* (a header, the footer). Each component is split across consecutive data shreds; the last one carries a DATA_COMPLETE flag.

Every shred carries a Merkle path to its FEC set's root, and the roots rebuild the block_id. To prove a transaction, Zelo takes:
- all of the block's FEC roots (32 bytes each), which must rebuild the certified block_id;
- only the shreds of the **one component** that holds the transaction, each proven against its FEC root;
- the single shred **before** them, which must carry DATA_COMPLETE, so the run starts on a real component boundary and cannot be read "out of frame".

It then joins the shreds and parses the component **up to** the transaction, which it finds by its first signature. The parser reads legacy, v0 and v1 transactions exactly as Agave does, which is checked on 300,000 random and mutated transactions.

**Signatures are checked in the proof.** Each Ed25519 signature is verified strictly, as Agave's signature check does. Relying on Solana's replay is not enough. Under fast leader handover, the part of a block before an *UpdateParent* marker is never replayed, yet it is covered by the block_id. A transaction there could claim any signer.

**Inclusion is not success.** A failed transaction is still in its block (it pays its fee). To act only on things that really happened, prove **state** instead (next section).

**A summary instead of the bytes.** The proof can hand the destination a *summary* of the transaction instead of its bytes: who signed it and, for each instruction, its program and the sha256 hash of its data. Every check is the same. This is for publishing data: the data itself stays on Solana, where anyone can download it, and the destination keeps only its hash. Publishing needs inclusion only, since a failed transaction still carries its data in a finalized block.

---

## 4. Proving what a block changed (state)

### The fingerprints Solana already signs

Each footer carries the block's **bank hash**, a 32-byte fingerprint of the whole chain state after the block:

```
bank_hash = sha256( sha256( parent_bank_hash | signature_count | last_blockhash ) | lattice_hash )
```

The **accounts lattice hash** is a 2,048-byte value equal to the *sum* of a 2,048-byte BLAKE3 hash of every account. Each account's hash covers its lamports, data, executable flag, owner and its own address. Because it is a sum, one block's effect is simple:

```
lattice_hash(after B) = lattice_hash(before B) + Σ over accounts B changed ( hash(new value) − hash(old value) )
```

### The proof, step by step

1. **B is final**, through its own certificate or a descendant's (section 2).
2. **B's footer is read from B's own shreds**, at the only place replay accepts it: the second-to-last component, followed by one final tick entry and the end of the slot. A footer-shaped component anywhere else is refused.
3. **Open the bank hash.** Given the parent's inner hash and lattice hash, B's signature count and last blockhash, and B's lattice hash, the formula must reproduce the footer's bank hash. That pins both lattice hashes.
4. **Open the change set.** The witness lists every account B changed, with its value before and after, in strictly increasing address order. The parent's lattice hash plus the listed changes must equal B's lattice hash. A false or missing entry would need a *collision* of the lattice hash (two different sets of accounts with the same sum), which is believed to be infeasible.
5. **Commit the change set** as a Merkle tree, the **diff root**. Each leaf is `(address, digest before, digest after)`, in address order, with full 32-byte hashes. One proof then serves every account the block changed:
   - **membership:** a leaf proves "X changed from pre to post in B";
   - **non-membership:** two neighbouring leaves around X prove "X had the same value before and after B".

### Extra depth against short hashes

Shred Merkle trees hash only the first **20 bytes** of each child, so the leader of B could, with about 2⁸⁰ work, build two footers with the same tree position. The bank-hash chain uses full 32-byte hashes, and each block's bank hash includes its parent's. So the proof can also open the footers of B's descendants and check that their bank hashes chain back to B's. The output counts the distinct later **leader windows** this covers. An app can require, for example, two windows, so that a forger would have to have led every block up to the highest opened footer. A per-proof value cap is still advised.

---

## 5. The proof's output

One SP1 program proves both kinds of claim. Its public values (layout version 2, big-endian) are:
- **a header:** the validator set hash, shred version, the certified block D (slot, block_id, fast or slow path, signed and total stake), the target block B (slot, block_id), and the depth (descendant footers opened, leader windows, highest slot);
- **tagged claims:**
  - tag 1, a transaction: where it sits in the block, plus its bytes;
  - tag 2, a change set: B's bank hash, the diff root and its leaf count.

A proof may carry both claims. The exact offsets are in [`reference.md`](reference.md).

---

## 6. On the destination chain

In [`zelo-evm`](https://github.com/AlphaR2/zelo-evm):
- **`SolanaLightClient`** checks a proof against the pinned program key, the cluster's shred version, and the validator set stored for the certified block's epoch. It then records the transaction, so apps can ask "was this signature in a finalized block?".
- **`StateRegistry`** checks a proof with a change-set claim and stores `(block, bank hash, diff root, depth)`, keyed by validator set and slot. A conflicting record is refused; depth only rises. Apps then call `open` (membership) or `proveUnchanged` (non-membership) with a Merkle path.
- **`SolanaControlled`** is a base contract that a Solana key controls through signed memo transactions.

---

## 7. The example application: cross-chain order settlement

Ada, on Base, wants to pay a merchant on Solana without trusting a bridge.

1. **Place.** Ada locks `amountIn` of a token in the **Escrow** contract on Base, with an order: "pay `amountOut` of Solana token `mint` to `merchant` before `deadline`". The order id is a hash of all of these fields.
2. **Fill.** A solver calls the **fill** program on Solana. In one transaction it:
   - recomputes the order id;
   - checks the deadline, and that the destination is the merchant's token account for that mint;
   - pays the merchant;
   - creates a "filled" marker so the order can be filled only once;
   - emits a 124-byte message through the **outbox**.
3. **Outbox.** The outbox appends the message to an **emitter account** that only it can write. The account holds a running hash chain: `chain' = sha256(chain | seq' | sha256(message))`. If the payment failed, the whole transaction failed, and nothing was written.
4. **Prove.** A state proof shows how the emitter account changed in a finalized block. It is registered in the `StateRegistry` on Base.
5. **Claim.** The solver hands the escrow the account's values before and after, plus the block's messages. The escrow:
   - opens the account against the registry;
   - replays the hash chain;
   - finds the order's message;
   - pays the solver `amountIn`.
6. **Refund.** If nobody fills, Ada takes her money back after the deadline plus a margin. No proof is needed for that.

The fill program signs from one of 16 **shard** PDAs, so busy fills don't all write one account. Only canonical PDAs are accepted everywhere, so an order cannot get a second marker at another bump. Both Solana programs are meant to be deployed **immutable**: their code is part of what the escrow trusts.

---

## 8. What is trusted

- **Solana consensus:** Alpenglow's safety assumption (under 20% of stake malicious).
- **The validator set** posted for each epoch, until the handover proof exists.
- **SP1's proof system** and the pinned verifier contract.
- **Solana's vote program**, which checks each BLS key's proof of possession when it is registered.
- **For state proofs:** the lattice hash's collision resistance. The 20-byte Merkle nodes cost an attacker about 2⁸⁰; use descendant depth and value caps.
- **For the example app:** the code of the immutable outbox and fill programs.

The full threat model, known limits and review history are in [`security.md`](security.md).

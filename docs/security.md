# Security

**Status: not production-ready. Do not put value behind it.** Outside reviews and their outcomes are listed at the end. A real Groth16 transaction proof was verified on Base Sepolia on 14 Nov 2026. Ancestry proofs, signature checks, state proofs and the Solana programs are tested, but have not yet run on a real chain.

## Threat model

| Who | Can they forge a proof? |
|---|---|
| Anyone with shreds and certificates | No. The certificate pins `block_id`; Huginn rebuilds it. |
| A relayer | No. They can delay or reorder relays, not change what is proven. `SolanaControlled`'s nonce fixes the order. |
| Less than 20% malicious Solana stake | No (Alpenglow's assumption). |
| The light client's owner | **Yes, after the public delay**, by proposing a validator set whose keys they hold. This is the remaining root of trust. |

## Known limits

- **Owner trust.** Mitigated, not removed: sets count only after `setDelay`, are visible (`pendingSet`, `ValidatorSetProposed`), can be cancelled or replaced, and ownership moves in two steps. Real fix: prove the epoch-to-epoch handover in SP1. Until then use a multisig and watch the events.
- **Inclusion is not success.** Failed transactions are in blocks too.
- **Cancel is a race.** A signed command is public once final; anyone can relay it before a cancel's proof exists. Make revocable commands conditional inside the contract.
- **A failing command does not stall the queue.** It logs `CommandFailed` and spends its nonce; sign it again to retry. A gas guard makes a relayer who starves a command revert the whole call instead of spending its nonce.
- **Transaction formats.** Legacy, v0 and v1 are parsed and match Agave on millions of random inputs. A future format placed before a target would hide it until the parser is updated.
- **Program key.** The program key is a hash of the compiled program, and every destination contract pins it, so anyone must be able to rebuild the code and get the same key. A native build can't promise that across machines: cargo mixes the host's description (for example `aarch64-apple-darwin`) into each crate's internal ID, and those IDs set the order of functions in the file. A Mac and a Linux server built the same commit into files of equal size with different bytes (14 Nov 2026). So `zk/prover/build.rs` builds the program inside SP1's official v6.10.0 Docker image, pinned by its digest (a hash of the image's exact contents), with locked dependencies, fixed source paths and stripped symbols. `HUGINN_NATIVE_PROGRAM=1` builds natively for quick development only; never deploy that key. `zk/check-program-key.sh` rebuilds a copy elsewhere and compares; run it on two machines and compare the printed hashes.
- **Untested at scale.** Real multi-signer certificates verified so far come from devnet (20 validators, 4 or 5 signers per certificate, 13 certificates in epoch 1206). Mainnet-size sets (about 670 validators) are tested only with synthetic data.
- **Stakes are not proven exact.** A set built from RPC uses *live* stake, not the frozen epoch stake certificates are weighted by. `check-set` shows the set verifies real certificates (`stakes_exact: false`), which does not pin stake amounts: thresholds still pass with slightly wrong stakes. Exact stakes need the recorder (see the handover section). On mainnet `fetch validator-set` refuses outright while any member shows 0 live stake.
- **Dependency advisories.** `sp1-sdk` 6.10 depends on `crypto-bigint` 0.5 (RUSTSEC-2026-0387, timing variation). It is used only on the prover operator's own machine, to sign prover-network requests. It will be upgraded with SP1. `deny.toml` lists every accepted advisory with its reason.
- **Rogue-key attack.** Huginn relies on Solana's vote program having checked proof of possession for every BLS key.

## Validator-set handover (design, not built yet)

*Handover*: the light client learning epoch N+1's validator set from data that epoch N's validators (already trusted) certified, so no owner is needed. Planned shape: a *recorder* program on Solana writes each member's frozen stake (`sol_get_epoch_stake`) into an account; the block holding that write is proven to the light client; each member's BLS key is read from the account's state just before the epoch boundary.

**The 60% overlap rule, and what it proves.** The recorder writes set N+1's stakes during epoch N+1, in a block certified by set N+1 itself, which the client does not trust yet. So the client also requires that the signers of that block, matched key by key with set N, hold at least 60% of set N's stake. Argument:
1. Alpenglow is safe while less than 20% of an epoch's stake is faulty.
2. Signers with at least 60% of set N's stake still hold a large share of set N+1's stake, because stake moves slowly: at most 9% of all stake can activate or deactivate per epoch (`NEW_WARMUP_COOLDOWN_RATE = 0.09`). Roughly (60 - 9) / 109, or about 47%, far above 20%.
3. So at least one honest validator notarized the block, and an honest validator only notarizes a block after replaying it and reproducing its footer bank hash (Agave `replay_stage.rs`). So the recorder's data in that block is what Solana's runtime really computed.

**Assumption the 9% figure does not cover.** The admission filter (VAT: a fee each validator pays to be in the set, plus the top-2,000 cut) can add or remove *whole validators* regardless of the 9% rate, for example a large validator that was excluded for low balance rejoins with all its stake. So the client must also check that set N+1's total stake is at most set N's total times (1 + a growth cap, e.g. 10%). Above the cap it refuses and falls back to an explicit owner re-anchor (a declared trust event with a long delay that the guardian can cancel).

**Limit: use it for nothing else.** The overlap rule only shows that the recorder's block was honestly replayed. It must never be used to accept any block as *final*: a notarize aggregate holding 60-79% of stake uses the same signed bytes as a fast-finalize, and counting old stake cannot tell them apart. New members whose stake is not yet known count zero toward the 60%.

**Where the code is.** The light client already has the slot for it: `installProvenSet` and the `IValidatorSetSource` interface in `huginn-evm/src/sources/`. The sources are placeholders; the Rust side will be added when the handover is built.

**Missed epoch.** Each proven set must be certified by the previous one, so if nobody installs epoch N, epoch N+1 cannot be proven and the client refuses that epoch. Recovery is an owner *re-anchor*: a declared trust event that waits a long delay (`reanchorDelay`, 7 days by default), emits its own event, and the guardian can cancel it. Prevent it by running the handover from at least two independent machines and archiving each boundary block's account changes as they happen.

**Missing key entries.** A member's vote account is written in the epoch-boundary block (admission fee, usually also a voting reward). If those two amounts cancel, its state is unchanged and the delta proof has no entry for it. That must not lock the member out: fall back to the key from the recorder block's delta, and test this case.

## State proofs and the example application

- **Transaction signatures are checked inside the proof**, strictly, as Agave does. Replay is not enough: under fast leader handover, the part of a block before an UpdateParent marker is never replayed, yet it is covered by the block_id.
- **Blocks without their own certificate** are proven through a certified descendant. Each parent link is rebuilt from block ids, at most 64 links. The light client checks the validator set of the *certified* block's epoch, and records both blocks.
- **20-byte Merkle nodes.** Shred trees keep only 20 bytes of each child, so a block's own leader could forge a tree position for about 2⁸⁰ work. Mitigations:
  - state proofs can open descendants' footers, whose bank hashes chain back with full 32-byte hashes, and report the leader windows covered, so apps can require depth;
  - keep a per-proof value cap.
- **The lattice hash's collision resistance** is assumed for change sets in which an attacker chooses the account list. That question is open for outside cryptographic review.
- **The Solana programs** accept only canonical PDAs, so an order has exactly one possible marker and an emitter exactly one account. They must be deployed immutable. Their code, and the outbox id built into the fill program, are part of what the escrow trusts.
- **The fill program's deadline check is weak**: the cluster clock can lag. The escrow's refund margin covers it; solvers carry that race.
- **Not built yet:** the live witness recorder (a Geyser plugin that saves each block's account changes). Until then, state witnesses come from replaying a ledger with `agave-ledger-tool`.

## Reviews

| Round | Findings | Outcome |
|---|---|---|
| 1 | 8 (2 high, 2 medium, 4 lower) plus usability | All fixed, mitigated or documented. Nonce-bound commands, delayed validator sets, v1 transactions, key handling, conflict check in `verify`. |
| 2 | 3 (aimed at the round-1 fixes) | Failed commands no longer stall the queue; conflict records are kept per validator set so a bad-set episode can't poison real proofs; the cancel race is documented and pinned by a test. |
| 3 | Validator sets and observability | Set membership from `epochVoteAccount`; set fitting replaced by a pass/fail `check-set`; secrets redacted everywhere; transaction records kept per validator set; warmup epochs; the set delay fits within an epoch; on-chain verifier rejects an identity aggregate key and is differential-tested against Rust. |
| 4–7 | State-proof design (four rounds) and the code built from it (two rounds) | Footer position rule (footer, then the final tick, at the end of the slot); descendant depth against the 20-byte bound; change sets committed as a diff root; one program with tagged claims; the escrow checks the current validator set per epoch; `amountIn` in the order id; only canonical PDAs, so one marker per order. |

The attacks are kept as tests: `crates/alpenglow-core/tests/adversarial.rs`, and `test/CommandAndOwnerAttacks.t.sol` and `test/QueueAndSetAttacks.t.sol` in huginn-evm (`test_FIXED_*` was an attack that now fails, `test_HELD_*` always failed, `test_LIMIT_*` is a documented limit, `test_POC_*` is an open weakness).

## Reporting a problem

Open a private security advisory on the GitHub repository, or contact the maintainers directly. Please don't file public issues for exploitable bugs.

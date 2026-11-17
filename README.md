# Huginn

**Prove what happened on Solana to a contract on another chain, using only Solana's own validators.**

A contract on Base (or any EVM chain) can check, by itself:
- that a Solana **transaction** is in a block Solana's validators finalized, with its signers and instructions;
- what a finalized Solana block **changed**: "account X went from these bytes to these bytes".

There is no bridge committee and no multisig vouching for it. The check is Solana's own validator signatures, verified inside a zero-knowledge program, so the destination contract only checks one small proof.

> **Status: experimental. Do not put value behind it.**
> - **Transaction proofs work end to end.** On 14 Nov 2026 a real devnet transaction was proven on Succinct's prover network and accepted by a light client on Base Sepolia (one Groth16 proof, about 388k gas to submit).
> - **Built and tested, not yet on a real chain:**
>   - proofs for blocks that never got their own certificate;
>   - signature checks inside the proof;
>   - state proofs;
>   - the Solana programs and contracts of the order-settlement example.
>
> See [`docs/security.md`](docs/security.md).

## How it works, in five steps

1. **Finality.** Solana's validators sign each block's fingerprint, its `block_id`, and their signatures are merged into one *certificate*. Huginn checks the certificate against the validator set of that epoch.
2. **Inclusion.** A block travels as *shreds*: small packets, each carrying a Merkle path to the `block_id`. Rebuilding the `block_id` from shreds proves they are that block, and so is any transaction inside them. The transaction's signatures are checked too.
3. **State.** Each block's footer carries its *bank hash*, a fingerprint of all of Solana's accounts after the block. Huginn opens it down to the exact set of accounts the block changed.
4. **Proof.** Steps 1–3 run inside an [SP1](https://github.com/succinctlabs/sp1) zkVM program. It outputs a short Groth16 proof plus fixed-layout *public values*.
5. **Verify.** A contract checks the proof, then reads the public values: the block, the transaction, or the change set.

The full explanation is in [`docs/how-it-works.md`](docs/how-it-works.md).

## Quick start

You need Rust, plus Docker, which builds the zkVM program reproducibly. The full tool list is in [`docs/run.md`](docs/run.md).

```bash
cargo test --release                  # the verifier, against real blocks and Agave's own code
cd zk/prover && cargo run --release   # run the real local transaction through the zkVM (no proof)
```

## Layout

```
crates/                 the main Rust workspace
  alpenglow-core/       the verifier: certificates, shreds, block_id, transactions, state. Runs in SP1
  alpenglow-lc/         the same checks with Agave's own crates, to confirm alpenglow-core agrees
  fetch/                CLI: validator sets, shred version, genesis certificates from RPC
  ledger-dump/          builds checked block files from a ledger or from shred files
  shred-repair/         downloads a block's shreds from validators (Agave's repair protocol)
  redact/               keeps API keys in RPC URLs out of every printed line
zk/
  program/              the SP1 program: what a proof proves
  prover/               the huginn-prover library and `huginn` CLI
programs/               Solana programs of the order-settlement example (Pinocchio)
  interface/            shared layouts: emitter account, order id, message
  outbox/               leaves messages other chains read through a state proof
  fill/                 a solver pays a merchant and leaves a message for the escrow
  tests/                runs the compiled programs in Agave's runtime (Mollusk)
plugins/footer-dump/    optional Geyser plugin that captures block footers
fixtures/               real data from mainnet, devnet and a local Alpenglow validator
docs/                   how it works, running it, the code, reference, security
```

Each folder with its own `Cargo.toml` workspace (`zk/*`, `programs/`, `plugins/footer-dump/`) has a reason, given at the top of the root `Cargo.toml`.

## Docs

| If you want to | Read |
|---|---|
| understand the idea and the proofs | [`docs/how-it-works.md`](docs/how-it-works.md) |
| install, run, prove, clean up | [`docs/run.md`](docs/run.md) |
| find your way around the code | [`docs/codebase.md`](docs/codebase.md) |
| look up a byte layout, constant or pinned version | [`docs/reference.md`](docs/reference.md) |
| judge the risks | [`docs/security.md`](docs/security.md) |

The EVM side, with contracts and the guided app, lives in [`huginn-evm`](https://github.com/AlphaR2/huginn-evm).

## License

[Apache-2.0](LICENSE).

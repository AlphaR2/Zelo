# Running Huginn

1. [Tools](#1-tools): what to install, once.
2. [Local](#2-local): tests, the local Alpenglow validator, the full flow on a laptop.
3. [The Solana programs](#3-the-solana-programs): build, test, deploy.
4. [A Groth16 proof](#4-a-groth16-proof): on Succinct's prover network, or on a rented 32 GB machine.
5. [Cleanup](#5-cleanup): everything the setup puts on your machine.

---

## 1. Tools

| Tool | Why | Install |
|---|---|---|
| Rust | everything (the repo pins 1.99.1 in `rust-toolchain.toml`; rustup installs it on first use) | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| SP1 6.10.0 | the zkVM; `cargo prove --version` must say 6.10.0 | `curl -L https://sp1up.succinct.xyz \| bash`, then `sp1up --version v6.10.0` |
| Docker | builds the SP1 program in SP1's pinned image, so its program key is the same on every machine | Docker Desktop (Ubuntu: `apt install docker.io`, then add your user to the `docker` group) |
| protoc | SP1 builds | `brew install protobuf` (Ubuntu: `apt install protobuf-compiler`) |
| Solana CLI, Agave 4.4.1 | the local Alpenglow validator; `cargo build-sbf` for the programs | `sh -c "$(curl -sSfL https://release.anza.xyz/v4.4.1/install)"` |
| Foundry | contracts and anvil (huginn-evm) | `curl -L https://foundry.paradigm.xyz \| bash`, then `foundryup` |

Open a new terminal afterwards, so the new `PATH` entries apply.

`HUGINN_NATIVE_PROGRAM=1` builds the SP1 program without Docker, which is faster for development. That program key depends on your machine, so **never deploy it**.

---

## 2. Local

**Clone both repos next to each other.** huginn-evm's app finds huginn at `../huginn`:

```bash
git clone https://github.com/AlphaR2/huginn
```
```bash
git clone --recursive https://github.com/AlphaR2/huginn-evm
```

**Run the tests** in `huginn`. They use only committed fixtures (real blocks), with no validator or network:

```bash
cargo test --release
```

**Run the zkVM program** on the real local transaction, without making a proof. It prints the cycle count:

```bash
cd zk/prover && cargo run --release
```

A state claim works the same way, on the committed state fixture:

```bash
cargo run --release -- --state ../../fixtures/local/state_156070.json --depth 2 --confirm 1
```

**Start a local Alpenglow validator** for the full flow. Leave it running in its own terminal. Its ledger grows by a few GB a day:

```bash
solana-test-validator --alpenglow --ledger .local-ledger
```

Certificates are read from the blocks' own shreds, so the footer-dump plugin is not needed. Add `--geyser-plugin-config plugins/footer-dump/config.json` only to compare against it.

**The whole flow** (send a transaction, wait for finality, fetch its shreds, prove, verify on a local EVM chain) runs from huginn-evm's app. See huginn-evm's `README.md` and `docs/demo.md`.

### The data tools, one by one

The app runs these for you. Run them by hand to look at each step.

| Step | Command (from `huginn`) |
|---|---|
| The cluster's shred version | `cargo run -p fetch -- cluster-info --cluster local` |
| The epoch's validator set | `cargo run -p fetch -- validator-set --cluster local` |
| A checked block file from the local ledger | `cargo run --release -p ledger-dump -- --slot <S>` |
| A block's shreds from validators, no node (`SOLANA_RPC` picks the cluster) | `cargo run --release -p shred-repair -- fetch --slot <S> --out <dir>` |
| A checked block file from those shreds | `cargo run --release -p ledger-dump -- --shreds <dir> --slot <S>` |

A fresh validator is a new chain, with a new shred version and a new validator set. Fetch both again after `--reset`. The committed `fixtures/local` files belong to an older chain and are only used by the tests.

---

## 3. The Solana programs

The outbox and fill programs are in `programs/`, a workspace of their own, and are built for SBF (Solana's program format, version 3). The fill program trusts exactly one outbox, whose id is fixed at build time through `HUGINN_OUTBOX_ID`.

**Build and test.** For tests, any outbox id works. This one is 32 bytes of `0x0b`:

```bash
cd programs
export HUGINN_OUTBOX_ID=k7FaK87WHGVXzkaoHb7CdVPgkKDQhZ29VLDeBVbDfYn
cargo build-sbf
cargo test
```

`cargo test` runs the unit tests and the Mollusk tests. Mollusk runs the compiled `.so` files in Agave's own program runtime, with the real SPL Token program. The first build downloads the SBF compiler, platform-tools v1.57, about 400 MB.

**Deploy to the local validator.** Pass `-u localhost` every time, so nothing goes to another cluster.

1. Build and deploy the outbox. Note the printed `Program Id`:
   ```bash
   cargo build-sbf && solana program deploy target/deploy/huginn_outbox.so -u localhost
   ```
2. Rebuild with the real outbox id, then deploy the fill program:
   ```bash
   HUGINN_OUTBOX_ID=<outbox id> cargo build-sbf && solana program deploy target/deploy/huginn_fill.so -u localhost
   ```
3. Print the 16 shard accounts the escrow on Base needs:
   ```bash
   HUGINN_OUTBOX_ID=<outbox id> cargo run --bin shards -- <fill id>
   ```

`target/deploy/*-keypair.json` are the programs' address keys. They are gitignored; never share them. Once the programs are final, make them immutable with `solana program set-upgrade-authority <id> --final`.

---

## 4. A Groth16 proof

Groth16 is the small proof format EVM contracts check. Making one locally needs at least 32 GB of RAM.

### On Succinct's prover network (recommended)

huginn-evm's app does this with `--mode groth16` and the `network` feature. It reads the prover account key only from the `NETWORK_PRIVATE_KEY` environment variable, and never prints it. Proving costs PROVE tokens. On 14 Nov 2026 a devnet transaction took 53 seconds.

### On a rented machine

**Machine:** Ubuntu 24.04, at least 32 GB of RAM, 8 vCPUs and 100 GB of disk. Billing is hourly, and the job takes about an hour.

1. Install the system packages and Docker:
   ```bash
   sudo apt update && sudo apt install -y build-essential pkg-config libssl-dev protobuf-compiler clang git curl docker.io && sudo usermod -aG docker $USER && newgrp docker
   ```
2. Install Rust and SP1 v6.10.0:
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && source ~/.cargo/env && curl -L https://sp1up.succinct.xyz | bash && source ~/.bashrc && sp1up --version v6.10.0
   ```
3. Get the code, either by cloning huginn, or from your laptop:
   ```bash
   rsync -az --exclude target --exclude .local-ledger huginn/ root@<vps-ip>:huginn/
   ```
4. Prove. On first use SP1 downloads about 6 GB of circuit files:
   ```bash
   cd huginn/zk/prover && cargo run --release -- --prove groth16
   ```
   It ends with a line naming the proof file in `fixtures/proofs/`.
5. Copy the proof file back:
   ```bash
   scp root@<vps-ip>:huginn/fixtures/proofs/<file>.json huginn/fixtures/proofs/
   ```
6. Delete the machine.

---

## 5. Cleanup

**Stop the validator and anvil:**
```bash
pkill -f solana-test-validator; pkill anvil
```

**Build outputs** (about 10 GB for huginn):
```bash
cargo clean && (cd zk/prover && cargo clean) && (cd zk/program && cargo clean) && (cd programs && cargo clean)
```

**The local ledger** (several GB):
```bash
rm -rf .local-ledger fixtures/local/footers.jsonl
```

**Tools in your home folder:**

| What | Size | Remove with |
|---|---:|---|
| SP1 Groth16 circuit files | 14 GB | `rm -rf ~/.sp1/circuits` |
| SP1 entirely | 15 GB | `rm -rf ~/.sp1`, then delete its `PATH` line in your shell profile |
| Solana platform-tools | 1–2 GB | `rm -rf ~/.cache/solana` |
| Rust download caches | 3 GB | `rm -rf ~/.cargo/registry ~/.cargo/git` |
| Solana CLI | 0.6 GB | `rm -rf ~/.local/share/solana ~/.config/solana` (the keypair there only holds local test SOL) |
| Foundry | 0.3 GB | `rm -rf ~/.foundry` |

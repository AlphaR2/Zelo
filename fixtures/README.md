# Test fixtures

Real data the tests run against, recorded once, so every test is offline: no validator, no
network. Nothing here is made up. A fixture is either a block or a validator set recorded from a
chain, or a file derived from one by a tool in this repo (the command is in the file's own
folder README where there is one).

None of it holds a key or a secret: it is public chain data. Output from your own runs does not
belong here: `fixtures/proofs/*` and `fixtures/local/footers.jsonl` are gitignored.

| Folder | What it is | Where it came from |
|---|---|---|
| `local/` | A one-validator Alpenglow chain on this machine (shred version 50893 or 28090, depending on the file): blocks, a validator set, two state witnesses (`state_156070.json`, `state_2103.json`), the memo transaction `tx_46718.json`, and two raw shreds (`shreds/`) | `solana-test-validator --alpenglow` plus `ledger-dump`. See `local/shreds/README.md` |
| `devnet/` | Real Solana devnet: a block (`block_522347550.json`), the validator sets of epochs 1198 and 1202, two raw shreds, and Alpenglow's genesis certificate | `shred-repair` and `fetch` against public devnet. See `devnet/shreds/README.md` |
| `devnet-1206/` | Five real devnet blocks' shreds and the epoch-1206 validator set in two forms (`set-orig`, `set-flagged`), for the validator-set check (`ledger-dump check-set`) | the same tools, 2026-11 |
| `mainnet/` | Mainnet's Alpenglow genesis certificate | `getAgGenesisCert` on a public RPC |
| `tx_v1_agave.json` | A v1 transaction serialized by the `solana-transaction` 5.1.0 crate (the one Agave uses), so our parser is checked against its bytes. Its signature is a placeholder, not a real one | that crate; see the file's `note` |
| `proofs/` | Where proofs you make are written. Gitignored: it holds only a `.gitkeep` | your own runs |

## Which tests use what

- `crates/alpenglow-core/tests/`: `proof.rs`, `adversarial.rs`, `ancestry.rs`, `block_contents.rs`
  and `tx_signatures.rs` use `local/`. `state.rs` and `state_real_certificate.rs` use the state
  witnesses and `local/block_2105.json`.
- `crates/alpenglow-lc/tests/`: `core_matches_agave.rs`, `local_block.rs` and `local_footers.rs`
  compare our verifier with Agave's own crates on `local/`. `devnet_genesis.rs` checks the devnet
  and mainnet genesis certificates.
- `crates/ledger-dump/tests/`: `devnet_block.rs` rebuilds a devnet block from `devnet/shreds`;
  `check_set.rs` runs the validator-set check on `devnet-1206/`; `shred_files.rs` uses `local/shreds`.
- zelo-evm's Solidity tests read copies of `local/tx_46718.json` and `local/state_156070.json`
  (`test/fixtures/` there). If you regenerate one, copy it across.

## Changing a fixture

These files are the evidence behind the tests, so change one only to replace it with a newer
recording, and say in the commit which tool made it. A different file means different hashes,
so the tests that hard-code a value (a block id, a bank hash) must be updated with it.

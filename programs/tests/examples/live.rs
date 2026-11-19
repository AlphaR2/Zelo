//! Real transactions against the outbox and fill programs on a local validator.
//!
//!   ZELO_OUTBOX_ID=<outbox id> cargo run --example live -- <fill program id> check
//!   ZELO_OUTBOX_ID=<outbox id> cargo run --example live -- <fill program id> setup
//!   ZELO_OUTBOX_ID=<outbox id> cargo run --example live -- <fill program id> fill <order.json> <solver EVM address>
//!
//! - `check` fills an order, tries to fill it again, and fills two orders in one transaction (so
//!   one block), checking balances, the marker and the emitter account's hash chain.
//! - `setup` creates a test mint, a solver holding tokens and a merchant's token account, for an
//!   order placed on an EVM chain. It saves them in `target/live-setup.json` (the solver's key in
//!   `target/live-solver-keypair.json`, gitignored) and prints MINT and MERCHANT for the order.
//! - `fill` fills the order in `order.json` (written by zelo-evm's `Orders.s.sol place()`) for
//!   the given solver, and writes the slot, signature and message to `target/live-fill.json`.
//!
//! Every key is a fresh one funded by the local faucet (`requestAirdrop`); no wallet of yours is
//! used. Only an RPC on this machine is accepted (`LIVE_RPC`, default http://127.0.0.1:8899).

use {
    zelo_programs::{
        fill::{MARKER_SEED, Order, SHARD_SEED},
        outbox::{STATE_LEN, next_chain, state_address},
    },
    serde_json::{Value, json},
    solana_commitment_config::CommitmentConfig,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_program_pack::Pack,
    solana_pubkey::Pubkey,
    solana_rpc_client::{api::config::UiTransactionEncoding, rpc_client::RpcClient},
    solana_signer::Signer,
    solana_transaction::Transaction,
    spl_token_interface::{
        instruction::{initialize_account3, initialize_mint2, mint_to},
        state::{Account as TokenAccount, Mint},
    },
    std::{str::FromStr, time::Duration},
};

const DECIMALS: u8 = 6;
const SHARD: u8 = 3;
const TARGET: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../target");

fn pubkey(s: &str, what: &str) -> Pubkey {
    Pubkey::from_str(s).unwrap_or_else(|_| panic!("{what} is not a base58 address"))
}

fn hex_bytes(s: &str) -> Vec<u8> {
    let s = s.trim_start_matches("0x");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn check(ok: bool, what: &str) {
    println!("{} {what}", if ok { "PASS" } else { "FAIL" });
    if !ok {
        std::process::exit(1);
    }
}

/// The accounts an order's fill uses besides the programs.
struct Parties {
    solver: Keypair,
    mint: Pubkey,
    source: Pubkey,
    dest: Pubkey,
    merchant: Pubkey,
}

struct Live {
    rpc: RpcClient,
    fill: Pubkey,
    outbox: Pubkey,
}

impl Live {
    fn send(&self, ixs: &[Instruction], signers: &[&Keypair]) -> Result<(String, u64), String> {
        let blockhash = self.rpc.get_latest_blockhash().map_err(|e| e.to_string())?;
        let tx =
            Transaction::new_signed_with_payer(ixs, Some(&signers[0].pubkey()), signers, blockhash);
        let sig = self
            .rpc
            .send_and_confirm_transaction(&tx)
            .map_err(|e| format!("{e:?}"))?;
        let slot = self
            .rpc
            .get_transaction(&sig, UiTransactionEncoding::Json)
            .map_or(0, |t| t.slot);
        Ok((sig.to_string(), slot))
    }

    fn fund(&self, who: &Pubkey, lamports: u64) {
        let sig = self
            .rpc
            .request_airdrop(who, lamports)
            .expect("airdrop from the local faucet");
        for _ in 0..60 {
            if self.rpc.confirm_transaction(&sig).unwrap_or(false) {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        panic!("airdrop not confirmed");
    }

    fn token_balance(&self, account: &Pubkey) -> u64 {
        TokenAccount::unpack(&self.rpc.get_account_data(account).expect("token account"))
            .unwrap()
            .amount
    }

    /// `seq` and `chain` of an emitter account, or None if it doesn't exist yet.
    fn emitter(&self, state: &Pubkey) -> Option<(u64, [u8; 32])> {
        let a = self.rpc.get_account(state).ok()?;
        (a.owner == self.outbox && a.data.len() == STATE_LEN).then(|| {
            (
                u64::from_le_bytes(a.data[32..40].try_into().unwrap()),
                a.data[40..72].try_into().unwrap(),
            )
        })
    }

    fn shard_state(&self) -> (Pubkey, Pubkey, u8) {
        let shard = Pubkey::find_program_address(&[SHARD_SEED, &[SHARD]], &self.fill).0;
        let (state, bump) = state_address(&self.outbox, &shard);
        (shard, state, bump)
    }

    /// A funded solver holding 1,000 tokens of a new mint, and a merchant's token account.
    fn setup(&self) -> Parties {
        let solver = Keypair::new();
        let (mint, source, dest) = (Keypair::new(), Keypair::new(), Keypair::new());
        let merchant = Pubkey::new_unique();
        self.fund(&solver.pubkey(), 10_000_000_000);
        let token = spl_token_interface::ID;
        let create = |kp: &Keypair, len: usize| {
            let rent = self
                .rpc
                .get_minimum_balance_for_rent_exemption(len)
                .unwrap();
            solana_system_interface::instruction::create_account(
                &solver.pubkey(),
                &kp.pubkey(),
                rent,
                len as u64,
                &token,
            )
        };
        self.send(
            &[
                create(&mint, Mint::LEN),
                initialize_mint2(&token, &mint.pubkey(), &solver.pubkey(), None, DECIMALS).unwrap(),
                create(&source, TokenAccount::LEN),
                initialize_account3(&token, &source.pubkey(), &mint.pubkey(), &solver.pubkey())
                    .unwrap(),
                create(&dest, TokenAccount::LEN),
                initialize_account3(&token, &dest.pubkey(), &mint.pubkey(), &merchant).unwrap(),
                mint_to(
                    &token,
                    &mint.pubkey(),
                    &source.pubkey(),
                    &solver.pubkey(),
                    &[],
                    1_000_000_000,
                )
                .unwrap(),
            ],
            &[&solver, &mint, &source, &dest],
        )
        .expect("setup transaction");
        Parties {
            solver,
            mint: mint.pubkey(),
            source: source.pubkey(),
            dest: dest.pubkey(),
            merchant,
        }
    }

    fn fill_ix(&self, o: &Order, p: &Parties) -> Instruction {
        let id = o.id(&p.merchant.to_bytes(), &p.mint.to_bytes());
        let marker = Pubkey::find_program_address(&[MARKER_SEED, &id], &self.fill).0;
        let (shard, state, _) = self.shard_state();
        Instruction::new_with_bytes(
            self.fill,
            &o.encode(),
            vec![
                AccountMeta::new(p.solver.pubkey(), true),
                AccountMeta::new(p.source, false),
                AccountMeta::new(p.dest, false),
                AccountMeta::new_readonly(p.mint, false),
                AccountMeta::new_readonly(spl_token_interface::ID, false),
                AccountMeta::new(marker, false),
                AccountMeta::new_readonly(shard, false),
                AccountMeta::new(state, false),
                AccountMeta::new_readonly(self.outbox, false),
                AccountMeta::new_readonly(Pubkey::default(), false),
            ],
        )
    }

    fn payload(&self, o: &Order, p: &Parties) -> [u8; 124] {
        let (m, k) = (p.merchant.to_bytes(), p.mint.to_bytes());
        o.payload(&o.id(&m, &k), &m, &k)
    }
}

/// `check`: the programs' main cases, with fresh parties.
fn run_check(live: &Live) {
    let p = live.setup();
    println!("setup: mint {}, merchant {}", p.mint, p.merchant);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let (_, state, state_bump) = live.shard_state();
    let order = |nonce: u64| {
        let mut amount_in = [0u8; 32];
        amount_in[16..].copy_from_slice(&100_000_000u128.to_be_bytes());
        Order {
            chain_id: 84532,
            escrow: [0x11; 20],
            user: [0x22; 20],
            nonce,
            amount_out: 50_000_000,
            deadline: now + 3600,
            amount_in,
            solver_evm: [0x55; 20],
            shard: SHARD,
            decimals: DECIMALS,
            state_bump,
        }
    };
    let (seq0, chain0) = live.emitter(&state).unwrap_or((0, [0; 32]));

    let first = order(now);
    let (sig1, slot1) = live
        .send(&[live.fill_ix(&first, &p)], &[&p.solver])
        .expect("first fill");
    check(
        live.token_balance(&p.dest) == 50_000_000,
        "the merchant received 50 tokens",
    );
    let id = first.id(&p.merchant.to_bytes(), &p.mint.to_bytes());
    let marker = Pubkey::find_program_address(&[MARKER_SEED, &id], &live.fill).0;
    check(
        live.rpc
            .get_account(&marker)
            .is_ok_and(|a| a.owner == live.fill && a.data == [1]),
        "the order's marker exists",
    );
    let chain1 = next_chain(&chain0, seq0 + 1, &live.payload(&first, &p));
    check(
        live.emitter(&state) == Some((seq0 + 1, chain1)),
        "the shard's emitter account advanced by one, chain as expected",
    );

    let again = live.send(&[live.fill_ix(&first, &p)], &[&p.solver]);
    check(
        again.as_ref().is_err_and(|e| e.contains("Custom(8)")),
        "a second fill of the same order is refused (AlreadyFilled)",
    );
    check(
        live.token_balance(&p.dest) == 50_000_000,
        "and the merchant was not paid twice",
    );
    check(
        live.emitter(&state) == Some((seq0 + 1, chain1)),
        "and nothing was emitted",
    );

    let (a, b) = (order(now + 1), order(now + 2));
    let (sig2, slot2) = live
        .send(&[live.fill_ix(&a, &p), live.fill_ix(&b, &p)], &[&p.solver])
        .expect("two fills in one transaction");
    let chain3 = next_chain(
        &next_chain(&chain1, seq0 + 2, &live.payload(&a, &p)),
        seq0 + 3,
        &live.payload(&b, &p),
    );
    check(
        live.emitter(&state) == Some((seq0 + 3, chain3)),
        "two fills in one block both entered the chain",
    );
    check(
        live.token_balance(&p.dest) == 150_000_000,
        "the merchant received both payments",
    );

    let out = json!({
        "outbox": live.outbox.to_string(), "fill": live.fill.to_string(), "shard": SHARD,
        "emitter_account": state.to_string(), "mint": p.mint.to_string(), "merchant": p.merchant.to_string(),
        "first_fill": {"signature": sig1, "slot": slot1},
        "two_fills_one_block": {"signature": sig2, "slot": slot2},
    });
    std::fs::write(
        format!("{TARGET}/live-run.json"),
        serde_json::to_string_pretty(&out).unwrap(),
    )
    .unwrap();
    println!(
        "first fill in slot {slot1}, two fills in slot {slot2}; details in target/live-run.json"
    );
}

/// `setup`: parties for an order placed elsewhere, saved for `fill`.
fn run_setup(live: &Live) {
    let p = live.setup();
    let key: Vec<u8> = p.solver.to_bytes().to_vec();
    std::fs::write(
        format!("{TARGET}/live-solver-keypair.json"),
        serde_json::to_string(&key).unwrap(),
    )
    .unwrap();
    let out = json!({
        "mint": p.mint.to_string(), "source": p.source.to_string(), "dest": p.dest.to_string(),
        "merchant": p.merchant.to_string(),
    });
    std::fs::write(
        format!("{TARGET}/live-setup.json"),
        serde_json::to_string_pretty(&out).unwrap(),
    )
    .unwrap();
    println!("saved in target/live-setup.json; for the order:");
    println!("MINT=0x{}", hex(&p.mint.to_bytes()));
    println!("MERCHANT=0x{}", hex(&p.merchant.to_bytes()));
}

/// `fill`: fills the order in `order.json` for `solver_evm`, with the parties from `setup`.
fn run_fill(live: &Live, order_file: &str, solver_evm: &str) {
    let s: Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{TARGET}/live-setup.json")).expect("run `setup` first"),
    )
    .unwrap();
    let key: Vec<u8> = serde_json::from_str(
        &std::fs::read_to_string(format!("{TARGET}/live-solver-keypair.json")).unwrap(),
    )
    .unwrap();
    let p = Parties {
        solver: Keypair::try_from(key.as_slice()).expect("solver key"),
        mint: pubkey(s["mint"].as_str().unwrap(), "mint"),
        source: pubkey(s["source"].as_str().unwrap(), "source"),
        dest: pubkey(s["dest"].as_str().unwrap(), "dest"),
        merchant: pubkey(s["merchant"].as_str().unwrap(), "merchant"),
    };
    let o: Value =
        serde_json::from_str(&std::fs::read_to_string(order_file).expect("order file")).unwrap();
    let field = |k: &str| -> Vec<u8> {
        hex_bytes(o[k].as_str().unwrap_or_else(|| panic!("order.json: {k}")))
    };
    assert_eq!(
        field("mint"),
        p.mint.to_bytes(),
        "the order's mint is not the one from setup"
    );
    assert_eq!(
        field("merchant"),
        p.merchant.to_bytes(),
        "the order's merchant is not the one from setup"
    );
    let mut amount_in = [0u8; 32];
    amount_in[16..]
        .copy_from_slice(&(o["amount_in"].as_u64().expect("amount_in") as u128).to_be_bytes());
    let (_, state, state_bump) = live.shard_state();
    let order = Order {
        chain_id: o["chain_id"].as_u64().unwrap(),
        escrow: field("escrow").try_into().unwrap(),
        user: field("user").try_into().unwrap(),
        nonce: o["nonce"].as_u64().unwrap(),
        amount_out: o["amount_out"].as_u64().unwrap(),
        deadline: o["deadline"].as_u64().unwrap(),
        amount_in,
        solver_evm: hex_bytes(solver_evm)
            .try_into()
            .expect("a 20-byte EVM address"),
        shard: SHARD,
        decimals: DECIMALS,
        state_bump,
    };
    let id = order.id(&p.merchant.to_bytes(), &p.mint.to_bytes());
    check(
        hex(&id) == hex(&field("order_id")),
        "the order id computed here equals the escrow's",
    );
    let before = live.token_balance(&p.dest);
    let (sig, slot) = live
        .send(&[live.fill_ix(&order, &p)], &[&p.solver])
        .expect("fill");
    check(
        live.token_balance(&p.dest) == before + order.amount_out,
        "the merchant was paid",
    );
    let out = json!({
        "signature": sig, "slot": slot, "emitter": format!("0x{}", hex(&state.to_bytes())),
        "payload": format!("0x{}", hex(&live.payload(&order, &p))), "order_id": format!("0x{}", hex(&id)),
    });
    std::fs::write(
        format!("{TARGET}/live-fill.json"),
        serde_json::to_string_pretty(&out).unwrap(),
    )
    .unwrap();
    println!("filled in slot {slot}; details in target/live-fill.json");
}

fn main() {
    let outbox = pubkey(
        &std::env::var("ZELO_OUTBOX_ID").expect("set ZELO_OUTBOX_ID"),
        "ZELO_OUTBOX_ID",
    );
    let args: Vec<String> = std::env::args().collect();
    let usage =
        "usage: live <fill program id> check | setup | fill <order.json> <solver EVM address>";
    let fill = pubkey(args.get(1).expect(usage), "the fill id");
    let url = std::env::var("LIVE_RPC").unwrap_or_else(|_| "http://127.0.0.1:8899".into());
    assert!(
        url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:"),
        "live only runs against a validator on this machine"
    );
    let live = Live {
        rpc: RpcClient::new_with_commitment(url, CommitmentConfig::confirmed()),
        fill,
        outbox,
    };
    match args.get(2).map(String::as_str) {
        Some("check") => run_check(&live),
        Some("setup") => run_setup(&live),
        Some("fill") => run_fill(&live, args.get(3).expect(usage), args.get(4).expect(usage)),
        _ => panic!("{usage}"),
    }
}

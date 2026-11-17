//! The compiled outbox and fill programs, run in Agave 4.4's runtime (Mollusk) with the real
//! SPL Token program, the system program and the clock.
//!
//! Build first, with the same outbox id for the build and the tests:
//!
//!   HUGINN_OUTBOX_ID=<id> cargo build-sbf && HUGINN_OUTBOX_ID=<id> cargo test

use {
    huginn_programs::{
        fill::{FillError, MARKER_SEED, Order, SHARD_SEED},
        outbox::{OutboxError, STATE_LEN, next_chain, state_address},
    },
    mollusk_svm::{
        Mollusk,
        program::{
            create_program_account_loader_v3, keyed_account_for_system_program, loader_keys,
        },
        result::{InstructionResult, ProgramResult},
    },
    mollusk_svm_programs_token::token,
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_program_error::ProgramError,
    solana_program_option::COption,
    solana_pubkey::Pubkey,
    spl_token_interface::state::{Account as TokenAccount, AccountState, Mint},
};

const OUTBOX: Pubkey = Pubkey::from_str_const(env!("HUGINN_OUTBOX_ID"));
const FILL: Pubkey = Pubkey::new_from_array([0xF1; 32]);
const NOW: i64 = 1_800_000_000;
const DECIMALS: u8 = 6;
const SOL: u64 = 1_000_000_000;

fn elf(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/deploy/{name}.so", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path)
        .unwrap_or_else(|_| panic!("{path} is missing: run `cargo build-sbf` first"))
}

fn mollusk() -> Mollusk {
    let mut m = Mollusk::default();
    m.add_program_with_loader_and_elf(&OUTBOX, &loader_keys::LOADER_V3, &elf("huginn_outbox"));
    m.add_program_with_loader_and_elf(&FILL, &loader_keys::LOADER_V3, &elf("huginn_fill"));
    token::add_program(&mut m);
    m.sysvars.clock.unix_timestamp = NOW;
    m
}

fn custom(code: u32) -> ProgramResult {
    ProgramResult::Failure(ProgramError::Custom(code))
}

/// A system-owned account holding only lamports.
fn wallet(lamports: u64) -> Account {
    Account {
        lamports,
        ..Account::default()
    }
}

fn account<'a>(r: &'a InstructionResult, key: &Pubkey) -> &'a Account {
    r.get_account(key).expect("account in the result")
}

/// `seq` and `chain` of an emitter account.
fn emitter_state(data: &[u8]) -> (u64, [u8; 32]) {
    assert_eq!(data.len(), STATE_LEN);
    (
        u64::from_le_bytes(data[32..40].try_into().unwrap()),
        data[40..72].try_into().unwrap(),
    )
}

// ---------------------------------------------------------------------------------------------
// Outbox

fn emit(emitter: Pubkey, state: Pubkey, payer: Pubkey, bump: u8, payload: &[u8]) -> Instruction {
    Instruction::new_with_bytes(
        OUTBOX,
        &[&[bump], payload].concat(),
        vec![
            AccountMeta::new_readonly(emitter, true),
            AccountMeta::new(state, false),
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(solana_pubkey::Pubkey::default(), false),
        ],
    )
}

fn outbox_accounts(emitter: Pubkey, state: (Pubkey, Account)) -> Vec<(Pubkey, Account)> {
    vec![
        (emitter, wallet(SOL)),
        state,
        keyed_account_for_system_program(),
    ]
}

#[test]
fn a_wallet_emits_two_messages_and_the_chain_grows() {
    let m = mollusk();
    let emitter = Pubkey::new_from_array([7; 32]);
    let (state, bump) = state_address(&OUTBOX, &emitter);
    let ixs = [
        emit(emitter, state, emitter, bump, b"first"),
        emit(emitter, state, emitter, bump, b"second"),
    ];
    let r =
        m.process_instruction_chain(&ixs, &outbox_accounts(emitter, (state, Account::default())));
    assert_eq!(r.program_result, ProgramResult::Success);
    let acct = account(&r, &state);
    assert_eq!(acct.owner, OUTBOX);
    assert_eq!(&acct.data[..32], emitter.as_ref());
    let (seq, chain) = emitter_state(&acct.data);
    assert_eq!(seq, 2);
    assert_eq!(
        chain,
        next_chain(&next_chain(&[0; 32], 1, b"first"), 2, b"second")
    );
}

#[test]
fn the_outbox_refuses_a_non_canonical_address_or_a_missing_signature() {
    let m = mollusk();
    let emitter = Pubkey::new_from_array([7; 32]);
    let (state, bump) = state_address(&OUTBOX, &emitter);
    let (odd, odd_bump) = (0..bump)
        .rev()
        .find_map(|b| {
            Pubkey::create_program_address(&[b"emitter", emitter.as_ref(), &[b]], &OUTBOX)
                .ok()
                .map(|a| (a, b))
        })
        .unwrap();
    let r = m.process_instruction(
        &emit(emitter, odd, emitter, odd_bump, b"x"),
        &outbox_accounts(emitter, (odd, Account::default())),
    );
    assert_eq!(r.program_result, custom(OutboxError::WrongAddress as u32));

    // A separate payer signs; the emitter does not.
    let payer = Pubkey::new_from_array([8; 32]);
    let mut unsigned = emit(emitter, state, payer, bump, b"x");
    unsigned.accounts[0].is_signer = false;
    let mut accounts = outbox_accounts(emitter, (state, Account::default()));
    accounts.push((payer, wallet(SOL)));
    let r = m.process_instruction(&unsigned, &accounts);
    assert_eq!(
        r.program_result,
        ProgramResult::Failure(ProgramError::MissingRequiredSignature)
    );
}

#[test]
fn a_pre_funded_emitter_address_does_not_block_the_first_message() {
    let m = mollusk();
    let emitter = Pubkey::new_from_array([7; 32]);
    let (state, bump) = state_address(&OUTBOX, &emitter);
    let r = m.process_instruction(
        &emit(emitter, state, emitter, bump, b"x"),
        &outbox_accounts(emitter, (state, wallet(1))),
    );
    assert_eq!(r.program_result, ProgramResult::Success);
    assert_eq!(emitter_state(&account(&r, &state).data).0, 1);
}

// ---------------------------------------------------------------------------------------------
// Fill

const SOLVER: Pubkey = Pubkey::new_from_array([0x51; 32]);
const SOURCE: Pubkey = Pubkey::new_from_array([0x52; 32]);
const MERCHANT: Pubkey = Pubkey::new_from_array([0x33; 32]);
const DEST: Pubkey = Pubkey::new_from_array([0x34; 32]);
const MINT: Pubkey = Pubkey::new_from_array([0x44; 32]);
const OTHER_MINT: Pubkey = Pubkey::new_from_array([0x45; 32]);

fn order(nonce: u64, shard: u8) -> Order {
    let mut amount_in = [0u8; 32];
    amount_in[16..].copy_from_slice(&100_000_000u128.to_be_bytes());
    let shard_address = Pubkey::find_program_address(&[SHARD_SEED, &[shard]], &FILL).0;
    Order {
        chain_id: 84532,
        escrow: [0x11; 20],
        user: [0x22; 20],
        nonce,
        amount_out: 50_000_000,
        deadline: NOW as u64 + 3600,
        amount_in,
        solver_evm: [0x55; 20],
        shard,
        decimals: DECIMALS,
        state_bump: state_address(&OUTBOX, &shard_address).1,
    }
}

fn mint_account(decimals: u8) -> Account {
    token::create_account_for_mint(Mint {
        mint_authority: COption::None,
        supply: 1_000_000_000_000,
        decimals,
        is_initialized: true,
        freeze_authority: COption::None,
    })
}

fn token_account(mint: Pubkey, owner: Pubkey, amount: u64) -> Account {
    token::create_account_for_token_account(TokenAccount {
        mint,
        owner,
        amount,
        delegate: COption::None,
        state: AccountState::Initialized,
        is_native: COption::None,
        delegated_amount: 0,
        close_authority: COption::None,
    })
}

/// The accounts a fill reads, as named in the program's docs.
struct Fill {
    order: Order,
    marker: Pubkey,
    shard: Pubkey,
    state: Pubkey,
}

impl Fill {
    fn new(order: Order) -> Fill {
        let id = order.id(&MERCHANT.to_bytes(), &MINT.to_bytes());
        let marker = Pubkey::find_program_address(&[MARKER_SEED, &id], &FILL).0;
        let shard = Pubkey::find_program_address(&[SHARD_SEED, &[order.shard]], &FILL).0;
        let state = state_address(&OUTBOX, &shard).0;
        Fill {
            order,
            marker,
            shard,
            state,
        }
    }

    fn id(&self) -> [u8; 32] {
        self.order.id(&MERCHANT.to_bytes(), &MINT.to_bytes())
    }

    fn payload(&self) -> [u8; 124] {
        self.order
            .payload(&self.id(), &MERCHANT.to_bytes(), &MINT.to_bytes())
    }

    fn instruction(&self) -> Instruction {
        Instruction::new_with_bytes(
            FILL,
            &self.order.encode(),
            vec![
                AccountMeta::new(SOLVER, true),
                AccountMeta::new(SOURCE, false),
                AccountMeta::new(DEST, false),
                AccountMeta::new_readonly(MINT, false),
                AccountMeta::new_readonly(token::ID, false),
                AccountMeta::new(self.marker, false),
                AccountMeta::new_readonly(self.shard, false),
                AccountMeta::new(self.state, false),
                AccountMeta::new_readonly(OUTBOX, false),
                AccountMeta::new_readonly(solana_pubkey::Pubkey::default(), false),
            ],
        )
    }

    /// Every account the instruction lists, in a fresh state: the solver holds `balance` tokens.
    fn accounts(&self, balance: u64) -> Vec<(Pubkey, Account)> {
        vec![
            (SOLVER, wallet(10 * SOL)),
            (SOURCE, token_account(MINT, SOLVER, balance)),
            (DEST, token_account(MINT, MERCHANT, 0)),
            (MINT, mint_account(DECIMALS)),
            token::keyed_account(),
            (self.marker, Account::default()),
            (self.shard, Account::default()),
            (self.state, Account::default()),
            (OUTBOX, create_program_account_loader_v3(&OUTBOX)),
            keyed_account_for_system_program(),
        ]
    }
}

fn token_amount(r: &InstructionResult, key: &Pubkey) -> u64 {
    u64::from_le_bytes(account(r, key).data[64..72].try_into().unwrap())
}

fn set(accounts: &mut [(Pubkey, Account)], key: Pubkey, value: Account) {
    accounts.iter_mut().find(|(k, _)| *k == key).unwrap().1 = value;
}

#[test]
fn a_fill_pays_the_merchant_marks_the_order_and_leaves_the_message() {
    let m = mollusk();
    let f = Fill::new(order(1, 3));
    let r = m.process_instruction(&f.instruction(), &f.accounts(80_000_000));
    assert_eq!(r.program_result, ProgramResult::Success);
    assert_eq!(token_amount(&r, &DEST), 50_000_000);
    assert_eq!(token_amount(&r, &SOURCE), 30_000_000);
    let marker = account(&r, &f.marker);
    assert_eq!((marker.owner, marker.data.as_slice()), (FILL, &[1u8][..]));
    let state = account(&r, &f.state);
    assert_eq!(state.owner, OUTBOX);
    assert_eq!(
        &state.data[..32],
        f.shard.as_ref(),
        "the emitter is the shard PDA"
    );
    assert_eq!(
        emitter_state(&state.data),
        (1, next_chain(&[0; 32], 1, &f.payload()))
    );
    println!("fill: {} compute units", r.compute_units_consumed);
}

#[test]
fn an_order_is_filled_once() {
    let m = mollusk();
    let f = Fill::new(order(1, 3));
    let r = m.process_instruction_chain(
        &[f.instruction(), f.instruction()],
        &f.accounts(200_000_000),
    );
    assert_eq!(r.program_result, custom(FillError::AlreadyFilled as u32));
}

#[test]
fn a_marker_at_another_bump_is_refused() {
    // The attack: a second "filled" marker for the same order at a non-canonical address.
    let m = mollusk();
    let mut f = Fill::new(order(1, 3));
    let id = f.id();
    let (_, bump) = Pubkey::find_program_address(&[MARKER_SEED, &id], &FILL);
    f.marker = (0..bump)
        .rev()
        .find_map(|b| Pubkey::create_program_address(&[MARKER_SEED, &id, &[b]], &FILL).ok())
        .unwrap();
    let r = m.process_instruction(&f.instruction(), &f.accounts(80_000_000));
    assert_eq!(r.program_result, custom(FillError::WrongMarker as u32));
}

#[test]
fn without_enough_tokens_nothing_happens() {
    let m = mollusk();
    let f = Fill::new(order(1, 3));
    let accounts = f.accounts(49_999_999);
    let r = m.process_instruction(&f.instruction(), &accounts);
    assert!(matches!(r.program_result, ProgramResult::Failure(_)));
    // A failed instruction leaves every account as it was: no marker, no message.
    for (key, before) in &accounts {
        assert_eq!(account(&r, key), before);
    }
}

#[test]
fn wrong_accounts_and_data_are_refused() {
    let m = mollusk();
    let f = Fill::new(order(1, 3));
    let run = |accounts: &[(Pubkey, Account)], ix: &Instruction| {
        m.process_instruction(ix, accounts).program_result
    };

    let mut accounts = f.accounts(80_000_000);
    set(&mut accounts, DEST, token_account(OTHER_MINT, MERCHANT, 0));
    assert_eq!(
        run(&accounts, &f.instruction()),
        custom(FillError::BadDestination as u32),
        "destination of another mint"
    );

    let mut accounts = f.accounts(80_000_000);
    set(&mut accounts, MINT, mint_account(9));
    assert_eq!(
        run(&accounts, &f.instruction()),
        custom(FillError::BadMint as u32),
        "decimals differ"
    );

    let mut late = order(1, 3);
    late.deadline = NOW as u64 - 1;
    let late = Fill::new(late);
    assert_eq!(
        run(&late.accounts(80_000_000), &late.instruction()),
        custom(FillError::PastDeadline as u32)
    );

    let mut ix = f.instruction();
    ix.data[124] = 16;
    assert_eq!(
        run(&f.accounts(80_000_000), &ix),
        custom(FillError::ShardOutOfRange as u32)
    );

    let other = Fill::new(order(1, 4));
    let mut ix = f.instruction();
    ix.accounts[6].pubkey = other.shard;
    let mut accounts = f.accounts(80_000_000);
    accounts[6].0 = other.shard;
    assert_eq!(
        run(&accounts, &ix),
        custom(FillError::WrongShard as u32),
        "shard 4's PDA for shard 3"
    );

    let mut ix = f.instruction();
    ix.accounts[8].pubkey = FILL;
    let mut accounts = f.accounts(80_000_000);
    accounts[8] = (FILL, create_program_account_loader_v3(&FILL));
    assert_eq!(run(&accounts, &ix), custom(FillError::NotTheOutbox as u32));

    let mut ix = f.instruction();
    ix.accounts[0].is_signer = false;
    assert_eq!(
        run(&f.accounts(80_000_000), &ix),
        ProgramResult::Failure(ProgramError::MissingRequiredSignature)
    );
}

#[test]
fn pre_funded_marker_and_emitter_addresses_do_not_block_a_fill() {
    let m = mollusk();
    let f = Fill::new(order(1, 3));
    let mut accounts = f.accounts(80_000_000);
    set(&mut accounts, f.marker, wallet(1));
    set(&mut accounts, f.state, wallet(1));
    let r = m.process_instruction(&f.instruction(), &accounts);
    assert_eq!(r.program_result, ProgramResult::Success);
}

#[test]
fn two_fills_on_one_shard_both_enter_the_chain() {
    let m = mollusk();
    let (a, b) = (Fill::new(order(1, 3)), Fill::new(order(2, 3)));
    let mut accounts = a.accounts(200_000_000);
    accounts.push((b.marker, Account::default()));
    let r = m.process_instruction_chain(&[a.instruction(), b.instruction()], &accounts);
    assert_eq!(r.program_result, ProgramResult::Success);
    let expected = next_chain(&next_chain(&[0; 32], 1, &a.payload()), 2, &b.payload());
    assert_eq!(emitter_state(&account(&r, &a.state).data), (2, expected));
    assert_eq!(token_amount(&r, &DEST), 100_000_000);
}

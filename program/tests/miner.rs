//! LiteSVM integration tests.
//! Require a built program: `cargo build-sbf`.

use litesvm::LiteSVM;
use miner_api::{consts::*, pda, sdk, state::*};
use solana_sdk::{
    account::Account,
    clock::Clock,
    hash::Hash,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    slot_hashes::SlotHashes,
    transaction::Transaction,
};

const SO_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../target/deploy/miner_program.so"
);

/// Round budget at the default (launch) cadence.
const DEFAULT_BUDGET: u64 = round_budget(INITIAL_ROUND_SECONDS);

// ---------- helpers ----------

fn setup() -> (LiteSVM, Keypair, Pubkey) {
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(miner_api::id(), SO_PATH)
        .expect("run `cargo build-sbf` first");

    // SlotHashes: the program reads the raw sysvar data as entropy.
    svm.set_sysvar::<SlotHashes>(&SlotHashes::new(&[(1, Hash::new_unique())]));

    let admin = Keypair::new();
    svm.airdrop(&admin.pubkey(), 10_000_000_000).unwrap();

    // Mint created "outside the program": decimals 9, authority = treasury PDA.
    let mint = Pubkey::new_unique();
    let (treasury, _) = pda::treasury_pda();
    svm.set_account(mint, mint_account(&treasury, 0)).unwrap();

    send(
        &mut svm,
        &[sdk::initialize(admin.pubkey(), mint)],
        &admin,
        &[],
    )
    .expect("initialize");

    // Production difficulty (20 bits) is ~1M hashes; that would take ages
    // on the host in debug mode, so tests grind at difficulty 8.
    send(
        &mut svm,
        &[sdk::update_config(
            admin.pubkey(),
            8,
            INITIAL_BASE_WEIGHT,
            INITIAL_ROUND_SECONDS,
        )],
        &admin,
        &[],
    )
    .expect("update_config (lower the difficulty for tests)");

    (svm, admin, mint)
}

/// Raw SPL mint account data (82 bytes).
fn mint_account(authority: &Pubkey, supply: u64) -> Account {
    let mut data = vec![0u8; 82];
    data[0..4].copy_from_slice(&[1, 0, 0, 0]); // COption::Some
    data[4..36].copy_from_slice(authority.as_ref());
    data[36..44].copy_from_slice(&supply.to_le_bytes());
    data[44] = TOKEN_DECIMALS;
    data[45] = 1; // is_initialized
    Account {
        lamports: 10_000_000,
        data,
        owner: SPL_TOKEN_PROGRAM_ID,
        executable: false,
        rent_epoch: 0,
    }
}

/// Raw SPL token account data (165 bytes).
fn token_account(mint: &Pubkey, owner: &Pubkey, amount: u64) -> Account {
    let mut data = vec![0u8; 165];
    data[0..32].copy_from_slice(mint.as_ref());
    data[32..64].copy_from_slice(owner.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1; // state = Initialized
    Account {
        lamports: 10_000_000,
        data,
        owner: SPL_TOKEN_PROGRAM_ID,
        executable: false,
        rent_epoch: 0,
    }
}

fn set_balance(svm: &mut LiteSVM, mint: &Pubkey, owner: &Pubkey, amount: u64) {
    let ata = pda::ata(owner, mint);
    svm.set_account(ata, token_account(mint, owner, amount))
        .unwrap();
}

fn send(
    svm: &mut LiteSVM,
    ixs: &[solana_sdk::instruction::Instruction],
    payer: &Keypair,
    extra_signers: &[&Keypair],
) -> Result<(), String> {
    let mut signers: Vec<&Keypair> = vec![payer];
    signers.extend_from_slice(extra_signers);
    let tx = Transaction::new_signed_with_payer(
        ixs,
        Some(&payer.pubkey()),
        &signers,
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx)
        .map(|_| ())
        .map_err(|e| format!("{:?}", e.err))
}

fn get_state<T: bytemuck::Pod>(svm: &LiteSVM, key: &Pubkey) -> T {
    let acc = svm.get_account(key).expect("account does not exist");
    *bytemuck::from_bytes::<T>(&acc.data)
}

fn get_config(svm: &LiteSVM) -> Config {
    get_state(svm, &pda::config_pda().0)
}

fn get_miner(svm: &LiteSVM, authority: &Pubkey) -> Miner {
    get_state(svm, &pda::miner_pda(authority).0)
}

fn get_round(svm: &LiteSVM, index: u64) -> Round {
    get_state(svm, &pda::round_pda(index).0)
}

fn token_balance(svm: &LiteSVM, mint: &Pubkey, owner: &Pubkey) -> u64 {
    let acc = svm.get_account(&pda::ata(owner, mint)).unwrap();
    u64::from_le_bytes(acc.data[64..72].try_into().unwrap())
}

/// Grinds a nonce that meets the difficulty (on the host; difficulty is low).
fn grind(svm: &LiteSVM, authority: &Pubkey) -> u64 {
    let config = get_config(svm);
    let miner = get_miner(svm, authority);
    let mut nonce = 0u64;
    loop {
        if sdk::hash_meets_difficulty(&miner.challenge, authority, nonce, config.min_difficulty)
        {
            return nonce;
        }
        nonce += 1;
    }
}

fn register_miner(svm: &mut LiteSVM, authority: &Keypair) {
    svm.airdrop(&authority.pubkey(), 1_000_000_000).unwrap();
    send(svm, &[sdk::register(authority.pubkey())], authority, &[]).expect("register");
}

/// Mine signed by the authority.
fn mine(svm: &mut LiteSVM, authority: &Keypair, mint: &Pubkey) -> Result<(), String> {
    let config = get_config(svm);
    let miner = get_miner(svm, &authority.pubkey());
    let nonce = grind(svm, &authority.pubkey());
    send(
        svm,
        &[sdk::mine(
            authority.pubkey(),
            authority.pubkey(),
            *mint,
            config.current_round,
            miner.last_round,
            nonce,
        )],
        authority,
        &[],
    )
}

/// Advances the clock and rolls the round over with the crank.
fn advance_round(svm: &mut LiteSVM, payer: &Keypair) {
    let round_seconds = get_config(svm).round_seconds as i64;
    let mut clock: Clock = svm.get_sysvar();
    clock.unix_timestamp += round_seconds + 1;
    svm.set_sysvar(&clock);
    // Refresh the blockhash so transactions are not deduplicated.
    svm.expire_blockhash();
    let config = get_config(svm);
    send(
        svm,
        &[sdk::crank(payer.pubkey(), config.current_round + 1)],
        payer,
        &[],
    )
    .expect("crank");
}

// ---------- tests ----------

#[test]
fn test_initialize() {
    let (svm, admin, mint) = setup();
    let config = get_config(&svm);
    assert_eq!(config.admin, admin.pubkey().to_bytes());
    assert_eq!(config.mint, mint.to_bytes());
    assert_eq!(config.current_round, 0);
    assert_eq!(config.base_weight, INITIAL_BASE_WEIGHT);
    let round0 = get_round(&svm, 0);
    assert_eq!(round0.index, 0);
    assert_eq!(round0.total_weight, 0);
}

#[test]
fn test_free_tier_full_flow() {
    let (mut svm, admin, mint) = setup();
    let miner_kp = Keypair::new();
    register_miner(&mut svm, &miner_kp);

    // Free tier: no token account -> weight = base_weight.
    mine(&mut svm, &miner_kp, &mint).expect("mine r0");
    let round0 = get_round(&svm, 0);
    assert_eq!(round0.total_weight, INITIAL_BASE_WEIGHT);

    // New round; the next submit settles the previous one (100% of the
    // budget, mining solo).
    advance_round(&mut svm, &admin);
    mine(&mut svm, &miner_kp, &mint).expect("mine r1");
    let m = get_miner(&svm, &miner_kp.pubkey());
    assert_eq!(m.pending_rewards, DEFAULT_BUDGET);

    // Claim: mints to the ATA.
    set_balance(&mut svm, &mint, &miner_kp.pubkey(), 0);
    send(
        &mut svm,
        &[sdk::claim(miner_kp.pubkey(), mint, m.last_round)],
        &miner_kp,
        &[],
    )
    .expect("claim");
    assert_eq!(token_balance(&svm, &mint, &miner_kp.pubkey()), DEFAULT_BUDGET);
    let m = get_miner(&svm, &miner_kp.pubkey());
    assert_eq!(m.pending_rewards, 0);
    assert_eq!(m.total_mined, DEFAULT_BUDGET);
}

#[test]
fn test_flash_balance_does_not_count() {
    let (mut svm, _admin, mint) = setup();
    let miner_kp = Keypair::new();
    register_miner(&mut svm, &miner_kp);

    // Tokens bought BEFORE the first submit: min(balance, 0) = 0.
    set_balance(&mut svm, &mint, &miner_kp.pubkey(), 1_000 * ONE_TOKEN);
    mine(&mut svm, &miner_kp, &mint).expect("mine r0");
    assert_eq!(get_round(&svm, 0).total_weight, INITIAL_BASE_WEIGHT);
}

#[test]
fn test_balance_counts_from_second_round() {
    let (mut svm, admin, mint) = setup();
    let miner_kp = Keypair::new();
    register_miner(&mut svm, &miner_kp);
    set_balance(&mut svm, &mint, &miner_kp.pubkey(), 1_000 * ONE_TOKEN);

    mine(&mut svm, &miner_kp, &mint).expect("mine r0");
    advance_round(&mut svm, &admin);
    mine(&mut svm, &miner_kp, &mint).expect("mine r1");

    // Second round: min(1000, 1000) = 1000 tokens of weight + the base.
    assert_eq!(
        get_round(&svm, 1).total_weight,
        INITIAL_BASE_WEIGHT + 1_000 * ONE_TOKEN
    );
}

#[test]
fn test_cycling_attack_blocked() {
    let (mut svm, admin, mint) = setup();
    let alice = Keypair::new();
    let bob = Keypair::new();
    register_miner(&mut svm, &alice);
    register_miner(&mut svm, &bob);

    // Alice holds 1000 tokens, Bob 0. Round 0: both submit.
    set_balance(&mut svm, &mint, &alice.pubkey(), 1_000 * ONE_TOKEN);
    mine(&mut svm, &alice, &mint).expect("alice r0");
    mine(&mut svm, &bob, &mint).expect("bob r0");

    advance_round(&mut svm, &admin);

    // Round 1: Alice submits with her balance (weight = base + 1000)...
    mine(&mut svm, &alice, &mint).expect("alice r1");
    // ...then transfers everything to Bob, who submits as well.
    set_balance(&mut svm, &mint, &alice.pubkey(), 0);
    set_balance(&mut svm, &mint, &bob.pubkey(), 1_000 * ONE_TOKEN);
    mine(&mut svm, &bob, &mint).expect("bob r1");

    // Bob: min(1000, previously 0) = 0 -> the same tokens do NOT count twice.
    assert_eq!(
        get_round(&svm, 1).total_weight,
        2 * INITIAL_BASE_WEIGHT + 1_000 * ONE_TOKEN
    );
}

#[test]
fn test_double_submit_rejected() {
    let (mut svm, _admin, mint) = setup();
    let miner_kp = Keypair::new();
    register_miner(&mut svm, &miner_kp);

    mine(&mut svm, &miner_kp, &mint).expect("first submit");
    svm.expire_blockhash();
    let err = mine(&mut svm, &miner_kp, &mint).expect_err("duplicate must fail");
    assert!(err.contains("Custom(5)"), "expected AlreadySubmitted: {err}");
}

#[test]
fn test_session_key_can_mine_but_not_claim() {
    let (mut svm, admin, mint) = setup();
    let owner = Keypair::new();
    let session = Keypair::new();
    register_miner(&mut svm, &owner);
    svm.airdrop(&session.pubkey(), 1_000_000_000).unwrap();

    send(
        &mut svm,
        &[sdk::authorize_session(owner.pubkey(), session.pubkey())],
        &owner,
        &[],
    )
    .expect("authorize_session");

    // Mine signed with the session key (payer = session).
    let config = get_config(&svm);
    let m = get_miner(&svm, &owner.pubkey());
    let nonce = grind(&svm, &owner.pubkey());
    send(
        &mut svm,
        &[sdk::mine(
            session.pubkey(),
            owner.pubkey(),
            mint,
            config.current_round,
            m.last_round,
            nonce,
        )],
        &session,
        &[],
    )
    .expect("mine via the session key");

    advance_round(&mut svm, &admin);

    // A claim signed with the session key must be rejected.
    set_balance(&mut svm, &mint, &owner.pubkey(), 0);
    let mut claim_ix = sdk::claim(owner.pubkey(), mint, 0);
    claim_ix.accounts[0].pubkey = session.pubkey();
    let err = send(&mut svm, &[claim_ix], &session, &[]).expect_err("session must not claim");
    assert!(err.contains("Custom(3)"), "expected Unauthorized: {err}");
}

#[test]
fn test_unregistered_signer_rejected() {
    let (mut svm, _admin, mint) = setup();
    let owner = Keypair::new();
    let attacker = Keypair::new();
    register_miner(&mut svm, &owner);
    svm.airdrop(&attacker.pubkey(), 1_000_000_000).unwrap();

    let config = get_config(&svm);
    let nonce = grind(&svm, &owner.pubkey());
    let err = send(
        &mut svm,
        &[sdk::mine(
            attacker.pubkey(),
            owner.pubkey(),
            mint,
            config.current_round,
            0,
            nonce,
        )],
        &attacker,
        &[],
    )
    .expect_err("a foreign signature must be rejected");
    assert!(err.contains("Custom(3)"), "expected Unauthorized: {err}");
}

#[test]
fn test_bad_nonce_rejected() {
    let (mut svm, _admin, mint) = setup();
    let miner_kp = Keypair::new();
    register_miner(&mut svm, &miner_kp);

    let config = get_config(&svm);
    // Find a nonce that does NOT meet the difficulty.
    let m = get_miner(&svm, &miner_kp.pubkey());
    let mut bad_nonce = 0u64;
    while sdk::hash_meets_difficulty(
        &m.challenge,
        &miner_kp.pubkey(),
        bad_nonce,
        config.min_difficulty,
    ) {
        bad_nonce += 1;
    }
    let err = send(
        &mut svm,
        &[sdk::mine(
            miner_kp.pubkey(),
            miner_kp.pubkey(),
            mint,
            config.current_round,
            0,
            bad_nonce,
        )],
        &miner_kp,
        &[],
    )
    .expect_err("a bad hash must be rejected");
    assert!(err.contains("Custom(0)"), "expected InvalidHash: {err}");
}

#[test]
fn test_two_miners_split_budget_by_weight() {
    let (mut svm, admin, mint) = setup();
    let alice = Keypair::new();
    let bob = Keypair::new();
    register_miner(&mut svm, &alice);
    register_miner(&mut svm, &bob);

    // Set balances from round 0 so they fully count from round 1.
    // Alice: 3x the base in tokens, Bob: 0 (free tier).
    let alice_tokens = 3 * INITIAL_BASE_WEIGHT;
    set_balance(&mut svm, &mint, &alice.pubkey(), alice_tokens);
    mine(&mut svm, &alice, &mint).expect("alice r0");
    mine(&mut svm, &bob, &mint).expect("bob r0");

    advance_round(&mut svm, &admin);
    mine(&mut svm, &alice, &mint).expect("alice r1");
    mine(&mut svm, &bob, &mint).expect("bob r1");

    advance_round(&mut svm, &admin);
    // The third submit settles round 1.
    mine(&mut svm, &alice, &mint).expect("alice r2");
    mine(&mut svm, &bob, &mint).expect("bob r2");

    let a = get_miner(&svm, &alice.pubkey());
    let b = get_miner(&svm, &bob.pubkey());

    // Round 0: both weights = base -> 50% of the budget each.
    // Round 1: Alice weighs 4x base, Bob 1x base -> 80% / 20%.
    let expected_alice = DEFAULT_BUDGET / 2 + DEFAULT_BUDGET * 4 / 5;
    let expected_bob = DEFAULT_BUDGET / 2 + DEFAULT_BUDGET / 5;
    assert_eq!(a.pending_rewards, expected_alice);
    assert_eq!(b.pending_rewards, expected_bob);
}

#[test]
fn test_round_seconds_change_applies_to_next_round() {
    let (mut svm, admin, mint) = setup();
    let miner_kp = Keypair::new();
    register_miner(&mut svm, &miner_kp);

    // The miner submits in round 0 (default budget, 60 s).
    mine(&mut svm, &miner_kp, &mint).expect("mine r0");

    // The admin shortens rounds to 15 s (difficulty stays at the test 8).
    send(
        &mut svm,
        &[sdk::update_config(admin.pubkey(), 8, INITIAL_BASE_WEIGHT, 15)],
        &admin,
        &[],
    )
    .expect("update_config");
    assert_eq!(get_config(&svm).round_seconds, 15);

    // The crank opens round 1 with the new, smaller budget.
    advance_round(&mut svm, &admin);
    assert_eq!(get_round(&svm, 1).budget, round_budget(15));

    // Round 0 settles with its frozen budget (60 s), not the new one.
    mine(&mut svm, &miner_kp, &mint).expect("mine r1");
    let m = get_miner(&svm, &miner_kp.pubkey());
    assert_eq!(m.pending_rewards, DEFAULT_BUDGET);
}

#[test]
fn test_empty_round_emission_lapses() {
    let (mut svm, admin, mint) = setup();
    let miner_kp = Keypair::new();
    register_miner(&mut svm, &miner_kp);

    // Nobody mines in rounds 0 and 1.
    advance_round(&mut svm, &admin);
    advance_round(&mut svm, &admin);

    // The miner only mines in round 2, so nothing from the empty rounds.
    mine(&mut svm, &miner_kp, &mint).expect("mine r2");
    let m = get_miner(&svm, &miner_kp.pubkey());
    assert_eq!(m.pending_rewards, 0);
    assert_eq!(m.last_round, 2);
}

#[test]
fn test_set_admin_rotates_authority() {
    let (mut svm, admin, _mint) = setup();
    let new_admin = Keypair::new();
    svm.airdrop(&new_admin.pubkey(), 1_000_000_000).unwrap();

    send(
        &mut svm,
        &[sdk::set_admin(admin.pubkey(), new_admin.pubkey())],
        &admin,
        &[],
    )
    .expect("set_admin");
    assert_eq!(get_config(&svm).admin, new_admin.pubkey().to_bytes());

    // The old admin loses access, both to the config and further rotations.
    // (difficulty 10, not 8: a tx identical to setup() would be deduplicated)
    let err = send(
        &mut svm,
        &[sdk::update_config(
            admin.pubkey(),
            10,
            INITIAL_BASE_WEIGHT,
            INITIAL_ROUND_SECONDS,
        )],
        &admin,
        &[],
    )
    .expect_err("the old admin must be rejected");
    assert!(err.contains("Custom(3)"), "expected Unauthorized: {err}");

    let err = send(
        &mut svm,
        &[sdk::set_admin(admin.pubkey(), admin.pubkey())],
        &admin,
        &[],
    )
    .expect_err("the old admin must not reclaim the role");
    assert!(err.contains("Custom(3)"), "expected Unauthorized: {err}");

    // The new admin manages parameters normally.
    send(
        &mut svm,
        &[sdk::update_config(
            new_admin.pubkey(),
            9,
            INITIAL_BASE_WEIGHT,
            INITIAL_ROUND_SECONDS,
        )],
        &new_admin,
        &[],
    )
    .expect("update_config as the new admin");
    assert_eq!(get_config(&svm).min_difficulty, 9);
}

#[test]
fn test_set_admin_rejects_non_admin_and_zero_key() {
    let (mut svm, admin, _mint) = setup();
    let attacker = Keypair::new();
    svm.airdrop(&attacker.pubkey(), 1_000_000_000).unwrap();

    let err = send(
        &mut svm,
        &[sdk::set_admin(attacker.pubkey(), attacker.pubkey())],
        &attacker,
        &[],
    )
    .expect_err("a non-admin must be rejected");
    assert!(err.contains("Custom(3)"), "expected Unauthorized: {err}");

    // The all-zero address is rejected; protects against accidentally
    // "burning" the role.
    let err = send(
        &mut svm,
        &[sdk::set_admin(admin.pubkey(), Pubkey::default())],
        &admin,
        &[],
    )
    .expect_err("the zero key must be rejected");
    assert!(err.contains("InvalidInstructionData"), "{err}");
}

#[test]
fn test_update_config_difficulty_cap() {
    let (mut svm, admin, _mint) = setup();

    // 33 bits exceeds the cap (the admin could freeze mining): rejected.
    let err = send(
        &mut svm,
        &[sdk::update_config(
            admin.pubkey(),
            33,
            INITIAL_BASE_WEIGHT,
            INITIAL_ROUND_SECONDS,
        )],
        &admin,
        &[],
    )
    .expect_err("difficulty 33 must be rejected");
    assert!(err.contains("InvalidInstructionData"), "{err}");

    // 32 bits is the maximum: passes.
    send(
        &mut svm,
        &[sdk::update_config(
            admin.pubkey(),
            32,
            INITIAL_BASE_WEIGHT,
            INITIAL_ROUND_SECONDS,
        )],
        &admin,
        &[],
    )
    .expect("difficulty 32 within the cap");
    assert_eq!(get_config(&svm).min_difficulty, 32);
}

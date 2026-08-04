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

fn get_referral(svm: &LiteSVM, authority: &Pubkey) -> Referral {
    get_state(svm, &pda::referral_pda(authority).0)
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

/// Mine for a miner enrolled in the referral program (trailing referral
/// account), signed by the authority.
fn mine_ref(svm: &mut LiteSVM, authority: &Keypair, mint: &Pubkey) -> Result<(), String> {
    let config = get_config(svm);
    let miner = get_miner(svm, &authority.pubkey());
    let nonce = grind(svm, &authority.pubkey());
    send(
        svm,
        &[sdk::mine_with_referral(
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

/// Warps the chain clock to an absolute timestamp.
fn warp_to(svm: &mut LiteSVM, ts: i64) {
    let mut clock: Clock = svm.get_sysvar();
    clock.unix_timestamp = ts;
    svm.set_sysvar(&clock);
    svm.expire_blockhash();
}

/// Rolls the round over with the crank at the current clock.
fn crank_next(svm: &mut LiteSVM, payer: &Keypair) {
    let config = get_config(svm);
    send(
        svm,
        &[sdk::crank(payer.pubkey(), config.current_round + 1)],
        payer,
        &[],
    )
    .expect("crank");
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
fn test_crank_opens_round_despite_prefunded_pda() {
    let (mut svm, admin, _mint) = setup();

    // Griefing attack: round PDAs are predictable, so an attacker sends
    // lamports to the next round's address before the crank creates it
    // (890880 = rent minimum of an empty account, below the rent-exempt
    // minimum for Round::SIZE, so the crank must top it up).
    let (round1_key, _) = pda::round_pda(1);
    svm.airdrop(&round1_key, 890_880).unwrap();

    // The crank must open the round anyway.
    advance_round(&mut svm, &admin);
    assert_eq!(get_round(&svm, 1).index, 1);
    let acc = svm.get_account(&round1_key).unwrap();
    assert_eq!(acc.owner, miner_api::id());
    assert_eq!(acc.data.len(), Round::SIZE);

    // Variant with a balance above the rent-exempt minimum: no top-up
    // needed, the lamports stay on the account and the crank still works.
    let (round2_key, _) = pda::round_pda(2);
    svm.airdrop(&round2_key, 10_000_000).unwrap();
    advance_round(&mut svm, &admin);
    assert_eq!(get_round(&svm, 2).index, 2);
    let acc = svm.get_account(&round2_key).unwrap();
    assert_eq!(acc.owner, miner_api::id());
    assert_eq!(acc.lamports, 10_000_000);
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

/// Halving schedule: the round budget halves every HALVING_SECONDS from
/// HALVING_ANCHOR_TS, is frozen per round, and pins to zero in the deep
/// future (guarded shift; a wrap would "revive" the emission).
#[test]
fn test_halving_schedule() {
    let (mut svm, admin, mint) = setup();

    // Before the anchor: the full base budget.
    warp_to(&mut svm, HALVING_ANCHOR_TS - 1_000);
    crank_next(&mut svm, &admin);
    let cfg = get_config(&svm);
    assert_eq!(get_round(&svm, cfg.current_round).budget, DEFAULT_BUDGET);

    // Epoch 0 (after the anchor, before the first halving): still full.
    warp_to(&mut svm, HALVING_ANCHOR_TS + 1_000);
    crank_next(&mut svm, &admin);
    let cfg = get_config(&svm);
    assert_eq!(get_round(&svm, cfg.current_round).budget, DEFAULT_BUDGET);

    // Epoch 1: half. Epoch 5: 1/32.
    warp_to(&mut svm, HALVING_ANCHOR_TS + HALVING_SECONDS + 1_000);
    crank_next(&mut svm, &admin);
    let cfg = get_config(&svm);
    assert_eq!(get_round(&svm, cfg.current_round).budget, DEFAULT_BUDGET / 2);

    warp_to(&mut svm, HALVING_ANCHOR_TS + 5 * HALVING_SECONDS + 1_000);
    crank_next(&mut svm, &admin);
    let cfg = get_config(&svm);
    let halved = DEFAULT_BUDGET / 32;
    assert_eq!(get_round(&svm, cfg.current_round).budget, halved);

    // A miner in a halved round settles the halved budget (solo -> 100%).
    let kp = Keypair::new();
    register_miner(&mut svm, &kp);
    mine(&mut svm, &kp, &mint).expect("mine in a halved round");
    advance_round(&mut svm, &admin);
    mine(&mut svm, &kp, &mint).expect("mine settles the previous round");
    assert_eq!(get_miner(&svm, &kp.pubkey()).pending_rewards, halved);

    // Deep future: the budget reaches zero naturally (~34 halvings) and
    // STAYS zero past epoch 64, where an unguarded u64 shift would wrap.
    warp_to(&mut svm, HALVING_ANCHOR_TS + 40 * HALVING_SECONDS);
    crank_next(&mut svm, &admin);
    let cfg = get_config(&svm);
    assert_eq!(get_round(&svm, cfg.current_round).budget, 0);

    warp_to(&mut svm, HALVING_ANCHOR_TS + 70 * HALVING_SECONDS);
    crank_next(&mut svm, &admin);
    let cfg = get_config(&svm);
    assert_eq!(get_round(&svm, cfg.current_round).budget, 0);
}

#[test]
fn test_referral_full_flow() {
    let (mut svm, admin, mint) = setup();
    let referrer = Keypair::new();
    let referee = Keypair::new();
    register_miner(&mut svm, &referrer);
    register_miner(&mut svm, &referee);

    // Enrollment: Referral PDA created, Miner flagged.
    send(
        &mut svm,
        &[sdk::set_referrer(referee.pubkey(), referrer.pubkey())],
        &referee,
        &[],
    )
    .expect("set_referrer");
    let r = get_referral(&svm, &referee.pubkey());
    assert_eq!(r.authority, referee.pubkey().to_bytes());
    assert_eq!(r.referrer, referrer.pubkey().to_bytes());
    let m = get_miner(&svm, &referee.pubkey());
    assert!(m.bump & MINER_FLAG_REFERRAL != 0);

    // Round 0: min-balance rule -> the fresh balance does not count yet, so
    // the boost has nothing to amplify (weight = base only).
    let tokens = 1_000 * ONE_TOKEN;
    set_balance(&mut svm, &mint, &referee.pubkey(), tokens);
    mine_ref(&mut svm, &referee, &mint).expect("mine r0");
    assert_eq!(get_round(&svm, 0).total_weight, INITIAL_BASE_WEIGHT);

    // Round 1: the token slice counts and is boosted by the referral bonus.
    let boosted = tokens * (BPS_DENOM + REFERRAL_BONUS_BPS) / BPS_DENOM;
    advance_round(&mut svm, &admin);
    mine_ref(&mut svm, &referee, &mint).expect("mine r1");
    assert_eq!(
        get_round(&svm, 1).total_weight,
        INITIAL_BASE_WEIGHT + boosted
    );

    // Round 2 submit settles round 1: sole miner -> the whole budget, the
    // token-slice reward charged the full ladder total into the pool.
    advance_round(&mut svm, &admin);
    mine_ref(&mut svm, &referee, &mint).expect("mine r2");
    let weight = INITIAL_BASE_WEIGHT + boosted;
    let token_reward =
        (DEFAULT_BUDGET as u128 * boosted as u128 / weight as u128) as u64;
    let pool = token_reward * REFERRAL_TOTAL_BPS / BPS_DENOM;
    // Round 0 settled earlier for the full budget (base weight only, no
    // commission), round 1 for budget - pool.
    let m = get_miner(&svm, &referee.pubkey());
    assert_eq!(m.pending_rewards, DEFAULT_BUDGET + DEFAULT_BUDGET - pool);
    let r = get_referral(&svm, &referee.pubkey());
    assert_eq!(r.pending_commission, pool);
    assert!(pool > 0);

    // Referee claims with a 1-level chain: the referrer takes the level-1
    // share, the shares of the missing levels 2 and 3 BURN (the referrer's
    // empty Referral PDA in the tx proves the chain ends). The carve is
    // flat: the referee gives up the full pool no matter the chain depth.
    let l1_share =
        (pool as u128 * REFERRAL_LEVEL_BPS[0] as u128 / REFERRAL_TOTAL_BPS as u128) as u64;
    let burned = pool - l1_share;
    let m = get_miner(&svm, &referee.pubkey());
    send(
        &mut svm,
        &[sdk::claim_with_referral(
            referee.pubkey(),
            mint,
            m.last_round,
            &[referrer.pubkey()],
        )],
        &referee,
        &[],
    )
    .expect("claim with referral");
    assert_eq!(
        token_balance(&svm, &mint, &referee.pubkey()),
        tokens + 2 * DEFAULT_BUDGET - pool
    );
    let r = get_referral(&svm, &referee.pubkey());
    assert_eq!(r.pending_commission, 0);
    assert_eq!(r.total_commission, l1_share);
    assert_eq!(r.total_burned, burned);
    assert!(burned > 0);
    let referrer_miner = get_miner(&svm, &referrer.pubkey());
    assert_eq!(referrer_miner.pending_rewards, l1_share);

    // The referrer claims the commission with a plain (legacy) claim.
    set_balance(&mut svm, &mint, &referrer.pubkey(), 0);
    send(
        &mut svm,
        &[sdk::claim(referrer.pubkey(), mint, 0)],
        &referrer,
        &[],
    )
    .expect("referrer claim");
    assert_eq!(token_balance(&svm, &mint, &referrer.pubkey()), l1_share);

    // Conservation: referee net + referrer commission + burn == 2 budgets,
    // i.e. the burn is exactly the emission that never got minted.
    assert_eq!(
        (2 * DEFAULT_BUDGET - pool) + l1_share + burned,
        2 * DEFAULT_BUDGET
    );
}

/// Full 3-level ladder: X is referred by A, A by B, B by C. X's commission
/// pool splits 5/3/1 across A, B and C; only the rounding dust burns.
#[test]
fn test_referral_ladder_three_levels() {
    let (mut svm, admin, mint) = setup();
    let x = Keypair::new();
    let a = Keypair::new();
    let b = Keypair::new();
    let c = Keypair::new();
    for kp in [&x, &a, &b, &c] {
        register_miner(&mut svm, kp);
    }
    send(&mut svm, &[sdk::set_referrer(x.pubkey(), a.pubkey())], &x, &[])
        .expect("x -> a");
    send(&mut svm, &[sdk::set_referrer(a.pubkey(), b.pubkey())], &a, &[])
        .expect("a -> b");
    send(&mut svm, &[sdk::set_referrer(b.pubkey(), c.pubkey())], &b, &[])
        .expect("b -> c");

    let tokens = 1_000 * ONE_TOKEN;
    set_balance(&mut svm, &mint, &x.pubkey(), tokens);
    mine_ref(&mut svm, &x, &mint).expect("mine r0");
    advance_round(&mut svm, &admin);
    mine_ref(&mut svm, &x, &mint).expect("mine r1");
    advance_round(&mut svm, &admin);
    mine_ref(&mut svm, &x, &mint).expect("mine r2");

    let r = get_referral(&svm, &x.pubkey());
    let pool = r.pending_commission;
    assert!(pool > 0);
    let share = |level: usize| {
        (pool as u128 * REFERRAL_LEVEL_BPS[level] as u128 / REFERRAL_TOTAL_BPS as u128) as u64
    };

    // The claim must carry the full chain: a 1-level claim is rejected
    // because A's live Referral PDA proves the chain continues.
    let m = get_miner(&svm, &x.pubkey());
    let err = send(
        &mut svm,
        &[sdk::claim_with_referral(
            x.pubkey(),
            mint,
            m.last_round,
            &[a.pubkey()],
        )],
        &x,
        &[],
    )
    .expect_err("a shortened chain must fail");
    assert!(err.contains("Custom(1)"), "expected InvalidAccount: {err}");

    // So is a 2-level claim (B is enrolled too).
    svm.expire_blockhash();
    let err = send(
        &mut svm,
        &[sdk::claim_with_referral(
            x.pubkey(),
            mint,
            m.last_round,
            &[a.pubkey(), b.pubkey()],
        )],
        &x,
        &[],
    )
    .expect_err("a shortened chain must fail");
    assert!(err.contains("Custom(1)"), "expected InvalidAccount: {err}");

    // The full chain distributes 5/3/1.
    send(
        &mut svm,
        &[sdk::claim_with_referral(
            x.pubkey(),
            mint,
            m.last_round,
            &[a.pubkey(), b.pubkey(), c.pubkey()],
        )],
        &x,
        &[],
    )
    .expect("claim with the full chain");
    assert_eq!(get_miner(&svm, &a.pubkey()).pending_rewards, share(0));
    assert_eq!(get_miner(&svm, &b.pubkey()).pending_rewards, share(1));
    assert_eq!(get_miner(&svm, &c.pubkey()).pending_rewards, share(2));
    let distributed = share(0) + share(1) + share(2);
    let r = get_referral(&svm, &x.pubkey());
    assert_eq!(r.pending_commission, 0);
    assert_eq!(r.total_commission, distributed);
    // With a full chain only the rounding dust burns.
    assert_eq!(r.total_burned, pool - distributed);
    // X pays the flat carve: the full pool, same as any other depth.
    assert_eq!(
        token_balance(&svm, &mint, &x.pubkey()),
        tokens + 2 * DEFAULT_BUDGET - pool
    );
}

/// A mutual cycle (X <-> A) must not break the chain walk: X's level-2
/// slot points back at X (that share BURNS, no double write) and level 3
/// is A again (paid twice, sequentially).
#[test]
fn test_referral_cycle_claim() {
    let (mut svm, admin, mint) = setup();
    let x = Keypair::new();
    let a = Keypair::new();
    register_miner(&mut svm, &x);
    register_miner(&mut svm, &a);
    send(&mut svm, &[sdk::set_referrer(x.pubkey(), a.pubkey())], &x, &[])
        .expect("x -> a");
    send(&mut svm, &[sdk::set_referrer(a.pubkey(), x.pubkey())], &a, &[])
        .expect("a -> x");

    let tokens = 1_000 * ONE_TOKEN;
    set_balance(&mut svm, &mint, &x.pubkey(), tokens);
    mine_ref(&mut svm, &x, &mint).expect("mine r0");
    advance_round(&mut svm, &admin);
    mine_ref(&mut svm, &x, &mint).expect("mine r1");
    advance_round(&mut svm, &admin);
    mine_ref(&mut svm, &x, &mint).expect("mine r2");

    let pool = get_referral(&svm, &x.pubkey()).pending_commission;
    assert!(pool > 0);
    let share = |level: usize| {
        (pool as u128 * REFERRAL_LEVEL_BPS[level] as u128 / REFERRAL_TOTAL_BPS as u128) as u64
    };

    // Chain resolved from on-chain state: A, then X (cycle), then A again.
    let m = get_miner(&svm, &x.pubkey());
    send(
        &mut svm,
        &[sdk::claim_with_referral(
            x.pubkey(),
            mint,
            m.last_round,
            &[a.pubkey(), x.pubkey(), a.pubkey()],
        )],
        &x,
        &[],
    )
    .expect("claim through the cycle");
    // A collects levels 1 and 3; X's own level-2 share burns (a cycle back
    // to yourself pays the flat carve like everyone else, no discount).
    let distributed = share(0) + share(2);
    assert_eq!(get_miner(&svm, &a.pubkey()).pending_rewards, distributed);
    assert_eq!(
        token_balance(&svm, &mint, &x.pubkey()),
        tokens + 2 * DEFAULT_BUDGET - pool
    );
    let r = get_referral(&svm, &x.pubkey());
    assert_eq!(r.total_commission, distributed);
    assert_eq!(r.total_burned, pool - distributed);
    assert!(r.total_burned >= share(1));
}

#[test]
fn test_set_referrer_guards() {
    let (mut svm, _admin, _mint) = setup();
    let alice = Keypair::new();
    let bob = Keypair::new();
    register_miner(&mut svm, &alice);
    register_miner(&mut svm, &bob);

    // Self-referral rejected.
    let err = send(
        &mut svm,
        &[sdk::set_referrer(alice.pubkey(), alice.pubkey())],
        &alice,
        &[],
    )
    .expect_err("self-referral must fail");
    assert!(err.contains("Custom(13)"), "expected SelfReferral: {err}");

    // An unregistered referrer rejected.
    let err = send(
        &mut svm,
        &[sdk::set_referrer(alice.pubkey(), Pubkey::new_unique())],
        &alice,
        &[],
    )
    .expect_err("an unregistered referrer must fail");
    assert!(err.contains("Custom(1)"), "expected InvalidAccount: {err}");

    // Enrollment is immutable: the second set_referrer fails on the
    // existing Referral account (even towards the same referrer).
    send(
        &mut svm,
        &[sdk::set_referrer(alice.pubkey(), bob.pubkey())],
        &alice,
        &[],
    )
    .expect("first enrollment");
    svm.expire_blockhash();
    let err = send(
        &mut svm,
        &[sdk::set_referrer(alice.pubkey(), bob.pubkey())],
        &alice,
        &[],
    )
    .expect_err("re-enrollment must fail");
    assert!(
        err.contains("AccountAlreadyInitialized"),
        "expected AccountAlreadyInitialized: {err}"
    );

    // Mutual referral (alice <-> bob) is allowed at enrollment: the claim
    // chain walk treats a cycle back to the claimer as a plain refund.
    send(
        &mut svm,
        &[sdk::set_referrer(bob.pubkey(), alice.pubkey())],
        &bob,
        &[],
    )
    .expect("mutual referral");
}

#[test]
fn test_enrolled_miner_must_pass_referral_accounts() {
    let (mut svm, admin, mint) = setup();
    let referrer = Keypair::new();
    let referee = Keypair::new();
    register_miner(&mut svm, &referrer);
    register_miner(&mut svm, &referee);
    send(
        &mut svm,
        &[sdk::set_referrer(referee.pubkey(), referrer.pubkey())],
        &referee,
        &[],
    )
    .expect("set_referrer");

    // A legacy 7-account mine must be rejected (commission bookkeeping
    // would be skipped otherwise).
    let err = mine(&mut svm, &referee, &mint).expect_err("legacy mine must fail");
    assert!(
        err.contains("Custom(14)"),
        "expected ReferralAccountRequired: {err}"
    );
    mine_ref(&mut svm, &referee, &mint).expect("mine with the referral account");

    // A legacy 8-account claim must be rejected as well.
    advance_round(&mut svm, &admin);
    mine_ref(&mut svm, &referee, &mint).expect("mine r1");
    let m = get_miner(&svm, &referee.pubkey());
    set_balance(&mut svm, &mint, &referee.pubkey(), 0);
    let err = send(
        &mut svm,
        &[sdk::claim(referee.pubkey(), mint, m.last_round)],
        &referee,
        &[],
    )
    .expect_err("legacy claim must fail");
    assert!(
        err.contains("Custom(14)"),
        "expected ReferralAccountRequired: {err}"
    );
}

#[test]
fn test_referral_zero_balance_is_neutral() {
    let (mut svm, admin, mint) = setup();
    let referrer = Keypair::new();
    let referee = Keypair::new();
    register_miner(&mut svm, &referrer);
    register_miner(&mut svm, &referee);
    send(
        &mut svm,
        &[sdk::set_referrer(referee.pubkey(), referrer.pubkey())],
        &referee,
        &[],
    )
    .expect("set_referrer");

    // No tokens -> the boost amplifies nothing and no commission accrues:
    // a farm of empty referred wallets earns the referrer exactly zero.
    mine_ref(&mut svm, &referee, &mint).expect("mine r0");
    assert_eq!(get_round(&svm, 0).total_weight, INITIAL_BASE_WEIGHT);
    advance_round(&mut svm, &admin);
    mine_ref(&mut svm, &referee, &mint).expect("mine r1");
    let m = get_miner(&svm, &referee.pubkey());
    assert_eq!(m.pending_rewards, DEFAULT_BUDGET);
    let r = get_referral(&svm, &referee.pubkey());
    assert_eq!(r.pending_commission, 0);

    // A non-enrolled miner may keep sending the trailing (empty) referral
    // account: it is ignored, so clients can always append it.
    let plain = Keypair::new();
    register_miner(&mut svm, &plain);
    mine_ref(&mut svm, &plain, &mint).expect("non-enrolled mine with a trailing account");
}

#[test]
fn test_refname_claim_unique_and_immutable() {
    let (mut svm, _admin, _mint) = setup();
    let alice = Keypair::new();
    let bob = Keypair::new();
    register_miner(&mut svm, &alice);
    register_miner(&mut svm, &bob);

    // Alice claims "mrminer": both directions of the mapping get written.
    send(
        &mut svm,
        &[sdk::set_refname(alice.pubkey(), "mrminer")],
        &alice,
        &[],
    )
    .expect("set_refname");
    let by_name: RefName = get_state(&svm, &pda::refname_pda(b"mrminer").0);
    assert_eq!(by_name.owner, alice.pubkey().to_bytes());
    assert_eq!(by_name.name_str(), "mrminer");
    let by_owner: RefName = get_state(&svm, &pda::refname_owner_pda(&alice.pubkey()).0);
    assert_eq!(by_owner.name_str(), "mrminer");

    // First come first served: Bob cannot take the same name.
    let err = send(
        &mut svm,
        &[sdk::set_refname(bob.pubkey(), "mrminer")],
        &bob,
        &[],
    )
    .expect_err("a taken name must fail");
    assert!(err.contains("AccountAlreadyInitialized"), "{err}");

    // One name per miner: Alice cannot claim a second one.
    let err = send(
        &mut svm,
        &[sdk::set_refname(alice.pubkey(), "othername")],
        &alice,
        &[],
    )
    .expect_err("a second name must fail");
    assert!(err.contains("AccountAlreadyInitialized"), "{err}");

    // A different free name still works for Bob.
    send(
        &mut svm,
        &[sdk::set_refname(bob.pubkey(), "bob_42")],
        &bob,
        &[],
    )
    .expect("bob claims a free name");
}

#[test]
fn test_refname_validation() {
    let (mut svm, _admin, _mint) = setup();
    let alice = Keypair::new();
    register_miner(&mut svm, &alice);

    // Too short, too long, uppercase, a dash: all rejected.
    for bad in ["ab", "seventeen_chars_x", "MrMiner", "mr-miner"] {
        let err = send(
            &mut svm,
            &[sdk::set_refname(alice.pubkey(), bad)],
            &alice,
            &[],
        )
        .expect_err("an invalid name must fail");
        assert!(err.contains("Custom(15)"), "expected InvalidName for {bad}: {err}");
        svm.expire_blockhash();
    }

    // An unregistered wallet cannot claim a name.
    let nobody = Keypair::new();
    svm.airdrop(&nobody.pubkey(), 1_000_000_000).unwrap();
    let err = send(
        &mut svm,
        &[sdk::set_refname(nobody.pubkey(), "ghost")],
        &nobody,
        &[],
    )
    .expect_err("an unregistered wallet must fail");
    assert!(err.contains("Custom(1)"), "expected InvalidAccount: {err}");

    // The boundary lengths pass.
    send(&mut svm, &[sdk::set_refname(alice.pubkey(), "abc")], &alice, &[])
        .expect("a 3-char name works");
    let bob = Keypair::new();
    register_miner(&mut svm, &bob);
    send(
        &mut svm,
        &[sdk::set_refname(bob.pubkey(), "sixteen_chars_ok")],
        &bob,
        &[],
    )
    .expect("a 16-char name works");
}

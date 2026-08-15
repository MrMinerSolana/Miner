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

/// The miners' cut of the round budget at the default (launch) cadence:
/// what Round.budget stores (the Motherlode share is withheld on top).
const DEFAULT_BUDGET: u64 = miners_budget(round_budget(INITIAL_ROUND_SECONDS));

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

    // Motherlode singleton (fee-paying Mine requires it).
    send(&mut svm, &[sdk::init_motherlode(admin.pubkey())], &admin, &[])
        .expect("init_motherlode");

    // The fee wallet must stay rent-exempt when the 5000-lamport fee lands
    // (on mainnet it is the funded ops wallet).
    svm.airdrop(&FEE_WALLET, 1_000_000_000).unwrap();

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

fn get_motherlode(svm: &LiteSVM) -> Motherlode {
    get_state(svm, &pda::motherlode_pda().0)
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

/// Rolls the round over with the crank at the current clock. The crank
/// takes the current Motherlode candidates' Win PDAs, exactly like the bot.
fn crank_next(svm: &mut LiteSVM, payer: &Keypair) {
    let config = get_config(svm);
    let candidates = get_motherlode(svm).candidates.map(Pubkey::new_from_array);
    send(
        svm,
        &[sdk::crank(payer.pubkey(), config.current_round + 1, candidates)],
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
    crank_next(svm, payer);
}

/// Grinds a SlotHashes entropy such that the NEXT crank's Motherlode draw
/// hits (or misses) deterministically, reproducing the on-chain roll
/// host-side. Randomness in the program comes from this sysvar, so the
/// test fully controls the outcome even at mainnet odds.
fn rig_draw(svm: &mut LiteSVM, hit: bool) {
    let config = get_config(svm);
    let ml = get_motherlode(svm);
    let slot: u64 = 1;
    loop {
        let hash = Hash::new_unique();
        // slot_hashes_entropy = vec len skipped, then slot u64 + hash.
        let mut entropy = [0u8; 40];
        entropy[..8].copy_from_slice(&slot.to_le_bytes());
        entropy[8..].copy_from_slice(hash.as_ref());
        let draw = solana_sdk::keccak::hashv(&[
            entropy.as_slice(),
            &config.current_round.to_le_bytes(),
            &ml.hashes.to_le_bytes(),
        ]);
        let roll = u64::from_le_bytes(draw.as_ref()[..8].try_into().unwrap());
        if (roll % MOTHERLODE_ODDS == 0) == hit {
            svm.set_sysvar::<SlotHashes>(&SlotHashes::new(&[(slot, hash)]));
            return;
        }
    }
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
    assert_eq!(get_round(&svm, 1).budget, miners_budget(round_budget(15)));

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

// ---------- lock-to-boost ----------

fn get_lock(svm: &LiteSVM, authority: &Pubkey) -> Lock {
    get_state(svm, &pda::lock_pda(authority).0)
}

/// Creates the per-user lock vault (the lock PDA's ATA) with a zero
/// balance, the same way clients ride an idempotent create-ATA instruction
/// in front of the Lock instruction.
fn create_lock_vault(svm: &mut LiteSVM, mint: &Pubkey, authority: &Pubkey) {
    let (lock_key, _) = pda::lock_pda(authority);
    let vault = pda::ata(&lock_key, mint);
    if svm.get_account(&vault).is_none() {
        svm.set_account(vault, token_account(mint, &lock_key, 0))
            .unwrap();
    }
}

fn lock_tokens(
    svm: &mut LiteSVM,
    authority: &Keypair,
    mint: &Pubkey,
    amount: u64,
    duration: i64,
) -> Result<(), String> {
    create_lock_vault(svm, mint, &authority.pubkey());
    send(
        svm,
        &[sdk::lock(authority.pubkey(), *mint, amount, duration)],
        authority,
        &[],
    )
}

/// Mine with the trailing Lock account (a miner not enrolled in referrals).
fn mine_lock(svm: &mut LiteSVM, authority: &Keypair, mint: &Pubkey) -> Result<(), String> {
    let config = get_config(svm);
    let miner = get_miner(svm, &authority.pubkey());
    let nonce = grind(svm, &authority.pubkey());
    let ix = sdk::with_lock(
        sdk::mine(
            authority.pubkey(),
            authority.pubkey(),
            *mint,
            config.current_round,
            miner.last_round,
            nonce,
        ),
        &authority.pubkey(),
    );
    send(svm, &[ix], authority, &[])
}

/// True when the account is gone (closed accounts may linger as
/// zero-lamport shells until the slot ends).
fn account_closed(svm: &LiteSVM, key: &Pubkey) -> bool {
    svm.get_account(key).map_or(true, |a| a.lamports == 0)
}

#[test]
fn test_lock_boost_full_flow() {
    let (mut svm, admin, mint) = setup();
    let alice = Keypair::new();
    register_miner(&mut svm, &alice);
    set_balance(&mut svm, &mint, &alice.pubkey(), 100 * ONE_TOKEN);

    let (duration, multiplier) = LOCK_TIERS[2]; // 90 days -> 2.0x
    lock_tokens(&mut svm, &alice, &mint, 40 * ONE_TOKEN, duration).expect("lock");

    // The tokens moved into the vault and the lock records the tier.
    assert_eq!(token_balance(&svm, &mint, &alice.pubkey()), 60 * ONE_TOKEN);
    let (lock_key, _) = pda::lock_pda(&alice.pubkey());
    let vault = pda::ata(&lock_key, &mint);
    let vault_acc = svm.get_account(&vault).unwrap();
    assert_eq!(
        u64::from_le_bytes(vault_acc.data[64..72].try_into().unwrap()),
        40 * ONE_TOKEN
    );
    let lock = get_lock(&svm, &alice.pubkey());
    assert_eq!(lock.amount, 40 * ONE_TOKEN);
    assert_eq!(lock.multiplier_bps, multiplier);

    // First submit: the wallet balance waits a round (min-balance rule) but
    // the locked tokens count immediately, times the multiplier.
    mine_lock(&mut svm, &alice, &mint).expect("mine 1");
    let round = get_round(&svm, get_config(&svm).current_round);
    assert_eq!(round.total_weight, INITIAL_BASE_WEIGHT + 80 * ONE_TOKEN);

    // Second round: the wallet balance joins in on top.
    advance_round(&mut svm, &admin);
    mine_lock(&mut svm, &alice, &mint).expect("mine 2");
    let round = get_round(&svm, get_config(&svm).current_round);
    assert_eq!(
        round.total_weight,
        INITIAL_BASE_WEIGHT + 60 * ONE_TOKEN + 80 * ONE_TOKEN
    );

    // Unlock before expiry is rejected.
    let err = send(&mut svm, &[sdk::unlock(alice.pubkey(), mint)], &alice, &[])
        .expect_err("unlock too early must fail");
    assert!(err.contains("Custom(18)"), "expected LockNotExpired: {err}");

    // Past expiry the whole stash comes back and both accounts close.
    let lock = get_lock(&svm, &alice.pubkey());
    warp_to(&mut svm, lock.unlock_ts + 1);
    send(&mut svm, &[sdk::unlock(alice.pubkey(), mint)], &alice, &[]).expect("unlock");
    assert_eq!(token_balance(&svm, &mint, &alice.pubkey()), 100 * ONE_TOKEN);
    assert!(account_closed(&svm, &pda::lock_pda(&alice.pubkey()).0));
    assert!(account_closed(&svm, &vault));
}

#[test]
fn test_lock_tiers_and_topup() {
    let (mut svm, _admin, mint) = setup();
    let alice = Keypair::new();
    register_miner(&mut svm, &alice);
    set_balance(&mut svm, &mint, &alice.pubkey(), 100 * ONE_TOKEN);

    // A duration outside the tiers is rejected.
    let err = lock_tokens(&mut svm, &alice, &mint, ONE_TOKEN, 86_400)
        .expect_err("a non-tier duration must fail");
    assert!(err.contains("Custom(16)"), "expected InvalidLockDuration: {err}");

    // Creating a lock with a zero amount is rejected.
    let err = lock_tokens(&mut svm, &alice, &mint, 0, LOCK_TIERS[0].0)
        .expect_err("a zero-amount create must fail");
    assert!(err.contains("Custom(17)"), "expected InvalidLockAmount: {err}");

    // Create at the 90-day tier; a 7-day top-up would shorten it: rejected.
    lock_tokens(&mut svm, &alice, &mint, 10 * ONE_TOKEN, LOCK_TIERS[2].0).expect("lock");
    let err = lock_tokens(&mut svm, &alice, &mint, 5 * ONE_TOKEN, LOCK_TIERS[0].0)
        .expect_err("a shortening top-up must fail");
    assert!(err.contains("Custom(16)"), "expected InvalidLockDuration: {err}");

    // A same-tier top-up accumulates the amount and extends the clock.
    let before = get_lock(&svm, &alice.pubkey());
    lock_tokens(&mut svm, &alice, &mint, 5 * ONE_TOKEN, LOCK_TIERS[2].0).expect("top-up");
    let after = get_lock(&svm, &alice.pubkey());
    assert_eq!(after.amount, 15 * ONE_TOKEN);
    assert!(after.unlock_ts >= before.unlock_ts);
    assert_eq!(token_balance(&svm, &mint, &alice.pubkey()), 85 * ONE_TOKEN);

    // Near expiry a shorter tier covers the remainder, so re-tiering (here
    // a pure extension with amount 0) is allowed; the multiplier follows.
    let warp_ts = after.unlock_ts - 86_400;
    warp_to(&mut svm, warp_ts);
    lock_tokens(&mut svm, &alice, &mint, 0, LOCK_TIERS[0].0).expect("re-tier");
    let lock = get_lock(&svm, &alice.pubkey());
    assert_eq!(lock.amount, 15 * ONE_TOKEN);
    assert_eq!(lock.multiplier_bps, LOCK_TIERS[0].1);
    assert_eq!(lock.unlock_ts, warp_ts + LOCK_TIERS[0].0);
}

#[test]
fn test_expired_lock_counts_at_face_value() {
    let (mut svm, admin, mint) = setup();
    let alice = Keypair::new();
    register_miner(&mut svm, &alice);
    set_balance(&mut svm, &mint, &alice.pubkey(), 50 * ONE_TOKEN);
    lock_tokens(&mut svm, &alice, &mint, 50 * ONE_TOKEN, LOCK_TIERS[0].0).expect("lock");

    // Active: 50 locked at the 7-day tier (1.2x) -> 60 weight.
    mine_lock(&mut svm, &alice, &mint).expect("mine while active");
    let round = get_round(&svm, get_config(&svm).current_round);
    assert_eq!(round.total_weight, INITIAL_BASE_WEIGHT + 60 * ONE_TOKEN);

    // Past expiry, not withdrawn: the tokens still count, at 1x.
    let lock = get_lock(&svm, &alice.pubkey());
    warp_to(&mut svm, lock.unlock_ts + 1);
    crank_next(&mut svm, &admin);
    mine_lock(&mut svm, &alice, &mint).expect("mine after expiry");
    let round = get_round(&svm, get_config(&svm).current_round);
    assert_eq!(round.total_weight, INITIAL_BASE_WEIGHT + 50 * ONE_TOKEN);
}

#[test]
fn test_lock_stacks_with_referral_boost() {
    let (mut svm, admin, mint) = setup();
    let referrer = Keypair::new();
    let bob = Keypair::new();
    register_miner(&mut svm, &referrer);
    register_miner(&mut svm, &bob);
    send(
        &mut svm,
        &[sdk::set_referrer(bob.pubkey(), referrer.pubkey())],
        &bob,
        &[],
    )
    .expect("enroll");

    set_balance(&mut svm, &mint, &bob.pubkey(), 40 * ONE_TOKEN);
    lock_tokens(&mut svm, &bob, &mint, 40 * ONE_TOKEN, LOCK_TIERS[2].0).expect("lock");

    // An enrolled miner cannot smuggle just the lock account past the
    // referral bookkeeping.
    let config = get_config(&svm);
    let miner = get_miner(&svm, &bob.pubkey());
    let nonce = grind(&svm, &bob.pubkey());
    let ix = sdk::with_lock(
        sdk::mine(
            bob.pubkey(),
            bob.pubkey(),
            mint,
            config.current_round,
            miner.last_round,
            nonce,
        ),
        &bob.pubkey(),
    );
    let err = send(&mut svm, &[ix], &bob, &[]).expect_err("referral account required");
    assert!(
        err.contains("Custom(14)"),
        "expected ReferralAccountRequired: {err}"
    );

    // Referral + lock together: the whole token slice (locked, multiplied)
    // gets the 15% boost, and the commission base records it.
    let ix = sdk::with_lock(
        sdk::mine_with_referral(
            bob.pubkey(),
            bob.pubkey(),
            mint,
            config.current_round,
            miner.last_round,
            nonce,
        ),
        &bob.pubkey(),
    );
    send(&mut svm, &[ix], &bob, &[]).expect("mine with referral + lock");
    let locked_weight = 40 * ONE_TOKEN * 2;
    let boosted = locked_weight * (BPS_DENOM + REFERRAL_BONUS_BPS) / BPS_DENOM;
    let round = get_round(&svm, get_config(&svm).current_round);
    assert_eq!(round.total_weight, INITIAL_BASE_WEIGHT + boosted);
    assert_eq!(
        get_referral(&svm, &bob.pubkey()).last_token_weight,
        boosted
    );

    // The trailing accounts are told apart by discriminator, so the
    // reversed order works just as well.
    advance_round(&mut svm, &admin);
    let config = get_config(&svm);
    let miner = get_miner(&svm, &bob.pubkey());
    let nonce = grind(&svm, &bob.pubkey());
    let mut ix = sdk::with_lock(
        sdk::mine_with_referral(
            bob.pubkey(),
            bob.pubkey(),
            mint,
            config.current_round,
            miner.last_round,
            nonce,
        ),
        &bob.pubkey(),
    );
    let n = ix.accounts.len();
    ix.accounts.swap(n - 2, n - 1);
    send(&mut svm, &[ix], &bob, &[]).expect("swapped trailing order works");
}

#[test]
fn test_foreign_lock_rejected() {
    use solana_sdk::instruction::{AccountMeta, Instruction};

    let (mut svm, _admin, mint) = setup();
    let alice = Keypair::new();
    let bob = Keypair::new();
    register_miner(&mut svm, &alice);
    register_miner(&mut svm, &bob);
    set_balance(&mut svm, &mint, &bob.pubkey(), 10 * ONE_TOKEN);
    lock_tokens(&mut svm, &bob, &mint, 10 * ONE_TOKEN, LOCK_TIERS[1].0).expect("bob locks");

    // Alice mining with bob's lock account.
    let config = get_config(&svm);
    let miner = get_miner(&svm, &alice.pubkey());
    let nonce = grind(&svm, &alice.pubkey());
    let ix = sdk::with_lock(
        sdk::mine(
            alice.pubkey(),
            alice.pubkey(),
            mint,
            config.current_round,
            miner.last_round,
            nonce,
        ),
        &bob.pubkey(),
    );
    let err = send(&mut svm, &[ix], &alice, &[]).expect_err("a foreign lock must fail");
    assert!(err.contains("Custom(1)"), "expected InvalidAccount: {err}");

    // Alice trying to unlock bob's stash into her own wallet.
    set_balance(&mut svm, &mint, &alice.pubkey(), 0);
    let (bob_lock, _) = pda::lock_pda(&bob.pubkey());
    let ix = Instruction {
        program_id: miner_api::id(),
        accounts: vec![
            AccountMeta::new(alice.pubkey(), true),
            AccountMeta::new(bob_lock, false),
            AccountMeta::new(pda::ata(&bob_lock, &mint), false),
            AccountMeta::new(pda::ata(&alice.pubkey(), &mint), false),
            AccountMeta::new_readonly(pda::config_pda().0, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
        ],
        data: vec![miner_api::instruction::MinerInstruction::Unlock as u8],
    };
    let err = send(&mut svm, &[ix], &alice, &[]).expect_err("a foreign unlock must fail");
    assert!(err.contains("Custom(3)"), "expected Unauthorized: {err}");
}

#[test]
fn test_unlock_rejects_non_canonical_vault() {
    use solana_sdk::instruction::{AccountMeta, Instruction};

    let (mut svm, _admin, mint) = setup();
    let alice = Keypair::new();
    register_miner(&mut svm, &alice);
    set_balance(&mut svm, &mint, &alice.pubkey(), 10 * ONE_TOKEN);
    lock_tokens(&mut svm, &alice, &mint, 10 * ONE_TOKEN, LOCK_TIERS[0].0).expect("lock");

    // A second token account owned by the lock PDA, at a non-ATA address.
    // Passing it as the vault must not be able to close the lock while the
    // real vault still holds the deposit.
    let (lock_key, _) = pda::lock_pda(&alice.pubkey());
    let decoy = Pubkey::new_unique();
    svm.set_account(decoy, token_account(&mint, &lock_key, ONE_TOKEN))
        .unwrap();

    let lock = get_lock(&svm, &alice.pubkey());
    warp_to(&mut svm, lock.unlock_ts + 1);

    let ix = Instruction {
        program_id: miner_api::id(),
        accounts: vec![
            AccountMeta::new(alice.pubkey(), true),
            AccountMeta::new(pda::lock_pda(&alice.pubkey()).0, false),
            AccountMeta::new(decoy, false),
            AccountMeta::new(pda::ata(&alice.pubkey(), &mint), false),
            AccountMeta::new_readonly(pda::config_pda().0, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
        ],
        data: vec![miner_api::instruction::MinerInstruction::Unlock as u8],
    };
    let err = send(&mut svm, &[ix], &alice, &[]).expect_err("a decoy vault must fail");
    assert!(err.contains("Custom(1)"), "expected InvalidAccount: {err}");

    // The canonical unlock still works and returns the full deposit.
    send(&mut svm, &[sdk::unlock(alice.pubkey(), mint)], &alice, &[]).expect("unlock");
    assert_eq!(token_balance(&svm, &mint, &alice.pubkey()), 10 * ONE_TOKEN);
    assert!(account_closed(&svm, &pda::lock_pda(&alice.pubkey()).0));

    // A second unlock has nothing to work on and fails cleanly.
    send(&mut svm, &[sdk::unlock(alice.pubkey(), mint)], &alice, &[])
        .expect_err("double unlock must fail");
}

// ---------- Motherlode ----------

fn get_win(svm: &LiteSVM, authority: &Pubkey) -> Win {
    get_state(svm, &pda::win_pda(authority).0)
}

/// SPL mint supply (to verify claim mints exactly the payout net of burn).
fn mint_supply(svm: &LiteSVM, mint: &Pubkey) -> u64 {
    let acc = svm.get_account(mint).unwrap();
    u64::from_le_bytes(acc.data[36..44].try_into().unwrap())
}

#[test]
fn test_motherlode_fee_and_hash_count() {
    let (mut svm, admin, mint) = setup();
    let alice = Keypair::new();
    let bob = Keypair::new();
    register_miner(&mut svm, &alice);
    register_miner(&mut svm, &bob);

    let fee_wallet_before = svm.get_balance(&FEE_WALLET).unwrap_or(0);

    // Every mine instruction pays the fee and counts as one chance at the
    // strike; the first hash always fills every candidate slot.
    mine(&mut svm, &alice, &mint).expect("alice mines");
    let ml = get_motherlode(&svm);
    assert_eq!(ml.hashes, 1);
    assert_eq!(ml.round_index, get_config(&svm).current_round);
    assert_eq!(ml.candidates, [alice.pubkey().to_bytes(); MOTHERLODE_WINNERS]);
    assert_eq!(ml.total_fees, MINE_FEE_LAMPORTS);
    assert_eq!(
        svm.get_balance(&FEE_WALLET).unwrap(),
        fee_wallet_before + MINE_FEE_LAMPORTS
    );

    mine(&mut svm, &bob, &mint).expect("bob mines");
    let ml = get_motherlode(&svm);
    assert_eq!(ml.hashes, 2);
    assert_eq!(ml.total_fees, 2 * MINE_FEE_LAMPORTS);
    assert_eq!(
        svm.get_balance(&FEE_WALLET).unwrap(),
        fee_wallet_before + 2 * MINE_FEE_LAMPORTS
    );

    // A new round resets the hash counter; the lifetime fee counter and
    // the pot never reset.
    rig_draw(&mut svm, false);
    advance_round(&mut svm, &admin);
    mine(&mut svm, &alice, &mint).expect("alice mines round 1");
    let ml = get_motherlode(&svm);
    assert_eq!(ml.hashes, 1);
    assert_eq!(ml.round_index, get_config(&svm).current_round);
    assert_eq!(ml.total_fees, 3 * MINE_FEE_LAMPORTS);
}

#[test]
fn test_motherlode_pot_draw_claim_burn() {
    let (mut svm, admin, mint) = setup();
    let alice = Keypair::new();
    register_miner(&mut svm, &alice);
    set_balance(&mut svm, &mint, &alice.pubkey(), 0);
    let tithe = motherlode_tithe(DEFAULT_BUDGET);

    // Round 0: mined, rigged miss -> the pot accrues, nobody wins.
    mine(&mut svm, &alice, &mint).expect("mine round 0");
    rig_draw(&mut svm, false);
    advance_round(&mut svm, &admin);
    let ml = get_motherlode(&svm);
    assert_eq!(ml.pot, tithe);
    assert!(svm.get_account(&pda::win_pda(&alice.pubkey()).0).is_none());

    // An empty round adds nothing to the pot (its emission lapses whole).
    rig_draw(&mut svm, false);
    advance_round(&mut svm, &admin);
    assert_eq!(get_motherlode(&svm).pot, tithe);

    // Round 2: mined, rigged hit -> the whole pot (two mined tithes) lands
    // in alice's Win account and the pot restarts.
    mine(&mut svm, &alice, &mint).expect("mine round 2");
    rig_draw(&mut svm, true);
    let round_seconds = get_config(&svm).round_seconds as i64;
    let mut clock: Clock = svm.get_sysvar();
    clock.unix_timestamp += round_seconds + 1;
    svm.set_sysvar(&clock);
    svm.expire_blockhash();

    // A crank pointing the win slots at the wrong wallets fails on a hit;
    // the retry with the real candidates goes through (the bot re-reads).
    let config = get_config(&svm);
    let err = send(
        &mut svm,
        &[sdk::crank(
            admin.pubkey(),
            config.current_round + 1,
            [Pubkey::new_unique(); MOTHERLODE_WINNERS],
        )],
        &admin,
        &[],
    )
    .expect_err("a crank with stale candidates must fail on a hit");
    assert!(err.contains("Custom(1)"), "expected InvalidAccount: {err}");
    crank_next(&mut svm, &admin);

    // Alice was the only miner, so she holds every candidate slot and the
    // whole pot (all three shares plus the division dust) lands in her
    // single Win account.
    let ml = get_motherlode(&svm);
    assert_eq!(ml.pot, 0);
    assert_eq!(ml.last_winners, [alice.pubkey().to_bytes(); MOTHERLODE_WINNERS]);
    assert_eq!(ml.last_win_amount, 2 * tithe / MOTHERLODE_WINNERS as u64);
    let win = get_win(&svm, &alice.pubkey());
    assert_eq!(win.authority, alice.pubkey().to_bytes());
    assert_eq!(win.amount, 2 * tithe);

    // Claim: 80% mints to the winner, 20% mints to the treasury ATA and
    // burns in the same instruction, the Win account closes.
    let (treasury, _) = pda::treasury_pda();
    svm.set_account(
        pda::ata(&treasury, &mint),
        token_account(&mint, &treasury, 0),
    )
    .unwrap();
    let supply_before = mint_supply(&svm, &mint);
    let expected_burn = 2 * tithe * MOTHERLODE_BURN_BPS / BPS_DENOM;
    send(
        &mut svm,
        &[sdk::claim_motherlode(alice.pubkey(), mint)],
        &alice,
        &[],
    )
    .expect("claim motherlode");
    assert_eq!(
        token_balance(&svm, &mint, &alice.pubkey()),
        2 * tithe - expected_burn
    );
    // Net supply grows only by the payout: the burned slice minted and
    // burned within the instruction.
    assert_eq!(
        mint_supply(&svm, &mint),
        supply_before + 2 * tithe - expected_burn
    );
    assert_eq!(token_balance(&svm, &mint, &treasury), 0);
    assert_eq!(get_motherlode(&svm).total_burned, expected_burn);
    assert!(account_closed(&svm, &pda::win_pda(&alice.pubkey()).0));

    // Nothing left to claim.
    send(
        &mut svm,
        &[sdk::claim_motherlode(alice.pubkey(), mint)],
        &alice,
        &[],
    )
    .expect_err("double claim must fail");
}

/// A strike with several miners in the round splits the pot across the
/// candidate slots: each candidate's Win account ends up with one share
/// per slot it holds (the division dust goes with slot 0), and the shares
/// always add back up to the whole pot.
#[test]
fn test_motherlode_split_across_candidates() {
    let (mut svm, admin, mint) = setup();
    let miners: Vec<Keypair> = (0..5).map(|_| Keypair::new()).collect();
    for m in &miners {
        register_miner(&mut svm, m);
    }
    for m in &miners {
        mine(&mut svm, m, &mint).expect("mine round 0");
    }
    let tithe = motherlode_tithe(DEFAULT_BUDGET);
    assert_eq!(get_motherlode(&svm).hashes, miners.len() as u64);

    rig_draw(&mut svm, true);
    advance_round(&mut svm, &admin);

    let ml = get_motherlode(&svm);
    assert_eq!(ml.pot, 0);
    let share = tithe / MOTHERLODE_WINNERS as u64;
    let remainder = tithe - share * MOTHERLODE_WINNERS as u64;
    assert_eq!(ml.last_win_amount, share);

    // Expected payout per wallet: one share per slot held, dust with slot 0.
    let mut expected = std::collections::HashMap::<[u8; 32], u64>::new();
    for (slot, cand) in ml.last_winners.iter().enumerate() {
        // Every winner really is one of the round's miners.
        assert!(miners.iter().any(|m| m.pubkey().to_bytes() == *cand));
        *expected.entry(*cand).or_default() +=
            share + if slot == 0 { remainder } else { 0 };
    }
    assert_eq!(expected.values().sum::<u64>(), tithe);
    for (cand, amount) in expected {
        let win = get_win(&svm, &Pubkey::new_from_array(cand));
        assert_eq!(win.authority, cand);
        assert_eq!(win.amount, amount);
    }
}

#[test]
fn test_motherlode_win_accumulates_and_draws_continue() {
    let (mut svm, admin, mint) = setup();
    let alice = Keypair::new();
    register_miner(&mut svm, &alice);
    set_balance(&mut svm, &mint, &alice.pubkey(), 0);
    let tithe = motherlode_tithe(DEFAULT_BUDGET);

    // First strike.
    mine(&mut svm, &alice, &mint).expect("mine round 0");
    rig_draw(&mut svm, true);
    advance_round(&mut svm, &admin);
    let win = get_win(&svm, &alice.pubkey());
    assert_eq!(win.amount, tithe);
    let first_since = win.since_ts;

    // Second strike before the first was claimed: the draw is not paused,
    // the amounts add up, the original timestamp stays.
    mine(&mut svm, &alice, &mint).expect("mine round 1");
    rig_draw(&mut svm, true);
    advance_round(&mut svm, &admin);
    let win = get_win(&svm, &alice.pubkey());
    assert_eq!(win.amount, 2 * tithe);
    assert_eq!(win.since_ts, first_since);
    assert_eq!(get_motherlode(&svm).pot, 0);

    // Someone else claiming alice's win fails.
    let bob = Keypair::new();
    svm.airdrop(&bob.pubkey(), 1_000_000_000).unwrap();
    let (treasury, _) = pda::treasury_pda();
    svm.set_account(
        pda::ata(&treasury, &mint),
        token_account(&mint, &treasury, 0),
    )
    .unwrap();
    let mut ix = sdk::claim_motherlode(bob.pubkey(), mint);
    // Point bob's claim at alice's win account.
    ix.accounts[1] = solana_sdk::instruction::AccountMeta::new(
        pda::win_pda(&alice.pubkey()).0,
        false,
    );
    let err = send(&mut svm, &[ix], &bob, &[]).expect_err("foreign claim must fail");
    assert!(err.contains("Custom(3)"), "expected Unauthorized: {err}");
}

// ---------- Tunnels (game) ----------

/// The EMA the game tests initialize with: 0.0005 SOL per token.
const GAME_EMA: u64 = 500_000;

/// Raw cp-amm pool bytes: mint/WSOL mints at their offsets and the Q64.64
/// sqrt price for the given spot (lamports per whole token).
fn pool_account(mint: &Pubkey, spot_lamports_per_token: u64) -> Account {
    let mut data = vec![0u8; CP_AMM_SQRT_PRICE_OFFSET + 16];
    data[CP_AMM_TOKEN_A_OFFSET..CP_AMM_TOKEN_A_OFFSET + 32].copy_from_slice(mint.as_ref());
    data[CP_AMM_TOKEN_B_OFFSET..CP_AMM_TOKEN_B_OFFSET + 32]
        .copy_from_slice(WSOL_MINT.as_ref());
    let sp = ((spot_lamports_per_token as f64 / 1e9).sqrt() * 18446744073709551616.0) as u128;
    data[CP_AMM_SQRT_PRICE_OFFSET..CP_AMM_SQRT_PRICE_OFFSET + 16]
        .copy_from_slice(&sp.to_le_bytes());
    Account {
        lamports: 1_000_000,
        data,
        owner: Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    }
}

/// The integer spot the program reads back from pool_account's bytes
/// (replicates the on-chain Q64.64 math; the float sqrt rounds a little).
fn expected_spot(pool: &Account) -> u64 {
    let sp = u128::from_le_bytes(
        pool.data[CP_AMM_SQRT_PRICE_OFFSET..CP_AMM_SQRT_PRICE_OFFSET + 16]
            .try_into()
            .unwrap(),
    );
    let s = sp >> 32;
    ((s * s).checked_mul(1_000_000_000).unwrap() >> 64) as u64
}

/// Creates the fake pool, the game token vault and the game state.
fn setup_game(svm: &mut LiteSVM, admin: &Keypair, mint: &Pubkey) -> Pubkey {
    let pool = Pubkey::new_unique();
    svm.set_account(pool, pool_account(mint, GAME_EMA)).unwrap();
    // The mint needs real supply on the books: the game burns from its
    // vault, and SPL burn decrements the supply counter.
    let (treasury, _) = pda::treasury_pda();
    svm.set_account(*mint, mint_account(&treasury, 1_000_000 * ONE_TOKEN))
        .unwrap();
    // The game token vault (the game PDA's ATA), created client-side.
    let (game_key, _) = pda::game_pda();
    svm.set_account(
        pda::game_token_vault(mint),
        token_account(mint, &game_key, 0),
    )
    .unwrap();
    send(
        svm,
        &[sdk::init_game(admin.pubkey(), pool, GAME_EMA)],
        admin,
        &[],
    )
    .expect("init_game");
    pool
}

fn get_game(svm: &LiteSVM) -> Game {
    get_state(svm, &pda::game_pda().0)
}

fn get_game_round(svm: &LiteSVM, index: u64) -> GameRound {
    get_state(svm, &pda::game_round_pda(index).0)
}

fn get_game_win(svm: &LiteSVM, authority: &Pubkey) -> GameWin {
    get_state(svm, &pda::game_win_pda(authority).0)
}

/// Stakes on a tunnel in the current game round.
fn game_stake(
    svm: &mut LiteSVM,
    player: &Keypair,
    mint: &Pubkey,
    tunnel: u8,
    sol: u64,
    miner: u64,
) -> Result<(), String> {
    let game = get_game(svm);
    send(
        svm,
        &[sdk::game_enter(
            player.pubkey(),
            *mint,
            game.current_round,
            tunnel,
            sol,
            miner,
        )],
        player,
        &[],
    )
}

/// Grinds a SlotHashes entropy so the next settle collapses exactly the
/// target tunnel set (when given) AND the players' Motherlode roll hits
/// or misses as requested, reproducing both on-chain rolls host-side
/// (uniform draw without replacement among ALL of the tunnels).
fn rig_game(svm: &mut LiteSVM, target: Option<&[usize]>, strike: bool) {
    let game = get_game(svm);
    let round = get_game_round(svm, game.current_round);
    let slot: u64 = 1;
    loop {
        let hash = Hash::new_unique();
        let mut entropy = [0u8; 40];
        entropy[..8].copy_from_slice(&slot.to_le_bytes());
        entropy[8..].copy_from_slice(hash.as_ref());

        let collapse_ok = match target {
            None => true,
            Some(t) => {
                let mut avail: Vec<usize> = (0..GAME_TUNNELS).collect();
                let mut dead = Vec::new();
                for pick in 0..GAME_COLLAPSES {
                    let roll_hash = solana_sdk::keccak::hashv(&[
                        entropy.as_slice(),
                        &round.index.to_le_bytes(),
                        &(pick as u64).to_le_bytes(),
                    ]);
                    let roll =
                        u64::from_le_bytes(roll_hash.as_ref()[..8].try_into().unwrap());
                    dead.push(avail.remove(roll as usize % avail.len()));
                }
                dead.len() == t.len() && t.iter().all(|x| dead.contains(x))
            }
        };

        let strike_hash = solana_sdk::keccak::hashv(&[
            entropy.as_slice(),
            &round.index.to_le_bytes(),
            &round.entries.to_le_bytes(),
            b"game_motherlode",
        ]);
        let strike_roll =
            u64::from_le_bytes(strike_hash.as_ref()[..8].try_into().unwrap());
        let strike_ok = (strike_roll % GAME_MOTHERLODE_ODDS == 0) == strike;

        if collapse_ok && strike_ok {
            svm.set_sysvar::<SlotHashes>(&SlotHashes::new(&[(slot, hash)]));
            return;
        }
    }
}

/// Settles the current game round at the current clock, exactly like the
/// bot: candidates read from the closing round.
fn game_settle_now(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey, pool: &Pubkey) {
    let game = get_game(svm);
    let round = get_game_round(svm, game.current_round);
    let candidates = round.candidates.map(Pubkey::new_from_array);
    send(
        svm,
        &[sdk::game_settle(
            payer.pubkey(),
            *mint,
            *pool,
            game.current_round + 1,
            candidates,
        )],
        payer,
        &[],
    )
    .expect("game_settle");
}

/// Advances the clock past the game round deadline, settles, and skips
/// the between-rounds intermission so the next round accepts entries.
fn advance_game_round(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey, pool: &Pubkey) {
    let game = get_game(svm);
    let mut clock: Clock = svm.get_sysvar();
    clock.unix_timestamp = game.round_start_ts + game.round_seconds as i64 + 1;
    svm.set_sysvar(&clock);
    svm.expire_blockhash();
    game_settle_now(svm, payer, mint, pool);
    let game = get_game(svm);
    let mut clock: Clock = svm.get_sysvar();
    if clock.unix_timestamp < game.round_start_ts {
        clock.unix_timestamp = game.round_start_ts + 1;
        svm.set_sysvar(&clock);
        svm.expire_blockhash();
    }
}

#[test]
fn game_init_admin_gate() {
    let (mut svm, admin, mint) = setup();
    let pool = Pubkey::new_unique();
    svm.set_account(pool, pool_account(&mint, GAME_EMA)).unwrap();

    // Non-admin init fails.
    let mallory = Keypair::new();
    svm.airdrop(&mallory.pubkey(), 10_000_000_000).unwrap();
    let err = send(
        &mut svm,
        &[sdk::init_game(mallory.pubkey(), pool, GAME_EMA)],
        &mallory,
        &[],
    )
    .expect_err("non-admin init_game must fail");
    assert!(err.contains("Custom(3)"), "expected Unauthorized: {err}");

    // Admin init succeeds, re-init fails.
    setup_game(&mut svm, &admin, &mint);
    let game = get_game(&svm);
    assert_eq!(game.ema_lamports_per_token, GAME_EMA);
    assert_eq!(game.round_seconds, GAME_ROUND_SECONDS);
    assert_eq!(game.current_round, 0);
    send(
        &mut svm,
        &[sdk::init_game(admin.pubkey(), pool, GAME_EMA)],
        &admin,
        &[],
    )
    .expect_err("re-init must fail");
}

#[test]
fn game_full_round_miner_tunnel_collapses() {
    let (mut svm, admin, mint) = setup();
    let pool = setup_game(&mut svm, &admin, &mint);

    let a = Keypair::new();
    let b = Keypair::new();
    let c = Keypair::new();
    for p in [&a, &b, &c] {
        svm.airdrop(&p.pubkey(), 10_000_000_000).unwrap();
    }
    // c stakes 2000 MINER; at the 500_000 EMA that is exactly 1 SOL of
    // weight, matching a and b.
    set_balance(&mut svm, &mint, &c.pubkey(), 2000 * ONE_TOKEN);
    game_stake(&mut svm, &a, &mint, 0, 1_000_000_000, 0).expect("a enters");
    game_stake(&mut svm, &b, &mint, 1, 1_000_000_000, 0).expect("b enters");
    game_stake(&mut svm, &c, &mint, 2, 0, 2000 * ONE_TOKEN).expect("c enters");

    let round = get_game_round(&svm, 0);
    let mut expected = [0u64; GAME_TUNNELS];
    expected[..3].copy_from_slice(&[1_000_000_000, 1_000_000_000, 1_000_000_000]);
    assert_eq!(round.weight, expected);
    assert_eq!(round.entries, 3);
    assert_eq!(token_balance(&svm, &mint, &c.pubkey()), 0);

    // The draw always takes 3 of the 9 tunnels, staked or not: rig
    // tunnels 1 (SOL), 2 (MINER) and the empty 5 to go; tunnel 0
    // survives alone.
    let supply_before = u64::from_le_bytes(
        svm.get_account(&mint).unwrap().data[36..44].try_into().unwrap(),
    );
    rig_game(&mut svm, Some(&[1, 2, 5]), false);
    advance_game_round(&mut svm, &admin, &mint, &pool);

    let round = get_game_round(&svm, 0);
    assert_eq!(round.settled, GAME_ROUND_SETTLED);
    assert_eq!(round.collapsed, 0b100110);
    assert_eq!(round.payout_sol, 900_000_000);
    assert_eq!(round.payout_miner, 1800 * ONE_TOKEN);
    assert_eq!(round.survivor_weight, 1_000_000_000);

    // Rake: 5% of the collapsed 2000 MINER burned on the spot (real supply
    // decrement), 5% into the players' Motherlode; the SOL side splits
    // the same way (5% fee wallet, 5% Motherlode).
    let supply_after = u64::from_le_bytes(
        svm.get_account(&mint).unwrap().data[36..44].try_into().unwrap(),
    );
    assert_eq!(supply_before - supply_after, 100 * ONE_TOKEN);
    let game = get_game(&svm);
    assert_eq!(game.total_burned, 100 * ONE_TOKEN);
    assert_eq!(game.ml_miner, 100 * ONE_TOKEN);
    assert_eq!(game.ml_sol, 50_000_000);
    assert_eq!(game.total_fee_sol, 50_000_000);
    assert_eq!(game.total_rounds_played, 1);
    assert_eq!(game.current_round, 1);

    // The sole survivor claims: 1 SOL stake back + the whole payout
    // (0.9 SOL + 1800 MINER).
    set_balance(&mut svm, &mint, &a.pubkey(), 0);
    let a_lamports = svm.get_account(&a.pubkey()).unwrap().lamports;
    send(&mut svm, &[sdk::game_claim(a.pubkey(), mint, 0)], &a, &[]).expect("a claims");
    assert_eq!(token_balance(&svm, &mint, &a.pubkey()), 1800 * ONE_TOKEN);
    let a_gain = svm.get_account(&a.pubkey()).unwrap().lamports - a_lamports;
    // 1 SOL stake + 0.9 SOL payout + entry rent - tx fee.
    assert!(a_gain > 1_899_000_000 && a_gain < 1_903_000_000, "a gain {a_gain}");

    // The collapsed stakes claim nothing (the entries still close).
    set_balance(&mut svm, &mint, &b.pubkey(), 0);
    send(&mut svm, &[sdk::game_claim(b.pubkey(), mint, 0)], &b, &[]).expect("b claims");
    assert_eq!(token_balance(&svm, &mint, &b.pubkey()), 0);

    send(&mut svm, &[sdk::game_claim(c.pubkey(), mint, 0)], &c, &[]).expect("c claims");
    assert_eq!(token_balance(&svm, &mint, &c.pubkey()), 0);
    assert!(svm
        .get_account(&pda::game_entry_pda(0, &c.pubkey()).0)
        .map(|a| a.data.is_empty())
        .unwrap_or(true));

    // Double claim fails (the entry is gone).
    send(&mut svm, &[sdk::game_claim(a.pubkey(), mint, 0)], &a, &[])
        .expect_err("double claim must fail");

    // The vault keeps exactly the players' Motherlode share.
    let (game_key, _) = pda::game_pda();
    assert_eq!(
        token_balance(&svm, &mint, &game_key),
        100 * ONE_TOKEN
    );
}

#[test]
fn game_full_round_sol_tunnel_collapses() {
    let (mut svm, admin, mint) = setup();
    let pool = setup_game(&mut svm, &admin, &mint);

    let a = Keypair::new();
    let b = Keypair::new();
    let c = Keypair::new();
    for p in [&a, &b, &c] {
        svm.airdrop(&p.pubkey(), 10_000_000_000).unwrap();
    }
    game_stake(&mut svm, &a, &mint, 0, 2_000_000_000, 0).expect("a enters");
    game_stake(&mut svm, &b, &mint, 1, 1_000_000_000, 0).expect("b enters");
    // c hedges: half on tunnel 0, half on tunnel 1.
    game_stake(&mut svm, &c, &mint, 0, 500_000_000, 0).expect("c enters t0");
    game_stake(&mut svm, &c, &mint, 1, 500_000_000, 0).expect("c hedges t1");

    // Rig tunnel 0 plus two empty tunnels to go; tunnel 1 survives.
    let fee_before = svm.get_account(&FEE_WALLET).unwrap().lamports;
    rig_game(&mut svm, Some(&[0, 4, 8]), false);
    advance_game_round(&mut svm, &admin, &mint, &pool);

    // Tunnel 0 (2.5 SOL) collapsed: 0.125 SOL to the fee wallet (buyback),
    // 0.125 SOL to the players' Motherlode, 2.25 SOL to the survivors.
    let round = get_game_round(&svm, 0);
    assert_eq!(round.collapsed, 0b100010001);
    assert_eq!(round.entries, 3);
    assert_eq!(round.payout_sol, 2_250_000_000);
    assert_eq!(round.survivor_weight, 1_500_000_000);
    let game = get_game(&svm);
    assert_eq!(game.ml_sol, 125_000_000);
    assert_eq!(game.total_fee_sol, 125_000_000);
    assert_eq!(
        svm.get_account(&FEE_WALLET).unwrap().lamports - fee_before,
        125_000_000
    );

    // b: 1 SOL stake back + 2.25 * (1/1.5) = 2.5 SOL total.
    let b_lamports = svm.get_account(&b.pubkey()).unwrap().lamports;
    send(&mut svm, &[sdk::game_claim(b.pubkey(), mint, 0)], &b, &[]).expect("b claims");
    let b_gain = svm.get_account(&b.pubkey()).unwrap().lamports - b_lamports;
    assert!(
        b_gain > 2_499_000_000 && b_gain < 2_503_000_000,
        "b gain {b_gain}"
    );

    // c: the tunnel 0 half fell, the tunnel 1 half survived:
    // 0.5 back + 2.25 * (0.5/1.5) = 1.25 SOL total.
    let c_lamports = svm.get_account(&c.pubkey()).unwrap().lamports;
    send(&mut svm, &[sdk::game_claim(c.pubkey(), mint, 0)], &c, &[]).expect("c claims");
    let c_gain = svm.get_account(&c.pubkey()).unwrap().lamports - c_lamports;
    assert!(
        c_gain > 1_249_000_000 && c_gain < 1_253_000_000,
        "c gain {c_gain}"
    );
}

#[test]
fn game_lone_survivor_refunds_and_total_collapse_burns() {
    let (mut svm, admin, mint) = setup();
    let pool = setup_game(&mut svm, &admin, &mint);

    let a = Keypair::new();
    svm.airdrop(&a.pubkey(), 10_000_000_000).unwrap();
    set_balance(&mut svm, &mint, &a.pubkey(), 1000 * ONE_TOKEN);

    // Round 0: a mixed stake, alone in the round, and the draw misses its
    // tunnel: the collapsed pots are empty, the stake comes back in full.
    game_stake(&mut svm, &a, &mint, 1, 500_000_000, 500 * ONE_TOKEN).expect("a enters");
    rig_game(&mut svm, Some(&[0, 4, 8]), false);
    advance_game_round(&mut svm, &admin, &mint, &pool);
    let round = get_game_round(&svm, 0);
    assert_eq!(round.settled, GAME_ROUND_SETTLED);
    assert_eq!(round.payout_sol + round.payout_miner, 0);

    let a_lamports = svm.get_account(&a.pubkey()).unwrap().lamports;
    send(&mut svm, &[sdk::game_claim(a.pubkey(), mint, 0)], &a, &[]).expect("a claims");
    assert_eq!(token_balance(&svm, &mint, &a.pubkey()), 1000 * ONE_TOKEN);
    let a_gain = svm.get_account(&a.pubkey()).unwrap().lamports - a_lamports;
    assert!(a_gain > 499_000_000 && a_gain < 503_000_000, "a gain {a_gain}");
    let game = get_game(&svm);
    assert_eq!(game.total_burned, 0);
    assert_eq!(game.ml_sol + game.ml_miner, 0);

    // Round 1: no staked tunnel survives: nobody to pay, so the whole
    // pot goes to buyback/burn ($MINER burns, SOL to the fee wallet) and
    // the claim returns only the entry rent.
    game_stake(&mut svm, &a, &mint, 1, 500_000_000, 500 * ONE_TOKEN).expect("a re-enters");
    let fee_before = svm.get_account(&FEE_WALLET).unwrap().lamports;
    rig_game(&mut svm, Some(&[1, 4, 8]), false);
    advance_game_round(&mut svm, &admin, &mint, &pool);
    let round = get_game_round(&svm, 1);
    assert_eq!(round.settled, GAME_ROUND_SETTLED);
    assert_eq!(round.survivor_weight, 0);
    assert_eq!(round.payout_sol + round.payout_miner, 0);
    let game = get_game(&svm);
    assert_eq!(game.total_burned, 500 * ONE_TOKEN);
    assert_eq!(game.total_fee_sol, 500_000_000);
    assert_eq!(game.ml_sol + game.ml_miner, 0);
    assert_eq!(
        svm.get_account(&FEE_WALLET).unwrap().lamports - fee_before,
        500_000_000
    );

    let a_lamports = svm.get_account(&a.pubkey()).unwrap().lamports;
    send(&mut svm, &[sdk::game_claim(a.pubkey(), mint, 1)], &a, &[]).expect("a claims r1");
    assert_eq!(token_balance(&svm, &mint, &a.pubkey()), 500 * ONE_TOKEN);
    let a_gain = svm.get_account(&a.pubkey()).unwrap().lamports - a_lamports;
    // Only the entry rent comes back.
    assert!(a_gain < 5_000_000, "a gain {a_gain}");
}

#[test]
fn game_enter_guards() {
    let (mut svm, admin, mint) = setup();
    let pool = setup_game(&mut svm, &admin, &mint);

    let a = Keypair::new();
    svm.airdrop(&a.pubkey(), 10_000_000_000).unwrap();

    // Invalid tunnel.
    let err =
        game_stake(&mut svm, &a, &mint, GAME_TUNNELS as u8, 1_000_000_000, 0).unwrap_err();
    assert!(err.contains("Custom(19)"), "expected InvalidTunnel: {err}");

    // Below the minimum stake value.
    let err = game_stake(&mut svm, &a, &mint, 0, GAME_MIN_WEIGHT - 1, 0).unwrap_err();
    assert!(err.contains("Custom(24)"), "expected InvalidStake: {err}");

    // One wallet can spread the round's stake across tunnels (the hedge)
    // and top up a tunnel it already staked; it still counts as one
    // player (entries do not recount).
    game_stake(&mut svm, &a, &mint, 0, 1_000_000_000, 0).expect("a enters");
    game_stake(&mut svm, &a, &mint, 1, 750_000_000, 0).expect("hedge on tunnel 1");
    game_stake(&mut svm, &a, &mint, 0, 1_500_000_000, 0).expect("top-up");
    let round = get_game_round(&svm, 0);
    assert_eq!(round.entries, 1);
    assert_eq!(round.weight[0], 2_500_000_000);
    assert_eq!(round.weight[1], 750_000_000);
    let entry: GameEntry = get_state(&svm, &pda::game_entry_pda(0, &a.pubkey()).0);
    let mut expected = [0u64; GAME_TUNNELS];
    expected[..2].copy_from_slice(&[2_500_000_000, 750_000_000]);
    assert_eq!(entry.sol, expected);
    assert_eq!(entry.weight, expected);

    // Claim before settle fails.
    let err = send(&mut svm, &[sdk::game_claim(a.pubkey(), mint, 0)], &a, &[])
        .unwrap_err();
    assert!(err.contains("Custom(22)"), "expected GameNotSettled: {err}");

    // Settle before the deadline fails.
    let game = get_game(&svm);
    let round0 = get_game_round(&svm, 0);
    let err = send(
        &mut svm,
        &[sdk::game_settle(
            admin.pubkey(),
            mint,
            pool,
            game.current_round + 1,
            round0.candidates.map(Pubkey::new_from_array),
        )],
        &admin,
        &[],
    )
    .unwrap_err();
    assert!(err.contains("Custom(20)"), "expected RoundStillOpen: {err}");

    // Entries close at the deadline even before the settle runs.
    let mut clock: Clock = svm.get_sysvar();
    clock.unix_timestamp = game.round_start_ts + game.round_seconds as i64 + 1;
    svm.set_sysvar(&clock);
    svm.expire_blockhash();
    let err = game_stake(&mut svm, &a, &mint, 0, 1_000_000_000, 0).unwrap_err();
    assert!(err.contains("Custom(21)"), "expected GameRoundClosed: {err}");

    // After the settle the next round sits in its intermission: entries
    // stay closed until start_ts, then open.
    game_settle_now(&mut svm, &admin, &mint, &pool);
    let game = get_game(&svm);
    let clock: Clock = svm.get_sysvar();
    assert!(
        clock.unix_timestamp < game.round_start_ts,
        "the new round must start after an intermission"
    );
    let err = game_stake(&mut svm, &a, &mint, 0, 1_000_000_000, 0).unwrap_err();
    assert!(err.contains("Custom(21)"), "expected GameRoundClosed: {err}");
    let mut clock: Clock = svm.get_sysvar();
    clock.unix_timestamp = game.round_start_ts + 1;
    svm.set_sysvar(&clock);
    svm.expire_blockhash();
    game_stake(&mut svm, &a, &mint, 0, 1_000_000_000, 0)
        .expect("entry after the intermission");
}

#[test]
fn game_motherlode_strike_and_claim() {
    let (mut svm, admin, mint) = setup();
    let pool = setup_game(&mut svm, &admin, &mint);

    let a = Keypair::new();
    let b = Keypair::new();
    let c = Keypair::new();
    for p in [&a, &b, &c] {
        svm.airdrop(&p.pubkey(), 10_000_000_000).unwrap();
    }

    // Round 0: a SOL tunnel collapses -> the pools get SOL.
    game_stake(&mut svm, &a, &mint, 0, 2_000_000_000, 0).expect("a enters");
    game_stake(&mut svm, &b, &mint, 1, 1_000_000_000, 0).expect("b enters");
    rig_game(&mut svm, Some(&[0, 4, 8]), false);
    advance_game_round(&mut svm, &admin, &mint, &pool);

    // Round 1: a MINER tunnel collapses -> the pools get MINER too.
    set_balance(&mut svm, &mint, &c.pubkey(), 2000 * ONE_TOKEN);
    game_stake(&mut svm, &c, &mint, 2, 0, 2000 * ONE_TOKEN).expect("c enters");
    game_stake(&mut svm, &b, &mint, 1, 1_000_000_000, 0).expect("b enters r1");
    rig_game(&mut svm, Some(&[2, 4, 8]), false);
    advance_game_round(&mut svm, &admin, &mint, &pool);

    let game = get_game(&svm);
    assert_eq!(game.ml_sol, 100_000_000);
    assert_eq!(game.ml_miner, 100 * ONE_TOKEN);

    // Round 2: d plays alone, its tunnel survives the draw, and the
    // strike hits. As the only wallet, d holds all candidate slots and
    // takes the whole pools.
    let d = Keypair::new();
    svm.airdrop(&d.pubkey(), 10_000_000_000).unwrap();
    game_stake(&mut svm, &d, &mint, 0, 1_000_000_000, 0).expect("d enters");
    let round = get_game_round(&svm, 2);
    assert_eq!(round.candidates, [d.pubkey().to_bytes(); GAME_MOTHERLODE_WINNERS]);
    rig_game(&mut svm, Some(&[4, 7, 8]), true);
    advance_game_round(&mut svm, &admin, &mint, &pool);

    let game = get_game(&svm);
    assert_eq!(game.ml_sol, 0);
    assert_eq!(game.ml_miner, 0);
    assert_eq!(game.ml_last_winners[0], d.pubkey().to_bytes());
    let win = get_game_win(&svm, &d.pubkey());
    assert_eq!(win.sol, 100_000_000);
    assert_eq!(win.miner, 100 * ONE_TOKEN);

    // d claims the win: both assets arrive, the win PDA closes.
    set_balance(&mut svm, &mint, &d.pubkey(), 0);
    let d_lamports = svm.get_account(&d.pubkey()).unwrap().lamports;
    send(&mut svm, &[sdk::game_claim_win(d.pubkey(), mint)], &d, &[])
        .expect("d claims win");
    assert_eq!(token_balance(&svm, &mint, &d.pubkey()), 100 * ONE_TOKEN);
    let d_gain = svm.get_account(&d.pubkey()).unwrap().lamports - d_lamports;
    assert!(d_gain > 99_000_000 && d_gain < 103_000_000, "d gain {d_gain}");
    send(&mut svm, &[sdk::game_claim_win(d.pubkey(), mint)], &d, &[])
        .expect_err("double win claim must fail");

    // d's surviving entry still refunds in full (empty pots, no payout).
    let d_lamports = svm.get_account(&d.pubkey()).unwrap().lamports;
    send(&mut svm, &[sdk::game_claim(d.pubkey(), mint, 2)], &d, &[]).expect("d refund");
    let d_gain = svm.get_account(&d.pubkey()).unwrap().lamports - d_lamports;
    assert!(d_gain > 999_000_000, "d refund {d_gain}");
}

#[test]
fn game_ema_steps_and_clamps() {
    let (mut svm, admin, mint) = setup();
    let pool = setup_game(&mut svm, &admin, &mint);

    // Spot moves to ~600_000: the EMA steps by (spot - ema) / 8.
    let acc = pool_account(&mint, 600_000);
    let spot = expected_spot(&acc);
    svm.set_account(pool, acc).unwrap();
    advance_game_round(&mut svm, &admin, &mint, &pool);
    let ema1 = get_game(&svm).ema_lamports_per_token;
    assert_eq!(ema1, GAME_EMA + (spot - GAME_EMA) / GAME_EMA_ALPHA);

    // Spot 10x: the per-settle move clamps at +5%.
    svm.set_account(pool, pool_account(&mint, 5_000_000)).unwrap();
    advance_game_round(&mut svm, &admin, &mint, &pool);
    let ema2 = get_game(&svm).ema_lamports_per_token;
    assert_eq!(
        ema2,
        ((ema1 as u128) * ((BPS_DENOM + GAME_EMA_CLAMP_BPS) as u128) / (BPS_DENOM as u128))
            as u64
    );

    // Spot crash: clamps at -5% per settle.
    svm.set_account(pool, pool_account(&mint, 50_000)).unwrap();
    advance_game_round(&mut svm, &admin, &mint, &pool);
    let ema3 = get_game(&svm).ema_lamports_per_token;
    assert_eq!(
        ema3,
        ((ema2 as u128) * ((BPS_DENOM - GAME_EMA_CLAMP_BPS) as u128) / (BPS_DENOM as u128))
            as u64
    );

    // A foreign account in the pool slot is rejected.
    let game = get_game(&svm);
    let round = get_game_round(&svm, game.current_round);
    let mut clock: Clock = svm.get_sysvar();
    clock.unix_timestamp = game.round_start_ts + game.round_seconds as i64 + 1;
    svm.set_sysvar(&clock);
    svm.expire_blockhash();
    let err = send(
        &mut svm,
        &[sdk::game_settle(
            admin.pubkey(),
            mint,
            Pubkey::new_unique(),
            game.current_round + 1,
            round.candidates.map(Pubkey::new_from_array),
        )],
        &admin,
        &[],
    )
    .unwrap_err();
    assert!(err.contains("Custom(1)"), "expected InvalidAccount: {err}");
}

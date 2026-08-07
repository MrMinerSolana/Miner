//! MINER protocol CLI: init / status / mine / crank / claim.
//!
//! Configuration via environment variables:
//!   RPC_URL     (defaults to the public mainnet endpoint; for serious
//!               mining use your own, e.g. a free Helius key)
//!   KEYPAIR     (defaults to ~/.config/solana/id.json)

use std::{thread::sleep, time::Duration};

use miner_api::{consts::*, pda, sdk, state::*};
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair, Signer},
    transaction::Transaction,
};

const PREMINE: u64 = 1_000_000 * ONE_TOKEN;

// Token metadata (Metaplex). The URI points to a JSON with the description,
// logo and website, served from the project site.
const TOKEN_NAME: &str = "MINER";
const TOKEN_SYMBOL: &str = "MINER";
const TOKEN_URI: &str = "https://miner.tools/token.json";
const MPL_TOKEN_METADATA_ID: Pubkey =
    Pubkey::from_str_const("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");
const RENT_SYSVAR_ID: Pubkey =
    Pubkey::from_str_const("SysvarRent111111111111111111111111111111111");

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("help");
    let ctx = Ctx::new();
    match cmd {
        "init" => cmd_init(&ctx, &args[2..]),
        "create-metadata" => cmd_create_metadata(&ctx, &args[2..]),
        "status" => cmd_status(&ctx),
        "mine" => cmd_mine(&ctx),
        "crank" => cmd_crank(&ctx),
        "claim" => cmd_claim(&ctx),
        "set-referrer" => cmd_set_referrer(&ctx, &args[2..]),
        "set-refname" => cmd_set_refname(&ctx, &args[2..]),
        "lock" => cmd_lock(&ctx, &args[2..]),
        "unlock" => cmd_unlock(&ctx),
        "update-config" => cmd_update_config(&ctx, &args[2..]),
        "set-admin" => cmd_set_admin(&ctx, &args[2..]),
        _ => {
            eprintln!("usage: miner <init|status|mine|crank|claim|set-referrer|set-refname|lock|unlock|update-config|set-admin>");
            eprintln!("  set-referrer <name_or_pubkey>    (once, immutable: +15% token weight,");
            eprintln!("                                    5/3/1% of token rewards up the chain)");
            eprintln!("  set-refname <name>               (once, immutable: your custom referral");
            eprintln!("                                    code, 3-16 chars a-z 0-9 _)");
            eprintln!("  lock <amount_tokens> <days>      (lock-to-boost: 7 d = 1.2x, 30 d = 1.5x,");
            eprintln!("                                    90 d = 2.0x weight; top-up re-locks all)");
            eprintln!("  unlock                           (withdraw an expired lock)");
            eprintln!("  update-config <min_difficulty> <base_weight_tokens> <round_seconds>");
            eprintln!("  set-admin <new_admin_pubkey>");
            std::process::exit(1);
        }
    }
}

struct Ctx {
    rpc: RpcClient,
    payer: Keypair,
}

impl Ctx {
    fn new() -> Self {
        let url = std::env::var("RPC_URL")
            .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
        let keypair_path = std::env::var("KEYPAIR").unwrap_or_else(|_| {
            format!(
                "{}/.config/solana/id.json",
                std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap()
            )
        });
        let payer = read_keypair_file(&keypair_path).expect("cannot read keypair file");
        let rpc = RpcClient::new_with_commitment(url, CommitmentConfig::confirmed());
        Self { rpc, payer }
    }

    fn send(&self, ixs: &[Instruction], extra_signers: &[&Keypair]) -> Result<(), String> {
        let blockhash = self
            .rpc
            .get_latest_blockhash()
            .map_err(|e| e.to_string())?;
        let mut signers: Vec<&Keypair> = vec![&self.payer];
        signers.extend_from_slice(extra_signers);
        let tx = Transaction::new_signed_with_payer(
            ixs,
            Some(&self.payer.pubkey()),
            &signers,
            blockhash,
        );
        self.rpc
            .send_and_confirm_transaction(&tx)
            .map(|sig| println!("  tx: {sig}"))
            .map_err(|e| e.to_string())
    }

    fn read<T: bytemuck::Pod>(&self, key: &Pubkey) -> Option<T> {
        let data = self.rpc.get_account_data(key).ok()?;
        if data.len() != core::mem::size_of::<T>() {
            return None;
        }
        Some(bytemuck::pod_read_unaligned::<T>(&data))
    }

    /// Config with retries: an RPC error (429 etc.) is not the same as a
    /// missing account.
    fn config(&self) -> Config {
        // RPC can return spurious "AccountNotFound" (a lagging node or
        // throttling), so treat the config as missing only after several
        // such reads in a row, so a long-running crank doesn't die on
        // a single hiccup.
        let mut missing_streak = 0u32;
        loop {
            match self.rpc.get_account_data(&pda::config_pda().0) {
                Ok(data) if data.len() == core::mem::size_of::<Config>() => {
                    return bytemuck::pod_read_unaligned::<Config>(&data);
                }
                Ok(_) => panic!("no config account, run `miner init` first"),
                Err(e) => {
                    let msg = e.to_string();
                    // Note: on transport errors (e.g. 429) the client also
                    // reports "AccountNotFound: ... HTTP status ...", so only
                    // consider the account missing without an HTTP error.
                    let looks_missing =
                        msg.contains("AccountNotFound") && !msg.contains("HTTP");
                    if looks_missing {
                        missing_streak += 1;
                        if missing_streak >= 5 {
                            panic!("no config account, run `miner init` first");
                        }
                    } else {
                        missing_streak = 0;
                    }
                    eprintln!("  RPC error ({}), retrying in 5 s…", short_err(&msg));
                    sleep(Duration::from_secs(5));
                }
            }
        }
    }
}

// ---------- SPL instructions built by hand ----------

fn ix_initialize_mint2(mint: &Pubkey, authority: &Pubkey) -> Instruction {
    let mut data = vec![20u8, TOKEN_DECIMALS];
    data.extend_from_slice(authority.as_ref());
    data.push(0); // freeze_authority: None
    Instruction {
        program_id: SPL_TOKEN_PROGRAM_ID,
        accounts: vec![AccountMeta::new(*mint, false)],
        data,
    }
}

fn ix_mint_to(mint: &Pubkey, dest: &Pubkey, authority: &Pubkey, amount: u64) -> Instruction {
    let mut data = vec![SPL_TOKEN_MINT_TO_IX];
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: SPL_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*mint, false),
            AccountMeta::new(*dest, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data,
    }
}

fn ix_set_mint_authority(mint: &Pubkey, current: &Pubkey, new: &Pubkey) -> Instruction {
    let mut data = vec![6u8, 0u8, 1u8]; // SetAuthority, MintTokens, COption::Some
    data.extend_from_slice(new.as_ref());
    Instruction {
        program_id: SPL_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*mint, false),
            AccountMeta::new_readonly(*current, true),
        ],
        data,
    }
}

/// Metaplex CreateMetadataAccountV3 built by hand (avoids pulling the mpl
/// crate and its solana-program version conflicts). Must run BEFORE the
/// mint authority moves to the treasury PDA, because it needs its signature.
fn ix_create_metadata_v3(
    mint: &Pubkey,
    mint_authority: &Pubkey,
    payer: &Pubkey,
    update_authority: &Pubkey,
) -> Instruction {
    let (metadata, _) = Pubkey::find_program_address(
        &[b"metadata", MPL_TOKEN_METADATA_ID.as_ref(), mint.as_ref()],
        &MPL_TOKEN_METADATA_ID,
    );

    // Borsh: discriminator 33 + DataV2 + is_mutable + collection_details.
    let mut data = vec![33u8];
    for s in [TOKEN_NAME, TOKEN_SYMBOL, TOKEN_URI] {
        data.extend_from_slice(&(s.len() as u32).to_le_bytes());
        data.extend_from_slice(s.as_bytes());
    }
    data.extend_from_slice(&0u16.to_le_bytes()); // seller_fee_basis_points
    data.extend_from_slice(&[0, 0, 0]); // creators/collection/uses: None
    data.push(1); // is_mutable (logo/description fixable via update authority)
    data.push(0); // collection_details: None

    Instruction {
        program_id: MPL_TOKEN_METADATA_ID,
        accounts: vec![
            AccountMeta::new(metadata, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(*mint_authority, true),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(*update_authority, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            AccountMeta::new_readonly(RENT_SYSVAR_ID, false),
        ],
        data,
    }
}

fn ix_create_ata_idempotent(payer: &Pubkey, wallet: &Pubkey, mint: &Pubkey) -> Instruction {
    Instruction {
        program_id: ASSOCIATED_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(pda::ata(wallet, mint), false),
            AccountMeta::new_readonly(*wallet, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
        ],
        data: vec![1], // CreateIdempotent
    }
}

// ---------- commands ----------

/// Launch: mint -> Metaplex metadata -> premine to the payer (seeds the LP)
/// -> mint authority to the treasury PDA -> program initialize. One atomic
/// transaction. Optional argument: metadata update authority (on mainnet:
/// the multisig vault, so a compromised deployer key cannot e.g. swap the
/// website link).
fn cmd_init(ctx: &Ctx, args: &[String]) {
    let payer = ctx.payer.pubkey();
    let metadata_update_authority: Pubkey = args
        .first()
        .map(|a| a.parse().expect("update authority: valid pubkey"))
        .unwrap_or(payer);
    if ctx.read::<Config>(&pda::config_pda().0).is_some() {
        println!("Program already initialized.");
        return;
    }
    let mint_kp = Keypair::new();
    let mint = mint_kp.pubkey();
    let (treasury, _) = pda::treasury_pda();

    println!("mint:     {mint}");
    println!("treasury: {treasury}");
    println!("config:   {}", pda::config_pda().0);

    let rent = ctx
        .rpc
        .get_minimum_balance_for_rent_exemption(82)
        .expect("rpc rent");
    let ixs = vec![
        solana_system_interface::instruction::create_account(
            &payer,
            &mint,
            rent,
            82,
            &SPL_TOKEN_PROGRAM_ID,
        ),
        ix_initialize_mint2(&mint, &payer),
        ix_create_metadata_v3(&mint, &payer, &payer, &metadata_update_authority),
        ix_create_ata_idempotent(&payer, &payer, &mint),
        ix_mint_to(&mint, &pda::ata(&payer, &mint), &payer, PREMINE),
        ix_set_mint_authority(&mint, &payer, &treasury),
        sdk::initialize(payer, mint),
    ];
    ctx.send(&ixs, &[&mint_kp]).expect("init failed");
    println!("OK: premined {} tokens to {}", PREMINE / ONE_TOKEN, payer);
}

fn cmd_status(ctx: &Ctx) {
    let config = ctx.config();
    let mint = Pubkey::new_from_array(config.mint);
    println!("mint:          {mint}");
    println!("round:         #{}", config.current_round);
    println!("round length:  {} s", config.round_seconds);
    println!("difficulty:    {} bits", config.min_difficulty);
    println!("base weight:   {} tokens", config.base_weight / ONE_TOKEN);
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let epoch = if now <= HALVING_ANCHOR_TS {
            0
        } else {
            ((now - HALVING_ANCHOR_TS) / HALVING_SECONDS) as u32
        };
        let emission = EMISSION_PER_MINUTE.checked_shr(epoch).unwrap_or(0);
        let next = HALVING_ANCHOR_TS + (epoch as i64 + 1) * HALVING_SECONDS;
        println!(
            "halving:       epoch {epoch} | emission {:.2} tokens/min | next in ~{} d",
            emission as f64 / ONE_TOKEN as f64,
            (next - now).max(0) / 86_400
        );
    }
    if let Some(round) = ctx.read::<Round>(&pda::round_pda(config.current_round).0) {
        println!(
            "total weight:  {:.2} tokens | budget {:.2} tokens",
            round.total_weight as f64 / ONE_TOKEN as f64,
            round.budget as f64 / ONE_TOKEN as f64
        );
    }
    let me = ctx.payer.pubkey();
    match ctx.read::<Miner>(&pda::miner_pda(&me).0) {
        Some(m) => {
            println!("--- miner {me} ---");
            println!("pending:       {:.4} tokens", m.pending_rewards as f64 / ONE_TOKEN as f64);
            println!("mined:         {:.4} tokens", m.total_mined as f64 / ONE_TOKEN as f64);
            println!("hashes:        {}", m.total_hashes);
            println!("last round #{} (weight {:.2})", m.last_round, m.last_round_weight as f64 / ONE_TOKEN as f64);
            match ctx.read::<Referral>(&pda::referral_pda(&me).0) {
                Some(r) => {
                    println!(
                        "referral:      +{}% token weight, referrer {}",
                        REFERRAL_BONUS_BPS / 100,
                        Pubkey::new_from_array(r.referrer)
                    );
                    println!(
                        "commission:    {:.4} pending | {:.4} lifetime (to your referral chain)",
                        r.pending_commission as f64 / ONE_TOKEN as f64,
                        r.total_commission as f64 / ONE_TOKEN as f64
                    );
                    println!(
                        "burned:        {:.4} lifetime (unfilled referral levels)",
                        r.total_burned as f64 / ONE_TOKEN as f64
                    );
                }
                None => println!(
                    "referral:      none (`miner set-referrer <pubkey>` = +{}% token weight)",
                    REFERRAL_BONUS_BPS / 100
                ),
            }
            match ctx.read::<RefName>(&pda::refname_owner_pda(&me).0) {
                Some(n) => println!(
                    "your ref link: https://miner.tools/?ref={}",
                    n.name_str()
                ),
                None => println!("your ref link: https://miner.tools/?ref={me}"),
            }
            if let Some(l) = ctx.read::<Lock>(&pda::lock_pda(&me).0) {
                use std::time::{SystemTime, UNIX_EPOCH};
                let now =
                    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
                if now < l.unlock_ts {
                    println!(
                        "lock:          {:.4} tokens at {:.1}x weight | unlocks in ~{:.1} d",
                        l.amount as f64 / ONE_TOKEN as f64,
                        l.multiplier_bps as f64 / BPS_DENOM as f64,
                        (l.unlock_ts - now) as f64 / 86_400.0
                    );
                } else {
                    println!(
                        "lock:          {:.4} tokens EXPIRED (1x weight), run `miner unlock`",
                        l.amount as f64 / ONE_TOKEN as f64
                    );
                }
            }
        }
        None => println!("miner: not registered (happens automatically on `miner mine`)"),
    }
}

/// Mining loop: register -> grind -> submit once per round.
fn cmd_mine(ctx: &Ctx) {
    let me = ctx.payer.pubkey();
    let config = ctx.config();
    let mint = Pubkey::new_from_array(config.mint);
    let miner_pda = pda::miner_pda(&me).0;

    if ctx.read::<Miner>(&miner_pda).is_none() {
        println!("Registering miner…");
        ctx.send(&[sdk::register(me)], &[]).expect("register");
    }

    // Referral enrollment is immutable, one read at startup is enough;
    // refreshed on a ReferralAccountRequired error (enrolled mid-run).
    let mut enrolled = ctx.read::<Referral>(&pda::referral_pda(&me).0).is_some();
    if enrolled {
        println!("Referral active: +{}% token weight", REFERRAL_BONUS_BPS / 100);
    }

    println!("Mining as {me} (Ctrl+C to stop)");
    loop {
        let config = ctx.config();
        let miner: Miner = ctx.read(&miner_pda).expect("miner account disappeared?");

        if miner.last_round == config.current_round && miner.last_round_weight > 0 {
            sleep(Duration::from_secs(2));
            continue; // waiting for the crank / next round
        }

        // Grind a nonce.
        let mut nonce = rand_seed();
        loop {
            if sdk::hash_meets_difficulty(&miner.challenge, &me, nonce, config.min_difficulty) {
                break;
            }
            nonce = nonce.wrapping_add(1);
        }

        let ix = if enrolled {
            sdk::mine_with_referral(me, me, mint, config.current_round, miner.last_round, nonce)
        } else {
            sdk::mine(me, me, mint, config.current_round, miner.last_round, nonce)
        };
        // The lock PDA rides along unconditionally: the program ignores it
        // while empty and counts the locked tokens once a lock exists.
        let ix = sdk::with_lock(ix, &me);
        match ctx.send(&[ix], &[]) {
            Ok(()) => {
                let m: Miner = ctx.read(&miner_pda).unwrap();
                println!(
                    "round #{:<6} weight {:>10.2} | pending {:.4} tokens",
                    config.current_round,
                    m.last_round_weight as f64 / ONE_TOKEN as f64,
                    m.pending_rewards as f64 / ONE_TOKEN as f64
                );
            }
            Err(e) => {
                // 0xe = ReferralAccountRequired: enrolled mid-run, switch over.
                if e.contains("0xe") {
                    enrolled = true;
                    println!("  referral enrollment detected, adding the referral account");
                    continue;
                }
                // The round may have rolled over mid-submit; just retry.
                println!("  submit failed ({}), retrying…", short_err(&e));
                sleep(Duration::from_secs(2));
            }
        }
    }
}

/// Permissionless crank: rolls rounds over and cleans up old accounts.
/// Sleeps until the end of the current round (per chain clock) instead of
/// polling every few seconds, which saves public RPC rate limits.
fn cmd_crank(ctx: &Ctx) {
    use std::time::{SystemTime, UNIX_EPOCH};
    println!("Crank bot (Ctrl+C to stop)");
    loop {
        let config = ctx.config();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let round_end = config.round_start_ts + config.round_seconds as i64;
        let remaining = round_end - now;
        if remaining > 1 {
            sleep(Duration::from_secs((remaining - 1) as u64));
            continue;
        }

        let next = config.current_round + 1;
        // Cleanup in the SAME transaction as the crank: round `next - RETENTION`
        // becomes closable exactly when current_round is bumped, so bundling
        // both instructions atomically wins any race for the rent refund (a
        // separate close after the crank lost to a sniper that was skimming
        // ~1.7 SOL per day). The close is added only when the round account
        // exists, so its absence never blocks opening the new round.
        let mut ixs = vec![sdk::crank(ctx.payer.pubkey(), next)];
        if let Some(old) = next.checked_sub(ROUND_RETENTION) {
            if ctx.read::<Round>(&pda::round_pda(old).0).is_some() {
                ixs.push(sdk::close_round(ctx.payer.pubkey(), old));
            }
        }
        match ctx.send(&ixs, &[]) {
            Ok(()) => println!("round #{next} opened"),
            Err(e) if e.contains("0x6") => sleep(Duration::from_secs(1)), /* round still open */
            Err(e) => {
                println!("  crank error: {}", short_err(&e));
                sleep(Duration::from_secs(5));
            }
        }
    }
}

fn cmd_claim(ctx: &Ctx) {
    let me = ctx.payer.pubkey();
    let config = ctx.config();
    let mint = Pubkey::new_from_array(config.mint);
    let miner: Miner = ctx
        .read(&pda::miner_pda(&me).0)
        .expect("miner not registered");
    // An enrolled miner's claim carries the full referral chain (the program
    // proves its length on-chain and distributes the 5/3/1 commission pool).
    let claim_ix = match ctx.read::<Referral>(&pda::referral_pda(&me).0) {
        Some(r) => sdk::claim_with_referral(me, mint, miner.last_round, &referral_chain(ctx, &r)),
        None => sdk::claim(me, mint, miner.last_round),
    };
    let ixs = vec![ix_create_ata_idempotent(&me, &me, &mint), claim_ix];
    ctx.send(&ixs, &[]).expect("claim failed");
    let balance = ctx
        .rpc
        .get_token_account_balance(&pda::ata(&me, &mint))
        .map(|b| b.ui_amount_string)
        .unwrap_or_default();
    println!("OK: token balance {balance}");
}

/// Walks the on-chain referral chain up from the claimer: level 1 is the
/// direct referrer, then their referrer, up to REFERRAL_LEVEL_BPS levels.
fn referral_chain(ctx: &Ctx, referral: &Referral) -> Vec<Pubkey> {
    let mut chain = vec![Pubkey::new_from_array(referral.referrer)];
    while chain.len() < REFERRAL_LEVEL_BPS.len() {
        match ctx.read::<Referral>(&pda::referral_pda(chain.last().unwrap()).0) {
            Some(next) => chain.push(Pubkey::new_from_array(next.referrer)),
            None => break,
        }
    }
    chain
}

/// Resolves a referrer given as either a wallet address or a custom
/// referral name (on-chain "refname" registry).
fn resolve_referrer(ctx: &Ctx, arg: &str) -> Pubkey {
    if let Ok(key) = arg.parse::<Pubkey>() {
        return key;
    }
    let name = arg.to_lowercase();
    let refname: RefName = ctx
        .read(&pda::refname_pda(name.as_bytes()).0)
        .unwrap_or_else(|| panic!("referral name '{name}' not found on-chain"));
    Pubkey::new_from_array(refname.owner)
}

/// Referral enrollment (once, immutable): +15% token weight for this miner,
/// 5/3/1% of its token-slice rewards to the chain of referrers. The referrer
/// must be a registered miner. Registers the caller first if needed.
fn cmd_set_referrer(ctx: &Ctx, args: &[String]) {
    if args.len() != 1 {
        eprintln!("usage: miner set-referrer <name_or_pubkey>");
        std::process::exit(1);
    }
    let referrer = resolve_referrer(ctx, &args[0]);
    let me = ctx.payer.pubkey();
    if ctx.read::<Referral>(&pda::referral_pda(&me).0).is_some() {
        println!("Referral already set (enrollment is immutable).");
        return;
    }

    let mut ixs = Vec::new();
    if ctx.read::<Miner>(&pda::miner_pda(&me).0).is_none() {
        println!("Registering miner…");
        ixs.push(sdk::register(me));
    }
    ixs.push(sdk::set_referrer(me, referrer));
    ctx.send(&ixs, &[]).expect("set_referrer failed");
    println!(
        "OK: referral active, +{}% token weight; referrer {} earns {}% of your token rewards (their chain {}% and {}%)",
        REFERRAL_BONUS_BPS / 100,
        referrer,
        REFERRAL_LEVEL_BPS[0] / 100,
        REFERRAL_LEVEL_BPS[1] / 100,
        REFERRAL_LEVEL_BPS[2] / 100
    );
}

/// Claim a custom referral name (once, immutable, first come first served):
/// links like miner.tools/?ref=<name> resolve to this wallet.
fn cmd_set_refname(ctx: &Ctx, args: &[String]) {
    if args.len() != 1 {
        eprintln!("usage: miner set-refname <name>  (3-16 chars, a-z 0-9 _)");
        std::process::exit(1);
    }
    let name = args[0].to_lowercase();
    let me = ctx.payer.pubkey();
    if let Some(existing) = ctx.read::<RefName>(&pda::refname_owner_pda(&me).0) {
        println!("Name already claimed: '{}' (one per miner).", existing.name_str());
        return;
    }
    if let Some(taken) = ctx.read::<RefName>(&pda::refname_pda(name.as_bytes()).0) {
        println!(
            "Name '{name}' is taken by {}.",
            Pubkey::new_from_array(taken.owner)
        );
        return;
    }
    ctx.send(&[sdk::set_refname(me, &name)], &[])
        .expect("set_refname failed");
    println!("OK: your referral link is https://miner.tools/?ref={name}");
}

/// Lock-to-boost: locks tokens in the program vault for a weight
/// multiplier. Topping up re-locks everything under the newly chosen tier.
fn cmd_lock(ctx: &Ctx, args: &[String]) {
    if args.len() != 2 {
        eprintln!("usage: miner lock <amount_tokens> <days>  (7 = 1.2x, 30 = 1.5x, 90 = 2.0x)");
        std::process::exit(1);
    }
    let amount_tokens: f64 = args[0].parse().expect("amount: token amount");
    let days: i64 = args[1].parse().expect("days: number");
    let duration = days * 86_400;
    let Some(multiplier) = lock_multiplier_bps(duration) else {
        let tiers: Vec<String> = LOCK_TIERS
            .iter()
            .map(|(d, m)| format!("{} d = {:.1}x", d / 86_400, *m as f64 / BPS_DENOM as f64))
            .collect();
        eprintln!("invalid tier; available: {}", tiers.join(", "));
        std::process::exit(1);
    };
    let amount = (amount_tokens * ONE_TOKEN as f64) as u64;
    let me = ctx.payer.pubkey();
    let config = ctx.config();
    let mint = Pubkey::new_from_array(config.mint);
    let (lock_key, _) = pda::lock_pda(&me);

    // The vault (the lock PDA's ATA) rides in the same transaction.
    let ixs = vec![
        ix_create_ata_idempotent(&me, &lock_key, &mint),
        sdk::lock(me, mint, amount, duration),
    ];
    ctx.send(&ixs, &[]).expect("lock failed");
    let lock: Lock = ctx.read(&pda::lock_pda(&me).0).expect("lock account");
    println!(
        "OK: {:.4} tokens locked at {:.1}x weight for {} days (total {:.4})",
        amount_tokens,
        multiplier as f64 / BPS_DENOM as f64,
        days,
        lock.amount as f64 / ONE_TOKEN as f64
    );
}

/// Withdraws an expired lock (tokens + both rents back to the wallet).
fn cmd_unlock(ctx: &Ctx) {
    let me = ctx.payer.pubkey();
    let config = ctx.config();
    let mint = Pubkey::new_from_array(config.mint);
    let ixs = vec![
        ix_create_ata_idempotent(&me, &me, &mint),
        sdk::unlock(me, mint),
    ];
    ctx.send(&ixs, &[]).expect("unlock failed");
    let balance = ctx
        .rpc
        .get_token_account_balance(&pda::ata(&me, &mint))
        .map(|b| b.ui_amount_string)
        .unwrap_or_default();
    println!("OK: lock withdrawn, token balance {balance}");
}

/// Admin: change parameters (difficulty, base weight, round length).
fn cmd_update_config(ctx: &Ctx, args: &[String]) {
    if args.len() != 3 {
        eprintln!("usage: miner update-config <min_difficulty> <base_weight_tokens> <round_seconds>");
        std::process::exit(1);
    }
    let min_difficulty: u64 = args[0].parse().expect("min_difficulty: number");
    let base_weight_tokens: u64 = args[1].parse().expect("base_weight: token amount");
    let round_seconds: u64 = args[2].parse().expect("round_seconds: number");

    let ix = sdk::update_config(
        ctx.payer.pubkey(),
        min_difficulty,
        base_weight_tokens * ONE_TOKEN,
        round_seconds,
    );
    ctx.send(&[ix], &[]).expect("update_config failed");
    let config = ctx.config();
    println!(
        "OK: difficulty {} bits | base weight {} tokens | round {} s",
        config.min_difficulty,
        config.base_weight / ONE_TOKEN,
        config.round_seconds
    );
}

/// Test/recovery helper: creates metadata for a mint whose authority is
/// still held by the payer.
/// Usage: miner create-metadata <mint> [update_authority]
fn cmd_create_metadata(ctx: &Ctx, args: &[String]) {
    let Some(mint_arg) = args.first() else {
        eprintln!("usage: miner create-metadata <mint> [update_authority]");
        std::process::exit(1);
    };
    let mint: Pubkey = mint_arg.parse().expect("mint: valid pubkey");
    let payer = ctx.payer.pubkey();
    let update_authority: Pubkey = args
        .get(1)
        .map(|a| a.parse().expect("update authority: valid pubkey"))
        .unwrap_or(payer);

    let ix = ix_create_metadata_v3(&mint, &payer, &payer, &update_authority);
    ctx.send(&[ix], &[]).expect("create_metadata failed");
    println!("OK: metadata {TOKEN_NAME} ({TOKEN_SYMBOL}) for {mint}");
}

/// Hand the admin role over (e.g. to a Squads multisig vault).
fn cmd_set_admin(ctx: &Ctx, args: &[String]) {
    if args.len() != 1 {
        eprintln!("usage: miner set-admin <new_admin_pubkey>");
        std::process::exit(1);
    }
    let new_admin: Pubkey = args[0].parse().expect("new admin: valid pubkey");

    let ix = sdk::set_admin(ctx.payer.pubkey(), new_admin);
    ctx.send(&[ix], &[]).expect("set_admin failed");
    let config = ctx.config();
    println!("OK: admin {}", Pubkey::new_from_array(config.admin));
}

// ---------- helpers ----------

fn rand_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
}

fn short_err(e: &str) -> String {
    e.chars().take(120).collect()
}

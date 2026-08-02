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
        "update-config" => cmd_update_config(&ctx, &args[2..]),
        "set-admin" => cmd_set_admin(&ctx, &args[2..]),
        _ => {
            eprintln!("usage: miner <init|status|mine|crank|claim|update-config|set-admin>");
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

        let ix = sdk::mine(me, me, mint, config.current_round, miner.last_round, nonce);
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
    let ixs = vec![
        ix_create_ata_idempotent(&me, &me, &mint),
        sdk::claim(me, mint, miner.last_round),
    ];
    ctx.send(&ixs, &[]).expect("claim failed");
    let balance = ctx
        .rpc
        .get_token_account_balance(&pda::ata(&me, &mint))
        .map(|b| b.ui_amount_string)
        .unwrap_or_default();
    println!("OK: token balance {balance}");
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

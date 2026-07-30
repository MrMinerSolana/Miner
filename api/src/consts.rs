use solana_program::{pubkey, pubkey::Pubkey};

/// Token decimal places.
pub const TOKEN_DECIMALS: u8 = 9;

/// One token in native units.
pub const ONE_TOKEN: u64 = 10u64.pow(TOKEN_DECIMALS as u32);

/// Emission: 10 tokens per minute.
pub const EMISSION_PER_MINUTE: u64 = 10 * ONE_TOKEN;

/// Initial round length in seconds (lives in Config, tunable via
/// update_config, from an "ORE feel" cadence to a frugal one).
pub const INITIAL_ROUND_SECONDS: u64 = 60;

/// Round length bounds for update_config.
pub const MIN_ROUND_SECONDS: u64 = 10;
pub const MAX_ROUND_SECONDS: u64 = 3600;

/// Round budget for a given length: 10 tokens/min pro-rata.
/// A round with no submissions = the emission lapses (nothing is minted).
pub const fn round_budget(round_seconds: u64) -> u64 {
    EMISSION_PER_MINUTE * round_seconds / 60
}

/// How many rounds back Round accounts are kept before they can be closed
/// (rent recovery: the crank closes old rounds and recycles the float).
/// 2880 rounds × 60 s = 2 days to settle a reward at launch settings.
pub const ROUND_RETENTION: u64 = 2880;

/// Initial minimum hash difficulty (leading zero bits).
/// 20 bits ≈ 1M hashes: a few seconds on a desktop, a dozen or so on a
/// weak CPU. Proof-of-liveness is meant to take a moment, not be a race.
pub const INITIAL_MIN_DIFFICULTY: u64 = 20;

/// Initial base weight (free tier) in native token units.
/// Denominated purely in tokens: each 1 token of balance = +1% power over
/// the base (100 tokens = 2×). Tuned via update_config.
pub const INITIAL_BASE_WEIGHT: u64 = 100 * ONE_TOKEN;

/// PDA seeds.
pub const CONFIG_SEED: &[u8] = b"config";
pub const TREASURY_SEED: &[u8] = b"treasury";
pub const ROUND_SEED: &[u8] = b"round";
pub const MINER_SEED: &[u8] = b"miner";

/// External programs (canonical addresses).
pub const SPL_TOKEN_PROGRAM_ID: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
pub const SYSTEM_PROGRAM_ID: Pubkey = pubkey!("11111111111111111111111111111111");
pub const SLOT_HASHES_SYSVAR_ID: Pubkey =
    pubkey!("SysvarS1otHashes111111111111111111111111111");

/// MintTo instruction index in the SPL Token program.
pub const SPL_TOKEN_MINT_TO_IX: u8 = 7;

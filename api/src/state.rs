use bytemuck::{Pod, Zeroable};

/// Account discriminators.
pub const CONFIG_DISCRIMINATOR: u64 = 1;
pub const ROUND_DISCRIMINATOR: u64 = 2;
pub const MINER_DISCRIMINATOR: u64 = 3;

/// Global program configuration ("config" PDA, singleton).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Config {
    pub discriminator: u64,
    /// Admin (update_config / set_admin). Held by a multisig with a timelock.
    pub admin: [u8; 32],
    /// Token mint (mint authority = "treasury" PDA).
    pub mint: [u8; 32],
    /// Index of the current (open) round.
    pub current_round: u64,
    /// Start timestamp of the current round.
    pub round_start_ts: i64,
    /// Minimum hash difficulty (leading zero bits).
    pub min_difficulty: u64,
    /// Free-tier base weight in native token units.
    pub base_weight: u64,
    /// Round length in seconds (submit cadence; the round budget scales
    /// pro-rata, so emission per minute stays constant).
    pub round_seconds: u64,
    /// Config PDA bump.
    pub config_bump: u64,
    /// Treasury PDA bump.
    pub treasury_bump: u64,
}

/// One distribution round ("round" PDA + LE index).
/// A round is open when index == config.current_round; settleable when
/// index < config.current_round; closable (rent recovery) after
/// ROUND_RETENTION.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Round {
    pub discriminator: u64,
    pub index: u64,
    /// Sum of all submit weights in the round (native token units).
    pub total_weight: u64,
    /// Round open timestamp.
    pub start_ts: i64,
    /// Round emission budget (frozen at open from config.round_seconds so
    /// a later cadence change never touches old rounds' settlements).
    pub budget: u64,
}

/// Miner account ("miner" PDA + authority).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Miner {
    pub discriminator: u64,
    /// Owner wallet (a browser burner or a connected wallet).
    pub authority: [u8; 32],
    /// Session key allowed to submit (zeros = none).
    pub session_key: [u8; 32],
    /// Current PoW challenge.
    pub challenge: [u8; 32],
    /// Round index of the last submit.
    pub last_round: u64,
    /// Weight submitted in round last_round (unsettled if > 0).
    pub last_round_weight: u64,
    /// Token balance at the last submit (min-balance / anti-cycling rule).
    pub last_balance: u64,
    /// Accrued, unclaimed rewards (native units).
    pub pending_rewards: u64,
    /// Lifetime stats.
    pub total_mined: u64,
    pub total_hashes: u64,
    /// PDA bump.
    pub bump: u64,
}

impl Config {
    pub const SIZE: usize = core::mem::size_of::<Config>();
}
impl Round {
    pub const SIZE: usize = core::mem::size_of::<Round>();
}
impl Miner {
    pub const SIZE: usize = core::mem::size_of::<Miner>();
}

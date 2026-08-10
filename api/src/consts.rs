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

/// Halving schedule. The round budget is cut in half every HALVING_SECONDS,
/// with epochs counted from HALVING_ANCHOR_TS on the wall clock (Bitcoin's
/// block-height schedule works the same way: late blocks lapse from the
/// schedule, exactly like our empty rounds). The geometric series bounds
/// all future emission to 2 * daily emission * period, which pins the max
/// supply without any on-chain supply tracking:
///   max supply = supply at anchor + 2 * 14_400 * period_days < 3.0M.
/// Finalized for the 2026-08-04 deploy: the anchor is that day's (already
/// past) midnight UTC, when the minted supply was <= 1,066,042, so the cap
/// is bounded by 1,066,042 + 2 * 14_400 * 67 = 2,995,642. Real supply will
/// land lower still: empty rounds, unfilled referral levels and rewards
/// unclaimed past retention all burn from the schedule.
#[cfg(not(feature = "short-halving"))]
pub const HALVING_ANCHOR_TS: i64 = 1_785_801_600; // 2026-08-04 00:00:00 UTC
#[cfg(not(feature = "short-halving"))]
pub const HALVING_SECONDS: i64 = 67 * 86_400;

/// Devnet rehearsal (`--features short-halving`): the same code path with a
/// compressed schedule (halving every 5 minutes) so the halvings can be
/// watched live on a real cluster. Never part of a mainnet artifact.
#[cfg(feature = "short-halving")]
pub const HALVING_ANCHOR_TS: i64 = 1_785_854_400; // 2026-08-04 14:40:00 UTC
#[cfg(feature = "short-halving")]
pub const HALVING_SECONDS: i64 = 300;

/// Round budget for a round opened at unix time `now`: the base pro-rata
/// budget halved once per elapsed halving epoch. Before the anchor the
/// budget is the plain base. checked_shr guards the shift: a u64 shift by
/// >= 64 would wrap in release builds and "revive" the emission; instead
/// the budget pins to zero (it naturally reaches zero after ~34 halvings).
pub const fn round_budget_at(round_seconds: u64, now: i64) -> u64 {
    let base = round_budget(round_seconds);
    if now <= HALVING_ANCHOR_TS {
        return base;
    }
    let epoch = ((now - HALVING_ANCHOR_TS) / HALVING_SECONDS) as u32;
    match base.checked_shr(epoch) {
        Some(b) => b,
        None => 0,
    }
}

#[cfg(test)]
mod halving_tests {
    use super::*;

    const BASE: u64 = round_budget(60);

    /// Epoch boundaries are exact: the budget switches at the very second
    /// of anchor + k * HALVING_SECONDS, never one second early or late.
    #[test]
    fn boundaries() {
        assert_eq!(round_budget_at(60, 0), BASE);
        assert_eq!(round_budget_at(60, HALVING_ANCHOR_TS), BASE);
        assert_eq!(
            round_budget_at(60, HALVING_ANCHOR_TS + HALVING_SECONDS - 1),
            BASE
        );
        assert_eq!(
            round_budget_at(60, HALVING_ANCHOR_TS + HALVING_SECONDS),
            BASE / 2
        );
        assert_eq!(
            round_budget_at(60, HALVING_ANCHOR_TS + 2 * HALVING_SECONDS - 1),
            BASE / 2
        );
        assert_eq!(
            round_budget_at(60, HALVING_ANCHOR_TS + 2 * HALVING_SECONDS),
            BASE / 4
        );
        assert_eq!(
            round_budget_at(60, HALVING_ANCHOR_TS + 10 * HALVING_SECONDS),
            BASE >> 10
        );
    }

    /// The budget reaches zero naturally (600e9 native units shift out
    /// after 40 halvings) and STAYS zero past epoch 64, where an unguarded
    /// u64 shift would wrap and "revive" the emission.
    #[test]
    fn reaches_zero_and_stays() {
        assert_eq!(round_budget_at(60, HALVING_ANCHOR_TS + 40 * HALVING_SECONDS), 0);
        assert_eq!(round_budget_at(60, HALVING_ANCHOR_TS + 63 * HALVING_SECONDS), 0);
        assert_eq!(round_budget_at(60, HALVING_ANCHOR_TS + 64 * HALVING_SECONDS), 0);
        assert_eq!(round_budget_at(60, HALVING_ANCHOR_TS + 500 * HALVING_SECONDS), 0);
    }

    /// All emission after the anchor is bounded by the geometric series:
    /// sum over epochs of (per-epoch emission) <= 2 * base epoch emission.
    #[test]
    fn tail_emission_bounded() {
        let epoch_emission = |e: u32| {
            (round_budget_at(60, HALVING_ANCHOR_TS + e as i64 * HALVING_SECONDS) as u128)
                * (HALVING_SECONDS as u128)
                / 60
        };
        let total: u128 = (0..128).map(epoch_emission).sum();
        let bound = 2 * (EMISSION_PER_MINUTE as u128) * (HALVING_SECONDS as u128) / 60;
        assert!(total <= bound, "total {total} > bound {bound}");
    }

    /// The budget scales pro-rata with the round length inside an epoch.
    #[test]
    fn pro_rata_round_length() {
        let ts = HALVING_ANCHOR_TS + 3 * HALVING_SECONDS;
        assert_eq!(round_budget_at(15, ts) * 4, round_budget_at(60, ts));
    }
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

/// Referral program, in basis points. The referee's token-derived weight is
/// boosted by REFERRAL_BONUS_BPS. The reward slice earned by that (boosted)
/// token weight is charged a three-level commission ladder: level 1 is the
/// direct inviter, level 2 their inviter, level 3 the next one up. At
/// settlement the full ladder total is carved into a pool; at claim the pool
/// is split by level, and the shares of levels that do not exist (short
/// chain, cycles) are BURNED, never minted, so the carve is flat for every
/// enrolled miner regardless of chain depth. The base weight slice is never
/// charged, so enrolling stays net-positive at any balance as long as the
/// compile-time guard below holds: (1 + bonus) * (1 - ladder total) > 1.
pub const REFERRAL_BONUS_BPS: u64 = 1500;
pub const REFERRAL_LEVEL_BPS: [u64; 3] = [500, 300, 100];
pub const REFERRAL_TOTAL_BPS: u64 =
    REFERRAL_LEVEL_BPS[0] + REFERRAL_LEVEL_BPS[1] + REFERRAL_LEVEL_BPS[2];
pub const BPS_DENOM: u64 = 10_000;
const _REFEREE_NET_POSITIVE: () = assert!(
    (BPS_DENOM + REFERRAL_BONUS_BPS) * (BPS_DENOM - REFERRAL_TOTAL_BPS)
        > BPS_DENOM * BPS_DENOM
);

/// Flag stored in the high bits of Miner.bump (the PDA seed byte itself is
/// the low byte, so `bump as u8` stays correct everywhere). When set, the
/// miner enrolled in the referral program and mine/claim must be given the
/// Referral account, otherwise commissions could be skipped.
pub const MINER_FLAG_REFERRAL: u64 = 1 << 8;

/// Lock-to-boost: tokens locked in a program vault (the "lock" PDA's ATA)
/// count toward the mining weight with a multiplier while the lock is
/// active. Tiers: (duration in seconds, weight multiplier in bps); the
/// duration sent to Lock must match a tier exactly. An expired lock that
/// has not been withdrawn counts at 1x, like a plain wallet balance.
/// Locked tokens physically cannot move between wallets, so they are
/// exempt from the min-balance (anti-cycling) rule by construction, and
/// they count from the very next submit.
#[cfg(not(feature = "short-lock"))]
pub const LOCK_TIERS: [(i64, u64); 3] = [
    (7 * 86_400, 12_000),  // 7 days  -> 1.2x
    (30 * 86_400, 15_000), // 30 days -> 1.5x
    (90 * 86_400, 20_000), // 90 days -> 2.0x
];

/// Devnet rehearsal (`--features short-lock`): the same code path with
/// compressed tiers (minutes instead of days) so a full lock -> mine ->
/// expire -> withdraw cycle can be watched live on a real cluster.
/// Never part of a mainnet artifact.
#[cfg(feature = "short-lock")]
pub const LOCK_TIERS: [(i64, u64); 3] = [
    (60, 12_000),  // 1 minute  -> 1.2x
    (120, 15_000), // 2 minutes -> 1.5x
    (180, 20_000), // 3 minutes -> 2.0x
];

/// Multiplier for an exact tier duration (None = not a valid tier).
pub const fn lock_multiplier_bps(duration_secs: i64) -> Option<u64> {
    let mut i = 0;
    while i < LOCK_TIERS.len() {
        if LOCK_TIERS[i].0 == duration_secs {
            return Some(LOCK_TIERS[i].1);
        }
        i += 1;
    }
    None
}

/// Motherlode: the protocol strike reward. Every round carves
/// MOTHERLODE_BPS of its budget into a pool (only rounds that actually had
/// miners; empty rounds keep burning everything). Every mine instruction
/// is one chance at the strike for that round; MOTHERLODE_WINNERS
/// independent candidate slots are each kept with reservoir sampling
/// (every hash ends up with an equal 1/n chance per slot, O(1) state).
/// When the crank closes a round, the strike hits with 1/MOTHERLODE_ODDS
/// probability: the pool splits evenly across the candidates' Win accounts
/// (the same wallet may hold more than one slot and simply gets a bigger
/// cut), claimable any time; the pool restarts and later strikes keep
/// running. At claim MOTHERLODE_BURN_BPS of the win is minted to the
/// treasury ATA and burned in the same instruction (a real, visible Burn),
/// the rest goes to the winner.
pub const MOTHERLODE_BPS: u64 = 1000; // 10% of every round budget
pub const MOTHERLODE_BURN_BPS: u64 = 2000; // 20% of a win burns at claim

/// How many candidate slots a strike pays out to (even split of the pool;
/// the division remainder goes to the first slot as dust).
pub const MOTHERLODE_WINNERS: usize = 3;

/// Strike chance: 1 in N round closes. 1440 rounds = on average one
/// strike every day at 60 s rounds.
#[cfg(not(feature = "short-motherlode"))]
pub const MOTHERLODE_ODDS: u64 = 1440;

/// Devnet rehearsal (`--features short-motherlode`): every round close
/// strikes, so the accrue -> win -> claim -> burn cycle can be watched
/// live. Never part of a mainnet artifact.
#[cfg(feature = "short-motherlode")]
pub const MOTHERLODE_ODDS: u64 = 1;

/// The miners' cut of a full round budget: what Round.budget stores. The
/// Motherlode share is withheld at round open and credited to the pool
/// when the round closes with any weight (empty rounds keep burning
/// everything).
pub const fn miners_budget(full_budget: u64) -> u64 {
    ((full_budget as u128) * ((BPS_DENOM - MOTHERLODE_BPS) as u128) / (BPS_DENOM as u128))
        as u64
}

/// The Motherlode share recovered from a stored (miners') budget at round
/// close: the withheld remainder of the same full budget.
pub const fn motherlode_tithe(stored_budget: u64) -> u64 {
    ((stored_budget as u128) * (MOTHERLODE_BPS as u128)
        / ((BPS_DENOM - MOTHERLODE_BPS) as u128)) as u64
}

/// Mining fee in lamports, charged per mine instruction and sent to the
/// fee wallet (the crank/ops wallet). Levels the playing field between
/// batched farms and individual miners (a batch pays per instruction) and
/// funds the daily buyback-and-burn of $MINER.
pub const MINE_FEE_LAMPORTS: u64 = 5000;

/// Fee destination: the ops (crank) wallet. A daily job swaps the fees
/// accumulated that day (tracked by Motherlode.total_fees) for $MINER and
/// burns them.
pub const FEE_WALLET: Pubkey = pubkey!("D9pvV1SQqYU8d2zcZHkhAdkzuZKaXntkKSNFLsvQvxxu");

/// Custom referral name (vanity code) length bounds. Charset: a-z 0-9 _
/// (lowercase only, clients normalize before sending). First come, first
/// served; one name per miner; immutable. Rent makes mass squatting cost
/// real SOL.
pub const REFNAME_MIN_LEN: usize = 3;
pub const REFNAME_MAX_LEN: usize = 16;

/// PDA seeds.
pub const CONFIG_SEED: &[u8] = b"config";
pub const TREASURY_SEED: &[u8] = b"treasury";
pub const ROUND_SEED: &[u8] = b"round";
pub const MINER_SEED: &[u8] = b"miner";
pub const REFERRAL_SEED: &[u8] = b"referral";
pub const LOCK_SEED: &[u8] = b"lock";
pub const MOTHERLODE_SEED: &[u8] = b"motherlode";
pub const WIN_SEED: &[u8] = b"win";
/// name -> owner (uniqueness + resolution of ?ref=<name> links).
pub const REFNAME_SEED: &[u8] = b"refname";
/// owner -> name (reverse lookup for UI + enforces one name per miner).
pub const REFNAME_OWNER_SEED: &[u8] = b"refname_owner";

/// External programs (canonical addresses).
pub const SPL_TOKEN_PROGRAM_ID: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
pub const SYSTEM_PROGRAM_ID: Pubkey = pubkey!("11111111111111111111111111111111");
pub const SLOT_HASHES_SYSVAR_ID: Pubkey =
    pubkey!("SysvarS1otHashes111111111111111111111111111");

/// MintTo instruction index in the SPL Token program.
pub const SPL_TOKEN_MINT_TO_IX: u8 = 7;

/// Transfer instruction index in the SPL Token program.
pub const SPL_TOKEN_TRANSFER_IX: u8 = 3;

/// Burn instruction index in the SPL Token program.
pub const SPL_TOKEN_BURN_IX: u8 = 8;

/// CloseAccount instruction index in the SPL Token program.
pub const SPL_TOKEN_CLOSE_ACCOUNT_IX: u8 = 9;

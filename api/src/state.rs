use bytemuck::{Pod, Zeroable};

use crate::consts::{GAME_MOTHERLODE_WINNERS, GAME_TUNNELS, MOTHERLODE_WINNERS};

/// Account discriminators.
pub const CONFIG_DISCRIMINATOR: u64 = 1;
pub const ROUND_DISCRIMINATOR: u64 = 2;
pub const MINER_DISCRIMINATOR: u64 = 3;
pub const REFERRAL_DISCRIMINATOR: u64 = 4;
pub const REFNAME_DISCRIMINATOR: u64 = 5;
pub const LOCK_DISCRIMINATOR: u64 = 6;
pub const MOTHERLODE_DISCRIMINATOR: u64 = 7;
pub const WIN_DISCRIMINATOR: u64 = 8;
pub const GAME_DISCRIMINATOR: u64 = 9;
pub const GAME_ROUND_DISCRIMINATOR: u64 = 10;
pub const GAME_ENTRY_DISCRIMINATOR: u64 = 11;
pub const GAME_WIN_DISCRIMINATOR: u64 = 12;
pub const TICKET_STATE_DISCRIMINATOR: u64 = 13;
pub const TICKET_BATCH_DISCRIMINATOR: u64 = 14;

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

/// Referral enrollment ("referral" PDA + authority), created by SetReferrer.
/// Exists only for enrolled miners; the Miner account itself is unchanged
/// (no migration), it just carries the MINER_FLAG_REFERRAL bit in `bump`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Referral {
    pub discriminator: u64,
    /// The enrolled miner (matches Miner.authority).
    pub authority: [u8; 32],
    /// Referrer wallet; commissions land in their Miner.pending_rewards.
    /// Immutable after enrollment.
    pub referrer: [u8; 32],
    /// Boosted token weight of the last submit (the commission base for the
    /// lazy settlement; zeroed together with Miner.last_round_weight).
    pub last_token_weight: u64,
    /// Commission pool carved at settlement (the full ladder total),
    /// distributed across the referral chain at the referee's next claim.
    /// The carve is flat for every enrolled miner regardless of chain
    /// depth; the shares of missing levels are burned (never minted).
    pub pending_commission: u64,
    /// Lifetime commission actually delivered up the chain (stats).
    pub total_commission: u64,
    /// Lifetime emission burned on this miner's claims: shares of levels
    /// that did not exist (short chain, cycles back to the claimer) plus
    /// rounding dust. Summed across all Referral accounts by the UI as the
    /// global "Burned $MINER from refs" counter.
    pub total_burned: u64,
    /// PDA bump.
    pub bump: u64,
}

/// Token lock ("lock" PDA + authority), created by Lock, closed by Unlock.
/// The locked tokens sit in the lock PDA's associated token account (the
/// per-user vault), so every user's tokens are isolated and only this PDA
/// can move them. While now < unlock_ts the amount counts toward the
/// mining weight multiplied by multiplier_bps; after expiry it counts at
/// 1x until withdrawn.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Lock {
    pub discriminator: u64,
    /// Owner wallet (matches Miner.authority).
    pub authority: [u8; 32],
    /// Locked amount in native units (mirrors the vault balance).
    pub amount: u64,
    /// Unix time when withdrawal opens. Topping up re-locks everything:
    /// the new now + duration must not come before the current value.
    pub unlock_ts: i64,
    /// Weight multiplier in bps while the lock is active (a LOCK_TIERS
    /// value; set from the tier chosen at the last Lock call).
    pub multiplier_bps: u64,
    /// PDA bump.
    pub bump: u64,
}

/// Motherlode: the protocol strike reward ("motherlode" PDA, singleton
/// created by InitMotherlode). Accrues MOTHERLODE_BPS of every non-empty
/// round budget as a counter (nothing is minted up front) and tracks the
/// current round's strike: every mine instruction is one chance, and each
/// of the MOTHERLODE_WINNERS candidate slots is maintained with its own
/// independent reservoir sample so every hash has an equal chance per
/// slot. The strike roll runs when the crank closes a round (see
/// crank.rs) and splits the pool evenly across the slots.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Motherlode {
    pub discriminator: u64,
    /// Accumulated pool in native units (minted only when a win is claimed).
    pub pot: u64,
    /// Round the hash counter below belongs to (reset on a new round).
    pub round_index: u64,
    /// Hashes submitted in round_index (= mine instructions so far).
    pub hashes: u64,
    /// Current winner candidates, one independent reservoir sample per
    /// slot. The same wallet may occupy several slots (it then simply
    /// receives several shares of the split).
    pub candidates: [[u8; 32]; MOTHERLODE_WINNERS],
    /// Lifetime $MINER burned from claimed wins (MOTHERLODE_BURN_BPS each),
    /// shown by the UI as part of the global burned counter.
    pub total_burned: u64,
    /// Lifetime mining fees routed to the fee wallet, in lamports. The
    /// daily buyback job swaps exactly the day's delta of this counter.
    pub total_fees: u64,
    /// The most recent strike (winner wallets, per-winner share, unix
    /// time), purely informational for the UI; zeroed until the first
    /// strike ever.
    pub last_winners: [[u8; 32]; MOTHERLODE_WINNERS],
    pub last_win_amount: u64,
    pub last_win_ts: i64,
    /// PDA bump.
    pub bump: u64,
}

/// A won, not-yet-claimed Motherlode strike ("win" PDA + authority),
/// created by the crank at the strike, closed by ClaimMotherlode (rent
/// goes to the winner). Wins accumulate: striking again before claiming
/// simply adds to the amount, and strikes never pause.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Win {
    pub discriminator: u64,
    /// The winning miner (matches Miner.authority).
    pub authority: [u8; 32],
    /// Amount waiting to be claimed, native units. At claim
    /// MOTHERLODE_BURN_BPS of it burns, the rest mints to the winner.
    pub amount: u64,
    /// Unix time of the (first unclaimed) strike, for the UI feed.
    pub since_ts: i64,
    /// PDA bump.
    pub bump: u64,
}

/// Custom referral name (vanity code), created by SetRefName. The same
/// struct backs BOTH directions of the mapping:
/// - "refname" PDA + name: uniqueness + resolving ?ref=<name> to a wallet,
/// - "refname_owner" PDA + owner: reverse lookup for UI, one name per miner.
/// Purely a pointer for clients; commissions always flow by wallet address.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct RefName {
    pub discriminator: u64,
    /// Wallet that owns the name (matches Miner.authority).
    pub owner: [u8; 32],
    /// The name itself: lowercase a-z 0-9 _, zero-padded to 32 bytes.
    pub name: [u8; 32],
    /// PDA bump.
    pub bump: u64,
}

impl RefName {
    /// The name as a str slice (trims the zero padding).
    pub fn name_str(&self) -> &str {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(32);
        core::str::from_utf8(&self.name[..len]).unwrap_or("")
    }
}

/// Tunnels game state ("game" PDA, singleton created by InitGame).
/// Holds the round cursor, the EMA price used to value $MINER stakes in
/// lamports, and the players' Motherlode pools (funds physically sit in
/// the game vaults; these are counters). Also the lifetime stats the UI
/// shows.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Game {
    pub discriminator: u64,
    /// Index of the current (open) game round.
    pub current_round: u64,
    /// Start timestamp of the current round.
    pub round_start_ts: i64,
    /// Round length in seconds (frozen from GAME_ROUND_SECONDS at init).
    pub round_seconds: u64,
    /// EMA of the $MINER price in lamports per whole token; values the
    /// $MINER side of stakes. Updated once per settle from the pool spot,
    /// clamped to GAME_EMA_CLAMP_BPS per update.
    pub ema_lamports_per_token: u64,
    /// The cp-amm pool the EMA reads (fixed at init).
    pub pool: [u8; 32],
    /// Players' Motherlode pools (lamports / native $MINER).
    pub ml_sol: u64,
    pub ml_miner: u64,
    /// The most recent players' strike, informational for the UI.
    pub ml_last_winners: [[u8; 32]; GAME_MOTHERLODE_WINNERS],
    pub ml_last_sol: u64,
    pub ml_last_miner: u64,
    pub ml_last_ts: i64,
    /// Lifetime stats: $MINER burned by the rake, SOL routed to the fee
    /// wallet (daily buyback), staked volume per asset, settled rounds
    /// with a collapse.
    pub total_burned: u64,
    pub total_fee_sol: u64,
    pub total_volume_sol: u64,
    pub total_volume_miner: u64,
    pub total_rounds_played: u64,
    /// PDA bumps (game + SOL vault).
    pub bump: u64,
    pub vault_bump: u64,
}

/// One game round ("game_round" PDA + LE index). Open while index ==
/// Game.current_round and now < start_ts + round_seconds; GameSettle then
/// rolls the collapse and freezes the payout numbers claims divide by.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct GameRound {
    pub discriminator: u64,
    pub index: u64,
    pub start_ts: i64,
    /// Stakes per tunnel, split by asset (lamports / native $MINER).
    pub sol: [u64; GAME_TUNNELS],
    pub miner: [u64; GAME_TUNNELS],
    /// Stake value per tunnel in lamport-equivalent (sol + miner * ema),
    /// frozen per entry at entry time (the EMA only moves between rounds).
    pub weight: [u64; GAME_TUNNELS],
    /// Wallets that entered (one per wallet; top-ups do not recount).
    pub entries: u64,
    /// Players' Motherlode candidates: independent reservoir samples over
    /// the round's wallets, one ticket per wallet regardless of stake.
    pub candidates: [[u8; 32]; GAME_MOTHERLODE_WINNERS],
    /// GAME_ROUND_OPEN / SETTLED / VOID.
    pub settled: u64,
    /// Bitmask of the collapsed tunnels (valid when settled == SETTLED).
    pub collapsed: u64,
    /// 90% of the collapsed pots, what survivors share pro-rata.
    pub payout_sol: u64,
    pub payout_miner: u64,
    /// Total weight of the surviving tunnels (the claim divisor).
    pub survivor_weight: u64,
}

/// A wallet's stake in one round ("game_entry" PDA + LE round + authority),
/// created by GameEnter (top-ups merge into it, same tunnel only), closed
/// by GameClaim (payout / refund / nothing for a collapsed stake; rent
/// back to the authority either way).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct GameEntry {
    pub discriminator: u64,
    pub authority: [u8; 32],
    pub round: u64,
    /// Stakes per tunnel: a wallet can spread one round's stake across
    /// any number of tunnels (the hedge is part of the game).
    pub sol: [u64; GAME_TUNNELS],
    pub miner: [u64; GAME_TUNNELS],
    /// Lamport-equivalent stake value per tunnel (claim numerators).
    pub weight: [u64; GAME_TUNNELS],
    pub bump: u64,
}

/// A won, not-yet-claimed players' Motherlode strike ("game_win" PDA +
/// authority), created by GameSettle at the strike, closed by GameClaimWin.
/// Striking again before claiming adds to the amounts.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct GameWin {
    pub discriminator: u64,
    pub authority: [u8; 32],
    pub sol: u64,
    pub miner: u64,
    pub since_ts: i64,
    pub bump: u64,
}

/// Motherlode ticket sale state ("tickets" PDA, singleton created by
/// InitTickets). A ticket costs TICKET_PRICE $MINER and the full price is
/// credited to the Motherlode pot; tickets stay in play for the whole
/// epoch, from one strike to the next. At a strike the pot still splits
/// across the MOTHERLODE_WINNERS slots, but each slot is drawn from
/// hashes + tickets chances: mined hashes keep their odds and every
/// ticket is one extra chance. A slot that lands on a ticket has its
/// share reserved here (pending_*) and SettleTicketWin delivers it to
/// the wallet whose TicketBatch covers the drawn index. The draw (win or
/// not) consumes the epoch's tickets.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct TicketState {
    pub discriminator: u64,
    /// Current sale epoch; bumped on every strike whose draw consumed the
    /// tickets.
    pub epoch: u64,
    /// Tickets sold in the current epoch (also the next batch's start).
    pub total_tickets: u64,
    /// Epoch the pending draws belong to.
    pub pending_epoch: u64,
    /// Reserved share per winner slot, waiting for SettleTicketWin
    /// (0 = that slot went to a mining candidate / already settled).
    /// Strikes that land while any share is still pending skip the
    /// ticket participation entirely, so a pending winner is never
    /// overwritten and tickets roll over to the next strike.
    pub pending_shares: [u64; MOTHERLODE_WINNERS],
    /// The drawn ticket index per winner slot (within pending_epoch).
    pub pending_tickets: [u64; MOTHERLODE_WINNERS],
    /// Lifetime tickets sold (UI stat).
    pub lifetime_tickets: u64,
    /// PDA bump.
    pub bump: u64,
}

/// One ticket purchase ("ticket_batch" PDA + epoch + start): the buyer
/// owns tickets [start, start + count) of that epoch. Closable by anyone
/// once the epoch is over (rent back to the buyer); the batch covering a
/// pending draw is protected until it is settled.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct TicketBatch {
    pub discriminator: u64,
    /// The buyer (matches Miner.authority for miners, but any wallet may
    /// buy tickets).
    pub wallet: [u8; 32],
    pub epoch: u64,
    /// First ticket index of this batch within the epoch.
    pub start: u64,
    pub count: u64,
    /// PDA bump.
    pub bump: u64,
}

impl Config {
    pub const SIZE: usize = core::mem::size_of::<Config>();
}
impl TicketState {
    pub const SIZE: usize = core::mem::size_of::<TicketState>();
}
impl TicketBatch {
    pub const SIZE: usize = core::mem::size_of::<TicketBatch>();
}
impl Game {
    pub const SIZE: usize = core::mem::size_of::<Game>();
}
impl GameRound {
    pub const SIZE: usize = core::mem::size_of::<GameRound>();
}
impl GameEntry {
    pub const SIZE: usize = core::mem::size_of::<GameEntry>();
}
impl GameWin {
    pub const SIZE: usize = core::mem::size_of::<GameWin>();
}
impl RefName {
    pub const SIZE: usize = core::mem::size_of::<RefName>();
}
impl Referral {
    pub const SIZE: usize = core::mem::size_of::<Referral>();
}
impl Round {
    pub const SIZE: usize = core::mem::size_of::<Round>();
}
impl Miner {
    pub const SIZE: usize = core::mem::size_of::<Miner>();
}
impl Lock {
    pub const SIZE: usize = core::mem::size_of::<Lock>();
}
impl Motherlode {
    pub const SIZE: usize = core::mem::size_of::<Motherlode>();
}
impl Win {
    pub const SIZE: usize = core::mem::size_of::<Win>();
}

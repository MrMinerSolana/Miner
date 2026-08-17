/// Instruction tags (first byte of instruction data).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MinerInstruction {
    /// Program initialization.
    /// Accounts: [admin (signer, payer), config (PDA), mint, round0 (PDA),
    ///           system_program]
    /// Mint requirements (created outside the program before initialize):
    /// decimals = 9, mint_authority = "treasury" PDA, freeze_authority = None.
    Initialize = 0,

    /// Miner registration.
    /// Accounts: [authority (signer, payer), miner (PDA), slot_hashes,
    ///           system_program]
    Register = 1,

    /// Set a session key (signs background submits).
    /// Accounts: [authority (signer), miner]
    /// Data: session_key [u8;32] (zeros = revoke)
    AuthorizeSession = 2,

    /// Hash submission (once per round): PoW verification, weight accrual,
    /// lazy settlement of the previous round. Charges MINE_FEE_LAMPORTS
    /// from the signer to the fee wallet and counts one Motherlode chance
    /// (reservoir sampling updates the round's winner candidate).
    /// Accounts: [signer (authority or session; writable, pays the fee),
    ///           miner, config, current_round, prev_round,
    ///           token_account (authority ATA), slot_hashes,
    ///           fee_wallet (FEE_WALLET), motherlode (PDA), system_program]
    /// Trailing accounts (any order, told apart by discriminator): the
    /// Referral PDA (required once enrolled) and/or the Lock PDA (optional;
    /// without it the locked tokens simply do not count that submit).
    /// Data: nonce u64 LE
    Mine = 3,

    /// Claim accrued rewards (mint to the authority's ATA).
    /// Accounts: [authority (signer), miner, config, prev_round, mint,
    ///           treasury (PDA), token_account (authority ATA), token_program]
    /// For an enrolled miner, trailing accounts carry the referral chain:
    /// referral(claimer), then per level the recipient's miner plus (for
    /// levels 1 and 2) the recipient's own referral PDA, possibly empty,
    /// which proves where the chain ends. Shares of missing levels burn.
    Claim = 4,

    /// Close the current round and open the next (permissionless crank).
    /// Carves MOTHERLODE_BPS of the closing round's budget into the pot
    /// (only when the round had any weight) and runs the Motherlode strike roll:
    /// with 1/MOTHERLODE_ODDS probability the pot splits evenly across the
    /// MOTHERLODE_WINNERS candidate slots into their Win accounts (created
    /// here if needed, rent from payer; a wallet holding several slots gets
    /// several shares in one account).
    /// The win accounts passed must match the current candidates in slot
    /// order; when the roll misses they are left untouched.
    /// Accounts: [payer (signer), config, new_round (PDA), system_program,
    ///           closing_round (the current round), motherlode (PDA),
    ///           slot_hashes, win_0..win_2 (candidates' PDAs, slot order)]
    Crank = 5,

    /// Close an expired Round account after retention (rent to the caller).
    /// Accounts: [recipient (signer), config, round]
    CloseRound = 6,

    /// Admin parameter change.
    /// Accounts: [admin (signer), config]
    /// Data: min_difficulty u64 LE, base_weight u64 LE, round_seconds u64 LE
    UpdateConfig = 7,

    /// Admin role handover (ultimately: a multisig with a timelock).
    /// Accounts: [admin (signer), config]
    /// Data: new_admin [u8;32]
    SetAdmin = 8,

    /// Referral enrollment (once per miner, immutable, authority only).
    /// The referrer must be a registered miner other than the caller.
    /// After enrollment Mine takes a trailing referral account and Claim
    /// takes the trailing referral-chain accounts (see Claim).
    /// Accounts: [authority (signer, payer), miner, referrer_miner,
    ///           referral (PDA), system_program]
    SetReferrer = 9,

    /// Claim a custom referral name (once per miner, immutable, first come
    /// first served). Creates both directions of the mapping.
    /// Accounts: [authority (signer, payer), miner, refname (PDA),
    ///           refname_owner (PDA), system_program]
    /// Data: the name (3-16 bytes, lowercase a-z 0-9 _)
    SetRefName = 10,

    /// Lock tokens for a mining weight multiplier (lock-to-boost). Creates
    /// or tops up the "lock" PDA and moves the tokens into its vault (the
    /// lock PDA's ATA, created client-side beforehand). Topping up re-locks
    /// the whole amount: the new now + duration must not come before the
    /// current unlock timestamp. Mine then takes the Lock account as a
    /// trailing account (see Mine) so the locked amount counts with the
    /// tier multiplier. Must be signed by the authority itself.
    /// Accounts: [authority (signer, payer), lock (PDA), user token
    ///           account, vault (lock PDA's ATA), config, token_program,
    ///           system_program]
    /// Data: amount u64 LE, duration_secs i64 LE (an exact LOCK_TIERS
    /// entry)
    Lock = 11,

    /// Withdraw an expired lock: transfers the vault balance back to the
    /// user, closes the vault and the lock PDA (rent back to the
    /// authority). Must be signed by the authority itself.
    /// Accounts: [authority (signer), lock (PDA), vault (lock PDA's ATA,
    ///           pinned to config.mint), user token account, config,
    ///           token_program]
    Unlock = 12,

    /// Create the Motherlode singleton PDA (permissionless, once; payer
    /// covers the rent). Required before fee-paying Mine can run.
    /// Accounts: [payer (signer), motherlode (PDA), system_program]
    InitMotherlode = 13,

    /// Claim a Motherlode win: MOTHERLODE_BURN_BPS of the amount is minted
    /// to the treasury ATA and burned in the same instruction (a real,
    /// visible Burn), the rest mints to the winner. Closes the Win PDA
    /// (rent to the winner). Must be signed by the authority itself.
    /// Accounts: [authority (signer), win (PDA), motherlode (PDA), config,
    ///           mint, treasury (PDA), token_account (authority ATA),
    ///           treasury_token (treasury ATA), token_program]
    ClaimMotherlode = 14,

    /// Create the Tunnels game state (admin-gated, once): the "game" PDA,
    /// the SOL vault PDA and game round 0. The $MINER vault (the game
    /// PDA's ATA) is created client-side. The pool account is stored as
    /// the EMA price source.
    /// Accounts: [admin (signer, payer), config, game (PDA),
    ///           game_vault (PDA), round0 (PDA), pool, system_program]
    /// Data: initial EMA u64 LE (lamports per whole token)
    InitGame = 15,

    /// Stake SOL and/or $MINER on a tunnel in the current game round.
    /// Creates the entry PDA or tops it up (same tunnel only). A new entry
    /// buys one players' Motherlode ticket (reservoir sampling, one per
    /// wallet per round regardless of stake). Must be signed by the
    /// authority itself.
    /// Accounts: [authority (signer, payer), game, game_round, entry (PDA),
    ///           game_vault (PDA), user token account, game token vault
    ///           (game PDA's ATA), config, slot_hashes, token_program,
    ///           system_program]
    /// Data: tunnel u8, sol u64 LE, miner u64 LE
    GameEnter = 16,

    /// Close the current game round and open the next (permissionless
    /// crank). With >= 2 staked tunnels: draws the collapsing tunnel with
    /// probability proportional to tunnel weight (entropy from slot
    /// hashes, unknown while entries were open), burns GAME_BURN_BPS of
    /// the collapsed pot's $MINER side, routes GAME_BURN_BPS of its SOL
    /// side to the fee wallet (the daily buyback), credits
    /// GAME_MOTHERLODE_BPS of both sides to the players' Motherlode and
    /// freezes the 90% payout for survivor claims. Otherwise the round is
    /// void (full refunds). Also updates the price EMA from the pool and
    /// rolls the players' Motherlode strike (1/GAME_MOTHERLODE_ODDS; the
    /// pools split evenly into the candidates' GameWin accounts).
    /// Accounts: [payer (signer), game, closing_round, new_round (PDA),
    ///           game_vault (PDA), game token vault, config, mint,
    ///           fee_wallet (FEE_WALLET), pool, slot_hashes,
    ///           token_program, system_program,
    ///           game_win_0..game_win_2 (candidates' PDAs, slot order)]
    GameSettle = 17,

    /// Claim a settled game entry: survivor payout (pro-rata share of the
    /// collapsed pot) or a full refund for a void round; a collapsed stake
    /// claims nothing. Closes the entry PDA (rent to the authority either
    /// way). Must be signed by the authority itself.
    /// Accounts: [authority (signer), game, game_round, entry (PDA),
    ///           game_vault (PDA), user token account, game token vault,
    ///           token_program]
    GameClaim = 18,

    /// Claim a players' Motherlode win: the SOL and $MINER amounts move
    /// from the game vaults to the winner. Closes the GameWin PDA (rent to
    /// the winner). Must be signed by the authority itself.
    /// Accounts: [authority (signer), game, game_win (PDA),
    ///           game_vault (PDA), user token account, game token vault,
    ///           token_program]
    GameClaimWin = 19,

    /// Close a settled game round account once the claim window
    /// (GAME_ROUND_RETENTION_SECONDS from the round's start) has passed;
    /// the rent goes to the recipient. Gated to the FEE_WALLET or the
    /// config admin signature so only the operator can garbage-collect
    /// rounds and collect the recycled rent. Unclaimed entries of a
    /// closed round lapse: GameClaim then refunds only the entry rent.
    /// GameWin (players' Motherlode) claims never expire - they do not
    /// touch round accounts.
    /// Accounts: [signer (FEE_WALLET or config.admin), recipient
    ///           (writable), config, game, game_round (writable)]
    GameCloseRound = 20,

    /// Create the Motherlode ticket sale singleton PDA (permissionless,
    /// once; payer covers the rent). Zero state: sales start in epoch 0.
    /// Accounts: [payer (signer), ticket_state (PDA), system_program]
    InitTickets = 21,

    /// Buy Motherlode tickets: burns count * TICKET_PRICE $MINER from the
    /// buyer (a real, visible SPL Burn), credits the same amount to the
    /// Motherlode pot and records the purchase as a TicketBatch PDA
    /// covering tickets [start, start + count) of the current epoch.
    /// Tickets stay valid until the next strike that runs the ticket draw
    /// (see Crank). Must be signed by the wallet itself.
    /// Accounts: [authority (signer, payer), ticket_state (PDA),
    ///           motherlode (PDA), config, mint, token_account (authority
    ///           ATA), ticket_batch (PDA), token_program, system_program]
    /// Data: count u64 LE
    BuyTickets = 22,

    /// Deliver a pending ticket-draw share (permissionless; the crank
    /// calls it right after a strike). The batch must cover the drawn
    /// ticket index of the pending epoch; the share moves into the batch
    /// wallet's Win PDA (created if needed, rent from the payer) exactly
    /// like a mining strike share, and the batch closes (rent back to the
    /// batch wallet).
    /// Accounts: [payer (signer), ticket_state (PDA), ticket_batch (PDA),
    ///           batch_wallet (writable, = batch.wallet), win (PDA of
    ///           batch.wallet), system_program]
    SettleTicketWin = 23,

    /// Close a stale TicketBatch once its epoch is over (rent back to the
    /// batch wallet; permissionless garbage collection). Refuses while the
    /// batch's epoch draw is still pending settlement.
    /// Accounts: [ticket_state (PDA), ticket_batch (PDA, writable),
    ///           batch_wallet (writable, = batch.wallet)]
    CloseTicketBatch = 24,
}

impl MinerInstruction {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Initialize,
            1 => Self::Register,
            2 => Self::AuthorizeSession,
            3 => Self::Mine,
            4 => Self::Claim,
            5 => Self::Crank,
            6 => Self::CloseRound,
            7 => Self::UpdateConfig,
            8 => Self::SetAdmin,
            9 => Self::SetReferrer,
            10 => Self::SetRefName,
            11 => Self::Lock,
            12 => Self::Unlock,
            13 => Self::InitMotherlode,
            14 => Self::ClaimMotherlode,
            15 => Self::InitGame,
            16 => Self::GameEnter,
            17 => Self::GameSettle,
            18 => Self::GameClaim,
            19 => Self::GameClaimWin,
            20 => Self::GameCloseRound,
            21 => Self::InitTickets,
            22 => Self::BuyTickets,
            23 => Self::SettleTicketWin,
            24 => Self::CloseTicketBatch,
            _ => return None,
        })
    }
}

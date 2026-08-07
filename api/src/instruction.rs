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
    /// lazy settlement of the previous round.
    /// Accounts: [signer (authority or session), miner, config,
    ///           current_round, prev_round, token_account (authority ATA),
    ///           slot_hashes]
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
    /// Accounts: [payer (signer), config, new_round (PDA), system_program]
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
            _ => return None,
        })
    }
}

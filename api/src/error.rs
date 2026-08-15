use solana_program::program_error::ProgramError;

/// Program errors (mapped to ProgramError::Custom).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MinerError {
    /// Hash does not meet the required difficulty.
    InvalidHash = 0,
    /// Account does not match the expected PDA / owner.
    InvalidAccount = 1,
    /// Wrong account discriminator.
    InvalidDiscriminator = 2,
    /// Signer is not authorized (neither authority nor session key).
    Unauthorized = 3,
    /// The given round is not the current round.
    RoundMismatch = 4,
    /// Miner already submitted in this round.
    AlreadySubmitted = 5,
    /// Round is still open (crank too early).
    RoundStillOpen = 6,
    /// Round too fresh to close (retention).
    RoundNotExpired = 7,
    /// Nothing to claim.
    NothingToClaim = 8,
    /// Token account mismatch (mint/owner/ATA).
    InvalidTokenAccount = 9,
    /// Mint does not meet the requirements (authority/decimals/freeze).
    InvalidMint = 10,
    /// Arithmetic overflow.
    Overflow = 11,
    /// Settling the previous round requires its account.
    SettlementRequired = 12,
    /// A miner cannot refer themselves.
    SelfReferral = 13,
    /// The miner enrolled in the referral program: mine/claim must be given
    /// the Referral account (and the referrer's Miner account for claim).
    ReferralAccountRequired = 14,
    /// Referral name outside the 3-16 length bound or the a-z 0-9 _ charset.
    InvalidName = 15,
    /// Lock duration is not a valid tier, or the top-up would shorten the
    /// existing lock.
    InvalidLockDuration = 16,
    /// Creating a lock requires a nonzero amount.
    InvalidLockAmount = 17,
    /// The lock has not expired yet.
    LockNotExpired = 18,
    /// Tunnel index outside 0..GAME_TUNNELS.
    InvalidTunnel = 19,
    /// The game round is still open (settle too early).
    GameRoundStillOpen = 20,
    /// The game round no longer accepts entries (past the deadline or not
    /// the current round).
    GameRoundClosed = 21,
    /// The game round has not been settled yet (claim too early).
    GameNotSettled = 22,
    /// A top-up must stay in the tunnel of the original entry.
    GameTunnelMismatch = 23,
    /// Stake below the minimum (or both amounts zero).
    InvalidStake = 24,
    /// The pool account does not carry a readable spot price.
    InvalidPool = 25,
}

impl From<MinerError> for ProgramError {
    fn from(e: MinerError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

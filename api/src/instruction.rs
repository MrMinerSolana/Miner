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
    /// Data: nonce u64 LE
    Mine = 3,

    /// Claim accrued rewards (mint to the authority's ATA).
    /// Accounts: [authority (signer), miner, config, prev_round, mint,
    ///           treasury (PDA), token_account (authority ATA), token_program]
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
            _ => return None,
        })
    }
}

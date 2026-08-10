use solana_program::pubkey::Pubkey;

use crate::consts::*;

pub fn config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[CONFIG_SEED], &crate::id())
}

pub fn treasury_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[TREASURY_SEED], &crate::id())
}

pub fn round_pda(index: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ROUND_SEED, &index.to_le_bytes()], &crate::id())
}

pub fn miner_pda(authority: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[MINER_SEED, authority.as_ref()], &crate::id())
}

pub fn referral_pda(authority: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[REFERRAL_SEED, authority.as_ref()], &crate::id())
}

pub fn refname_pda(name: &[u8]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[REFNAME_SEED, name], &crate::id())
}

pub fn refname_owner_pda(authority: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[REFNAME_OWNER_SEED, authority.as_ref()], &crate::id())
}

pub fn lock_pda(authority: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[LOCK_SEED, authority.as_ref()], &crate::id())
}

pub fn motherlode_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[MOTHERLODE_SEED], &crate::id())
}

pub fn win_pda(authority: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[WIN_SEED, authority.as_ref()], &crate::id())
}

/// Per-user lock vault: the canonical ATA of the lock PDA.
pub fn lock_vault(authority: &Pubkey, mint: &Pubkey) -> Pubkey {
    ata(&lock_pda(authority).0, mint)
}

/// Canonical Associated Token Account for (wallet, mint).
pub fn ata(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            wallet.as_ref(),
            SPL_TOKEN_PROGRAM_ID.as_ref(),
            mint.as_ref(),
        ],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0
}

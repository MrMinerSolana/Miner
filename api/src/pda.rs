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

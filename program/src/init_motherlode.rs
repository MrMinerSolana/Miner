use miner_api::{consts::*, pda, state::*};
#[cfg(feature = "short-motherlode")]
use solana_program::sysvar::Sysvar;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
};

use crate::loaders::*;

/// Creates the Motherlode singleton PDA (permissionless, once; create_pda
/// rejects an already initialized account). Zero state: the pot starts
/// accruing with the next round close, hash counting with the next mine.
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    let [payer_info, motherlode_info, system_program] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(payer_info)?;
    expect_writable(motherlode_info)?;

    let (motherlode_key, bump) = pda::motherlode_pda();
    expect_key(motherlode_info, &motherlode_key)?;

    // Devnet rehearsal only: an account left over from an older layout is
    // resized and reset instead of blocking the rehearsal (the singleton
    // PDA cannot move). A mainnet artifact never carries this branch: there
    // the account is created once at the right size and re-init always
    // fails on AccountAlreadyInitialized.
    #[cfg(feature = "short-motherlode")]
    if motherlode_info.owner.eq(&miner_api::id())
        && motherlode_info.data_len() != Motherlode::SIZE
    {
        motherlode_info.resize(Motherlode::SIZE)?;
        let rent = solana_program::rent::Rent::get()?.minimum_balance(Motherlode::SIZE);
        let shortfall = rent.saturating_sub(motherlode_info.lamports());
        if shortfall > 0 {
            // SystemInstruction::Transfer (bincode): tag u32 + lamports u64.
            let mut data = Vec::with_capacity(12);
            data.extend_from_slice(&2u32.to_le_bytes());
            data.extend_from_slice(&shortfall.to_le_bytes());
            let ix = solana_program::instruction::Instruction {
                program_id: SYSTEM_PROGRAM_ID,
                accounts: vec![
                    solana_program::instruction::AccountMeta::new(*payer_info.key, true),
                    solana_program::instruction::AccountMeta::new(*motherlode_info.key, false),
                ],
                data,
            };
            solana_program::program::invoke(
                &ix,
                &[payer_info.clone(), motherlode_info.clone(), system_program.clone()],
            )?;
        }
        return write_state(
            motherlode_info,
            &Motherlode {
                discriminator: MOTHERLODE_DISCRIMINATOR,
                pot: 0,
                round_index: 0,
                hashes: 0,
                candidates: [[0u8; 32]; MOTHERLODE_WINNERS],
                total_burned: 0,
                total_fees: 0,
                last_winners: [[0u8; 32]; MOTHERLODE_WINNERS],
                last_win_amount: 0,
                last_win_ts: 0,
                bump: bump as u64,
            },
        );
    }

    create_pda(
        motherlode_info,
        payer_info,
        system_program,
        Motherlode::SIZE,
        &[MOTHERLODE_SEED, &[bump]],
    )?;
    write_state(
        motherlode_info,
        &Motherlode {
            discriminator: MOTHERLODE_DISCRIMINATOR,
            pot: 0,
            round_index: 0,
            hashes: 0,
            candidates: [[0u8; 32]; MOTHERLODE_WINNERS],
            total_burned: 0,
            total_fees: 0,
            last_winners: [[0u8; 32]; MOTHERLODE_WINNERS],
            last_win_amount: 0,
            last_win_ts: 0,
            bump: bump as u64,
        },
    )
}

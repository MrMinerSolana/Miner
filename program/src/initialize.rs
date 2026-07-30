use miner_api::{consts::*, error::MinerError, pda, state::*};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, sysvar::Sysvar,
};

use crate::loaders::*;

/// Initialization: creates the Config (PDA) and round 0.
///
/// The mint is created outside the program BEFORE initialize (launch flow:
/// create mint -> premine to LP -> set mint authority = treasury PDA);
/// here it is only verified. The program never mints outside of claim.
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    let [admin_info, config_info, mint_info, round0_info, system_program] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(admin_info)?;

    let (config_key, config_bump) = pda::config_pda();
    let (treasury_key, treasury_bump) = pda::treasury_pda();
    let (round0_key, round0_bump) = pda::round_pda(0);
    expect_key(config_info, &config_key)?;
    expect_key(round0_info, &round0_key)?;

    verify_mint(mint_info, &treasury_key)?;

    let now = Clock::get()?.unix_timestamp;

    create_pda(
        config_info,
        admin_info,
        system_program,
        Config::SIZE,
        &[CONFIG_SEED, &[config_bump]],
    )?;
    write_state(
        config_info,
        &Config {
            discriminator: CONFIG_DISCRIMINATOR,
            admin: admin_info.key.to_bytes(),
            mint: mint_info.key.to_bytes(),
            current_round: 0,
            round_start_ts: now,
            min_difficulty: INITIAL_MIN_DIFFICULTY,
            base_weight: INITIAL_BASE_WEIGHT,
            round_seconds: INITIAL_ROUND_SECONDS,
            config_bump: config_bump as u64,
            treasury_bump: treasury_bump as u64,
        },
    )?;

    create_pda(
        round0_info,
        admin_info,
        system_program,
        Round::SIZE,
        &[ROUND_SEED, &0u64.to_le_bytes(), &[round0_bump]],
    )?;
    write_state(
        round0_info,
        &Round {
            discriminator: ROUND_DISCRIMINATOR,
            index: 0,
            total_weight: 0,
            start_ts: now,
            budget: round_budget(INITIAL_ROUND_SECONDS),
        },
    )?;

    Ok(())
}

/// Mint: SPL Token, decimals 9, mint authority = treasury PDA, no freeze.
fn verify_mint(mint_info: &AccountInfo, treasury: &solana_program::pubkey::Pubkey) -> ProgramResult {
    if mint_info.owner.ne(&SPL_TOKEN_PROGRAM_ID) {
        return Err(MinerError::InvalidMint.into());
    }
    let data = mint_info.try_borrow_data()?;
    if data.len() != 82 {
        return Err(MinerError::InvalidMint.into());
    }
    // COption<Pubkey> mint_authority: tag 1 (Some) + treasury key
    if data[0..4] != [1, 0, 0, 0] || data[4..36] != treasury.to_bytes() {
        return Err(MinerError::InvalidMint.into());
    }
    // decimals
    if data[44] != TOKEN_DECIMALS {
        return Err(MinerError::InvalidMint.into());
    }
    // is_initialized
    if data[45] != 1 {
        return Err(MinerError::InvalidMint.into());
    }
    // freeze_authority: None
    if data[46..50] != [0, 0, 0, 0] {
        return Err(MinerError::InvalidMint.into());
    }
    Ok(())
}

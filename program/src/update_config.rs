use miner_api::{consts::*, error::MinerError, state::*};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
};

use crate::loaders::*;

/// Admin parameter change: min_difficulty, base_weight and round_seconds
/// (round cadence; the budget scales pro-rata, so emission per minute
/// stays constant; takes effect from the next round).
pub fn process(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [admin_info, config_info] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(admin_info)?;
    expect_writable(config_info)?;
    expect_program_account(config_info, CONFIG_DISCRIMINATOR)?;

    if data.len() != 24 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let min_difficulty = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let base_weight = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let round_seconds = u64::from_le_bytes(data[16..24].try_into().unwrap());

    // Sanity: difficulty within a sane range (capped at 32; beyond that
    // even strong hardware cannot finish within a round, i.e. the admin
    // could freeze mining), non-zero base, cadence within allowed bounds.
    if min_difficulty > 32
        || base_weight == 0
        || !(MIN_ROUND_SECONDS..=MAX_ROUND_SECONDS).contains(&round_seconds)
    {
        return Err(ProgramError::InvalidInstructionData);
    }

    let mut config = read_state::<Config>(config_info)?;
    if config.admin != admin_info.key.to_bytes() {
        return Err(MinerError::Unauthorized.into());
    }
    config.min_difficulty = min_difficulty;
    config.base_weight = base_weight;
    config.round_seconds = round_seconds;
    write_state(config_info, &config)?;

    Ok(())
}

use miner_api::{consts::*, error::MinerError, pda, state::*};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, sysvar::Sysvar,
};

use crate::loaders::*;

/// Permissionless crank: closes the current round (implicitly, in that it stops
/// being current) and opens the next one. Anyone can call it; the caller
/// pays rent for the new round account and recovers it after retention
/// via close_round.
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    let [payer_info, config_info, new_round_info, system_program] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(payer_info)?;
    expect_writable(config_info)?;
    expect_program_account(config_info, CONFIG_DISCRIMINATOR)?;
    let mut config = read_state::<Config>(config_info)?;

    let now = Clock::get()?.unix_timestamp;
    let round_seconds = config.round_seconds as i64;
    if now < config.round_start_ts.saturating_add(round_seconds) {
        return Err(MinerError::RoundStillOpen.into());
    }

    let new_index = config
        .current_round
        .checked_add(1)
        .ok_or(MinerError::Overflow)?;
    let (new_round_key, new_round_bump) = pda::round_pda(new_index);
    expect_key(new_round_info, &new_round_key)?;

    create_pda(
        new_round_info,
        payer_info,
        system_program,
        Round::SIZE,
        &[ROUND_SEED, &new_index.to_le_bytes(), &[new_round_bump]],
    )?;
    write_state(
        new_round_info,
        &Round {
            discriminator: ROUND_DISCRIMINATOR,
            index: new_index,
            total_weight: 0,
            start_ts: now,
            // Halving-aware budget, frozen for the round's lifetime like
            // before (later halvings never touch already-open rounds).
            budget: round_budget_at(config.round_seconds, now),
        },
    )?;

    config.current_round = new_index;
    // Keep the cadence on small slips; reset on a large backlog.
    let scheduled = config.round_start_ts.saturating_add(round_seconds);
    config.round_start_ts = if now < scheduled.saturating_add(round_seconds) {
        scheduled
    } else {
        now
    };
    write_state(config_info, &config)?;

    Ok(())
}

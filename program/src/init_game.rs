use miner_api::{consts::*, error::MinerError, pda, state::*};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, sysvar::Sysvar,
};

use crate::loaders::*;

/// Creates the Tunnels game state: the "game" PDA, the SOL vault (a
/// zero-data program-owned PDA: stakes flow in via system transfers,
/// payouts debit its lamports directly) and game round 0. Admin-gated:
/// the pool stored here is the price source for valuing $MINER stakes,
/// so its choice is as sensitive as any config parameter. The $MINER
/// vault (the game PDA's ATA) is created client-side in the same
/// transaction.
pub fn process(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [admin_info, config_info, game_info, vault_info, round0_info, pool_info, system_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() != 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let initial_ema = u64::from_le_bytes(data.try_into().unwrap());
    if initial_ema == 0 {
        return Err(MinerError::InvalidPool.into());
    }

    expect_signer(admin_info)?;
    expect_program_account(config_info, CONFIG_DISCRIMINATOR)?;
    let config = read_state::<Config>(config_info)?;
    if config.admin != admin_info.key.to_bytes() {
        return Err(MinerError::Unauthorized.into());
    }

    let (game_key, game_bump) = pda::game_pda();
    expect_key(game_info, &game_key)?;
    expect_writable(game_info)?;

    let (vault_key, vault_bump) = pda::game_vault_pda();
    expect_key(vault_info, &vault_key)?;
    expect_writable(vault_info)?;

    let (round0_key, round0_bump) = pda::game_round_pda(0);
    expect_key(round0_info, &round0_key)?;
    expect_writable(round0_info)?;

    let now = Clock::get()?.unix_timestamp;

    create_pda(
        game_info,
        admin_info,
        system_program,
        Game::SIZE,
        &[GAME_SEED, &[game_bump]],
    )?;
    write_state(
        game_info,
        &Game {
            discriminator: GAME_DISCRIMINATOR,
            current_round: 0,
            round_start_ts: now,
            round_seconds: GAME_ROUND_SECONDS,
            ema_lamports_per_token: initial_ema,
            pool: pool_info.key.to_bytes(),
            ml_sol: 0,
            ml_miner: 0,
            ml_last_winners: [[0u8; 32]; GAME_MOTHERLODE_WINNERS],
            ml_last_sol: 0,
            ml_last_miner: 0,
            ml_last_ts: 0,
            total_burned: 0,
            total_fee_sol: 0,
            total_volume_sol: 0,
            total_volume_miner: 0,
            total_rounds_played: 0,
            bump: game_bump as u64,
            vault_bump: vault_bump as u64,
        },
    )?;

    // The SOL vault: zero data, owned by this program. The rent-exempt
    // cushion paid here never leaves: payouts only move amounts that were
    // staked in on top of it.
    create_pda(
        vault_info,
        admin_info,
        system_program,
        0,
        &[GAME_VAULT_SEED, &[vault_bump]],
    )?;

    create_pda(
        round0_info,
        admin_info,
        system_program,
        GameRound::SIZE,
        &[GAME_ROUND_SEED, &0u64.to_le_bytes(), &[round0_bump]],
    )?;
    write_state(
        round0_info,
        &GameRound {
            discriminator: GAME_ROUND_DISCRIMINATOR,
            index: 0,
            start_ts: now,
            sol: [0; GAME_TUNNELS],
            miner: [0; GAME_TUNNELS],
            weight: [0; GAME_TUNNELS],
            entries: 0,
            candidates: [[0u8; 32]; GAME_MOTHERLODE_WINNERS],
            settled: GAME_ROUND_OPEN,
            collapsed: 0,
            payout_sol: 0,
            payout_miner: 0,
            survivor_weight: 0,
        },
    )?;

    Ok(())
}

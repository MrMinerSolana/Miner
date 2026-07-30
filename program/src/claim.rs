use miner_api::{consts::*, error::MinerError, state::*};
use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::loaders::*;

/// Reward claim: mints pending_rewards to the authority's token account.
/// Must be signed by the authority (a session key may not claim).
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    let [authority_info, miner_info, config_info, prev_round_info, mint_info, treasury_info, token_account_info, token_program_info] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(authority_info)?;
    expect_program_account(config_info, CONFIG_DISCRIMINATOR)?;
    let config = read_state::<Config>(config_info)?;

    expect_writable(miner_info)?;
    expect_program_account(miner_info, MINER_DISCRIMINATOR)?;
    let mut miner = read_state::<Miner>(miner_info)?;
    if miner.authority != authority_info.key.to_bytes() {
        return Err(MinerError::Unauthorized.into());
    }

    // Settle any outstanding round so the claim collects everything.
    settle_previous_round(&mut miner, config.current_round, prev_round_info)?;

    let amount = miner.pending_rewards;
    if amount == 0 {
        return Err(MinerError::NothingToClaim.into());
    }

    // Validate the CPI accounts.
    expect_key(mint_info, &Pubkey::new_from_array(config.mint))?;
    expect_key(token_program_info, &SPL_TOKEN_PROGRAM_ID)?;
    let treasury_key = Pubkey::create_program_address(
        &[TREASURY_SEED, &[config.treasury_bump as u8]],
        &miner_api::id(),
    )
    .map_err(|_| ProgramError::from(MinerError::InvalidAccount))?;
    expect_key(treasury_info, &treasury_key)?;

    // The destination must exist and belong to the authority (right mint).
    if token_account_info.data_is_empty() {
        return Err(MinerError::InvalidTokenAccount.into());
    }
    read_token_balance(token_account_info, &config.mint, &miner.authority)?;

    // CPI: spl_token::mint_to signed by the treasury PDA.
    let mut ix_data = Vec::with_capacity(9);
    ix_data.push(SPL_TOKEN_MINT_TO_IX);
    ix_data.extend_from_slice(&amount.to_le_bytes());
    let mint_to_ix = Instruction {
        program_id: SPL_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*mint_info.key, false),
            AccountMeta::new(*token_account_info.key, false),
            AccountMeta::new_readonly(treasury_key, true),
        ],
        data: ix_data,
    };
    invoke_signed(
        &mint_to_ix,
        &[
            mint_info.clone(),
            token_account_info.clone(),
            treasury_info.clone(),
        ],
        &[&[TREASURY_SEED, &[config.treasury_bump as u8]]],
    )?;

    miner.pending_rewards = 0;
    miner.total_mined = miner
        .total_mined
        .checked_add(amount)
        .ok_or(MinerError::Overflow)?;
    write_state(miner_info, &miner)?;

    Ok(())
}

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

/// spl_token::mint_to signed by the treasury PDA.
fn mint_to<'info>(
    mint_info: &AccountInfo<'info>,
    destination_info: &AccountInfo<'info>,
    treasury_info: &AccountInfo<'info>,
    treasury_seeds: &[&[u8]],
    value: u64,
) -> ProgramResult {
    let mut ix_data = Vec::with_capacity(9);
    ix_data.push(SPL_TOKEN_MINT_TO_IX);
    ix_data.extend_from_slice(&value.to_le_bytes());
    let ix = Instruction {
        program_id: SPL_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*mint_info.key, false),
            AccountMeta::new(*destination_info.key, false),
            AccountMeta::new_readonly(*treasury_info.key, true),
        ],
        data: ix_data,
    };
    invoke_signed(
        &ix,
        &[
            mint_info.clone(),
            destination_info.clone(),
            treasury_info.clone(),
        ],
        &[treasury_seeds],
    )
}

/// Claim a Motherlode win. MOTHERLODE_BURN_BPS of the amount is minted to
/// the treasury ATA and burned in the same instruction (a real Burn any
/// explorer shows), the rest mints to the winner. The Win PDA is closed and
/// its rent goes to the winner. Must be signed by the authority itself.
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    let [authority_info, win_info, motherlode_info, config_info, mint_info, treasury_info, token_account_info, treasury_token_info, token_program_info] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(authority_info)?;
    expect_program_account(config_info, CONFIG_DISCRIMINATOR)?;
    let config = read_state::<Config>(config_info)?;

    expect_writable(win_info)?;
    expect_program_account(win_info, WIN_DISCRIMINATOR)?;
    let win = read_state::<Win>(win_info)?;
    if win.authority != authority_info.key.to_bytes() {
        return Err(MinerError::Unauthorized.into());
    }

    expect_writable(motherlode_info)?;
    expect_program_account(motherlode_info, MOTHERLODE_DISCRIMINATOR)?;
    let mut motherlode = read_state::<Motherlode>(motherlode_info)?;

    let amount = win.amount;
    if amount == 0 {
        return Err(MinerError::NothingToClaim.into());
    }
    let burn_amount = ((amount as u128) * (MOTHERLODE_BURN_BPS as u128)
        / (BPS_DENOM as u128)) as u64;
    let payout = amount - burn_amount;

    // Validate the CPI accounts.
    expect_key(mint_info, &Pubkey::new_from_array(config.mint))?;
    expect_key(token_program_info, &SPL_TOKEN_PROGRAM_ID)?;
    let treasury_seeds: &[&[u8]] = &[TREASURY_SEED, &[config.treasury_bump as u8]];
    let treasury_key = Pubkey::create_program_address(treasury_seeds, &miner_api::id())
        .map_err(|_| ProgramError::from(MinerError::InvalidAccount))?;
    expect_key(treasury_info, &treasury_key)?;

    // Destinations must exist with the right mint/owner.
    if token_account_info.data_is_empty() {
        return Err(MinerError::InvalidTokenAccount.into());
    }
    read_token_balance(token_account_info, &config.mint, &win.authority)?;
    if treasury_token_info.data_is_empty() {
        return Err(MinerError::InvalidTokenAccount.into());
    }
    read_token_balance(treasury_token_info, &config.mint, &treasury_key.to_bytes())?;

    // 80% to the winner.
    mint_to(mint_info, token_account_info, treasury_info, treasury_seeds, payout)?;

    // 20% minted to the treasury ATA and burned right away, so the burn is
    // a visible SPL Burn and the mint's supply stays exact.
    if burn_amount > 0 {
        mint_to(
            mint_info,
            treasury_token_info,
            treasury_info,
            treasury_seeds,
            burn_amount,
        )?;
        let mut ix_data = Vec::with_capacity(9);
        ix_data.push(SPL_TOKEN_BURN_IX);
        ix_data.extend_from_slice(&burn_amount.to_le_bytes());
        let burn_ix = Instruction {
            program_id: SPL_TOKEN_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(*treasury_token_info.key, false),
                AccountMeta::new(*mint_info.key, false),
                AccountMeta::new_readonly(treasury_key, true),
            ],
            data: ix_data,
        };
        invoke_signed(
            &burn_ix,
            &[
                treasury_token_info.clone(),
                mint_info.clone(),
                treasury_info.clone(),
            ],
            &[treasury_seeds],
        )?;
    }

    motherlode.total_burned = motherlode
        .total_burned
        .checked_add(burn_amount)
        .ok_or(MinerError::Overflow)?;
    write_state(motherlode_info, &motherlode)?;

    // Close the Win PDA: zero the data (no revival within the same tx) and
    // move the rent to the winner.
    {
        let mut data = win_info.try_borrow_mut_data()?;
        data.fill(0);
    }
    let lamports = win_info.lamports();
    **win_info.try_borrow_mut_lamports()? = 0;
    **authority_info.try_borrow_mut_lamports()? = authority_info
        .lamports()
        .checked_add(lamports)
        .ok_or(MinerError::Overflow)?;

    Ok(())
}

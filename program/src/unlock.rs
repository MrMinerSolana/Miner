use miner_api::{consts::*, error::MinerError, pda, state::*};
use solana_program::{
    account_info::AccountInfo,
    clock::Clock,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::Sysvar,
};

use crate::loaders::*;

/// Lock withdrawal after expiry: transfers the whole vault balance back to
/// the authority's token account, closes the vault (SPL CloseAccount) and
/// the lock PDA itself, returning both rents to the authority. Must be
/// signed by the authority itself.
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    let [authority_info, lock_info, vault_info, user_token_info, config_info, token_program_info] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(authority_info)?;
    expect_program_account(config_info, CONFIG_DISCRIMINATOR)?;
    let config = read_state::<Config>(config_info)?;

    expect_writable(lock_info)?;
    expect_program_account(lock_info, LOCK_DISCRIMINATOR)?;
    let lock = read_state::<Lock>(lock_info)?;
    let authority = authority_info.key.to_bytes();
    if lock.authority != authority {
        return Err(MinerError::Unauthorized.into());
    }

    let now = Clock::get()?.unix_timestamp;
    if now < lock.unlock_ts {
        return Err(MinerError::LockNotExpired.into());
    }

    let lock_seeds: &[&[u8]] = &[LOCK_SEED, authority.as_ref(), &[lock.bump as u8]];
    let lock_key = Pubkey::create_program_address(lock_seeds, &miner_api::id())
        .map_err(|_| ProgramError::from(MinerError::InvalidAccount))?;
    expect_key(lock_info, &lock_key)?;

    // Vault: pinned to the canonical ATA of the lock PDA for config.mint,
    // the same address Lock deposited into. Accepting any token account
    // owned by the lock PDA would let a hand-crafted transaction close the
    // lock while the real vault still holds the tokens.
    expect_key(
        vault_info,
        &pda::ata(&lock_key, &Pubkey::new_from_array(config.mint)),
    )?;
    expect_writable(vault_info)?;
    if vault_info.data_is_empty() {
        return Err(MinerError::InvalidTokenAccount.into());
    }
    let vault_balance = read_token_balance(vault_info, &config.mint, &lock_key.to_bytes())?;

    // Destination: the authority's token account with the same mint.
    expect_writable(user_token_info)?;
    if user_token_info.data_is_empty() {
        return Err(MinerError::InvalidTokenAccount.into());
    }
    read_token_balance(user_token_info, &config.mint, &authority)?;

    expect_key(token_program_info, &SPL_TOKEN_PROGRAM_ID)?;

    // CPI: transfer the whole vault balance back, signed by the lock PDA.
    if vault_balance > 0 {
        let mut ix_data = Vec::with_capacity(9);
        ix_data.push(SPL_TOKEN_TRANSFER_IX);
        ix_data.extend_from_slice(&vault_balance.to_le_bytes());
        let transfer_ix = Instruction {
            program_id: SPL_TOKEN_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(*vault_info.key, false),
                AccountMeta::new(*user_token_info.key, false),
                AccountMeta::new_readonly(lock_key, true),
            ],
            data: ix_data,
        };
        invoke_signed(
            &transfer_ix,
            &[
                vault_info.clone(),
                user_token_info.clone(),
                lock_info.clone(),
            ],
            &[lock_seeds],
        )?;
    }

    // CPI: close the vault, rent to the authority.
    let close_ix = Instruction {
        program_id: SPL_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*vault_info.key, false),
            AccountMeta::new(*authority_info.key, false),
            AccountMeta::new_readonly(lock_key, true),
        ],
        data: vec![SPL_TOKEN_CLOSE_ACCOUNT_IX],
    };
    invoke_signed(
        &close_ix,
        &[
            vault_info.clone(),
            authority_info.clone(),
            lock_info.clone(),
        ],
        &[lock_seeds],
    )?;

    // Close the lock PDA: zero the data (no revival within the same tx)
    // and move the lamports out.
    {
        let mut data = lock_info.try_borrow_mut_data()?;
        data.fill(0);
    }
    let lamports = lock_info.lamports();
    **lock_info.try_borrow_mut_lamports()? = 0;
    **authority_info.try_borrow_mut_lamports()? = authority_info
        .lamports()
        .checked_add(lamports)
        .ok_or(MinerError::Overflow)?;

    Ok(())
}

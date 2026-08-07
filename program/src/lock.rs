use miner_api::{consts::*, error::MinerError, pda, state::*};
use solana_program::{
    account_info::AccountInfo,
    clock::Clock,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::Sysvar,
};

use crate::loaders::*;

/// Lock-to-boost deposit: creates or tops up the "lock" PDA and transfers
/// the tokens into its vault (the lock PDA's ATA, per-user isolation: only
/// this PDA can move them). While the lock is active the amount counts
/// toward the mining weight multiplied by the tier's multiplier (Mine takes
/// the Lock account as a trailing account).
///
/// Topping up re-locks everything: the new unlock timestamp (now +
/// duration) must not come before the current one, so a lock can never be
/// shortened. The multiplier follows the tier chosen last (the no-shorten
/// rule guarantees the remaining time always covers it). Must be signed by
/// the authority itself (a session key may not lock).
pub fn process(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority_info, lock_info, user_token_info, vault_info, config_info, token_program_info, system_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() != 16 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let amount = u64::from_le_bytes(data[..8].try_into().unwrap());
    let duration = i64::from_le_bytes(data[8..16].try_into().unwrap());
    let multiplier_bps =
        lock_multiplier_bps(duration).ok_or(MinerError::InvalidLockDuration)?;

    expect_signer(authority_info)?;
    expect_program_account(config_info, CONFIG_DISCRIMINATOR)?;
    let config = read_state::<Config>(config_info)?;
    let authority = authority_info.key.to_bytes();

    // Source: the authority's token account with the right mint.
    expect_writable(user_token_info)?;
    if user_token_info.data_is_empty() {
        return Err(MinerError::InvalidTokenAccount.into());
    }
    read_token_balance(user_token_info, &config.mint, &authority)?;

    let (lock_key, lock_bump) = pda::lock_pda(authority_info.key);
    expect_key(lock_info, &lock_key)?;
    expect_writable(lock_info)?;

    // Vault: the canonical ATA of the lock PDA, created client-side (the
    // idempotent create-ATA instruction rides in the same transaction).
    expect_key(
        vault_info,
        &pda::ata(&lock_key, &Pubkey::new_from_array(config.mint)),
    )?;
    expect_writable(vault_info)?;
    if vault_info.data_is_empty() {
        return Err(MinerError::InvalidTokenAccount.into());
    }
    read_token_balance(vault_info, &config.mint, &lock_key.to_bytes())?;

    expect_key(token_program_info, &SPL_TOKEN_PROGRAM_ID)?;

    let now = Clock::get()?.unix_timestamp;
    let unlock_ts = now.checked_add(duration).ok_or(MinerError::Overflow)?;

    let mut lock = if lock_info.data_is_empty() {
        if amount == 0 {
            return Err(MinerError::InvalidLockAmount.into());
        }
        create_pda(
            lock_info,
            authority_info,
            system_program,
            Lock::SIZE,
            &[LOCK_SEED, authority.as_ref(), &[lock_bump]],
        )?;
        Lock {
            discriminator: LOCK_DISCRIMINATOR,
            authority,
            amount: 0,
            unlock_ts,
            multiplier_bps,
            bump: lock_bump as u64,
        }
    } else {
        expect_program_account(lock_info, LOCK_DISCRIMINATOR)?;
        let existing = read_state::<Lock>(lock_info)?;
        if existing.authority != authority {
            return Err(MinerError::Unauthorized.into());
        }
        existing
    };

    // A top-up may extend but never shorten the lock.
    if unlock_ts < lock.unlock_ts {
        return Err(MinerError::InvalidLockDuration.into());
    }
    lock.unlock_ts = unlock_ts;
    lock.multiplier_bps = multiplier_bps;
    lock.amount = lock
        .amount
        .checked_add(amount)
        .ok_or(MinerError::Overflow)?;

    if amount > 0 {
        // CPI: spl_token::transfer user -> vault, signed by the authority.
        let mut ix_data = Vec::with_capacity(9);
        ix_data.push(SPL_TOKEN_TRANSFER_IX);
        ix_data.extend_from_slice(&amount.to_le_bytes());
        let transfer_ix = Instruction {
            program_id: SPL_TOKEN_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(*user_token_info.key, false),
                AccountMeta::new(*vault_info.key, false),
                AccountMeta::new_readonly(*authority_info.key, true),
            ],
            data: ix_data,
        };
        invoke(
            &transfer_ix,
            &[
                user_token_info.clone(),
                vault_info.clone(),
                authority_info.clone(),
            ],
        )?;
    }

    write_state(lock_info, &lock)?;
    Ok(())
}

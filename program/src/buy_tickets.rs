use miner_api::{consts::*, error::MinerError, pda, state::*};
use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::loaders::*;

/// Buy Motherlode tickets: burns count * TICKET_PRICE $MINER from the
/// buyer's ATA (a real, visible SPL Burn signed by the buyer), credits the
/// full amount to the Motherlode pot and records the purchase as a
/// TicketBatch covering tickets [start, start + count) of the current
/// epoch. Tickets stay valid until the next strike that runs the ticket
/// draw (see crank.rs); the batch account is then garbage-collectable
/// with the rent going back to the buyer.
pub fn process(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority_info, ticket_state_info, motherlode_info, config_info, mint_info, token_account_info, ticket_batch_info, token_program_info, system_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(authority_info)?;

    let count = u64::from_le_bytes(
        data.try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    if count == 0 {
        return Err(MinerError::InvalidTicketCount.into());
    }
    let cost = count
        .checked_mul(TICKET_PRICE)
        .ok_or(MinerError::InvalidTicketCount)?;

    expect_writable(ticket_state_info)?;
    expect_program_account(ticket_state_info, TICKET_STATE_DISCRIMINATOR)?;
    let mut tickets = read_state::<TicketState>(ticket_state_info)?;

    expect_writable(motherlode_info)?;
    expect_program_account(motherlode_info, MOTHERLODE_DISCRIMINATOR)?;
    let mut motherlode = read_state::<Motherlode>(motherlode_info)?;

    expect_program_account(config_info, CONFIG_DISCRIMINATOR)?;
    let config = read_state::<Config>(config_info)?;
    expect_key(mint_info, &Pubkey::new_from_array(config.mint))?;
    expect_key(token_program_info, &SPL_TOKEN_PROGRAM_ID)?;

    // The buyer's ATA must exist with the right mint/owner and cover the
    // cost (the burn CPI would fail anyway; this just fails cleaner).
    if token_account_info.data_is_empty() {
        return Err(MinerError::InvalidTokenAccount.into());
    }
    let balance = read_token_balance(
        token_account_info,
        &config.mint,
        &authority_info.key.to_bytes(),
    )?;
    if balance < cost {
        return Err(MinerError::InvalidTokenAccount.into());
    }

    // The batch PDA is keyed by the lifetime ticket counter: it never
    // resets, so the address is unique for every purchase ever made and a
    // stale, never-collected batch can't block a future buy.
    let start = tickets.total_tickets;
    let epoch = tickets.epoch;
    let lifetime = tickets.lifetime_tickets;
    let (batch_key, batch_bump) = pda::ticket_batch_pda(lifetime);
    expect_key(ticket_batch_info, &batch_key)?;
    expect_writable(ticket_batch_info)?;

    tickets.total_tickets = start
        .checked_add(count)
        .ok_or(MinerError::InvalidTicketCount)?;
    tickets.lifetime_tickets = lifetime
        .checked_add(count)
        .ok_or(MinerError::Overflow)?;

    // Burn the payment from the buyer's ATA (buyer signs the CPI).
    let mut ix_data = Vec::with_capacity(9);
    ix_data.push(SPL_TOKEN_BURN_IX);
    ix_data.extend_from_slice(&cost.to_le_bytes());
    let burn_ix = Instruction {
        program_id: SPL_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*token_account_info.key, false),
            AccountMeta::new(*mint_info.key, false),
            AccountMeta::new_readonly(*authority_info.key, true),
        ],
        data: ix_data,
    };
    invoke(
        &burn_ix,
        &[
            token_account_info.clone(),
            mint_info.clone(),
            authority_info.clone(),
        ],
    )?;

    // The burned payment stacks the pot 1:1 (minted back only when a win
    // is claimed, and 20% of that burns again at claim - net deflation).
    motherlode.pot = motherlode
        .pot
        .checked_add(cost)
        .ok_or(MinerError::Overflow)?;
    write_state(motherlode_info, &motherlode)?;

    create_pda(
        ticket_batch_info,
        authority_info,
        system_program,
        TicketBatch::SIZE,
        &[TICKET_BATCH_SEED, &lifetime.to_le_bytes(), &[batch_bump]],
    )?;
    write_state(
        ticket_batch_info,
        &TicketBatch {
            discriminator: TICKET_BATCH_DISCRIMINATOR,
            wallet: authority_info.key.to_bytes(),
            epoch,
            start,
            count,
            bump: batch_bump as u64,
        },
    )?;
    write_state(ticket_state_info, &tickets)?;

    Ok(())
}

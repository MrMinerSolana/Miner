use miner_api::{consts::*, pda, state::*};
use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    rent::Rent,
    sysvar::Sysvar,
};

use crate::loaders::*;

/// Creates the Motherlode ticket sale singleton PDA (permissionless, once;
/// create_pda rejects an already initialized account). Zero state: sales
/// start in epoch 0 and the first strike with tickets runs the first draw.
///
/// A singleton left over from an older layout (shorter account data, a
/// devnet rehearsal artifact) is resized and reset instead; an account
/// already at the current size is rejected, so a live sale can never be
/// wiped by re-running this.
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    let [payer_info, ticket_state_info, system_program] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(payer_info)?;
    expect_writable(ticket_state_info)?;

    let (ticket_state_key, bump) = pda::ticket_state_pda();
    expect_key(ticket_state_info, &ticket_state_key)?;

    if ticket_state_info.owner.eq(&miner_api::id())
        && !ticket_state_info.data_is_empty()
    {
        if ticket_state_info.data_len() == TicketState::SIZE {
            return Err(ProgramError::AccountAlreadyInitialized);
        }
        expect_program_account(ticket_state_info, TICKET_STATE_DISCRIMINATOR)?;
        let required = Rent::get()?.minimum_balance(TicketState::SIZE);
        let held = ticket_state_info.lamports();
        if held < required {
            // SystemInstruction::Transfer (bincode): tag u32 + lamports.
            let mut data = Vec::with_capacity(12);
            data.extend_from_slice(&2u32.to_le_bytes());
            data.extend_from_slice(&(required - held).to_le_bytes());
            let ix = Instruction {
                program_id: SYSTEM_PROGRAM_ID,
                accounts: vec![
                    AccountMeta::new(*payer_info.key, true),
                    AccountMeta::new(*ticket_state_info.key, false),
                ],
                data,
            };
            invoke(
                &ix,
                &[
                    payer_info.clone(),
                    ticket_state_info.clone(),
                    system_program.clone(),
                ],
            )?;
        }
        ticket_state_info.resize(TicketState::SIZE)?;
    } else {
        create_pda(
            ticket_state_info,
            payer_info,
            system_program,
            TicketState::SIZE,
            &[TICKET_STATE_SEED, &[bump]],
        )?;
    }
    write_state(
        ticket_state_info,
        &TicketState {
            discriminator: TICKET_STATE_DISCRIMINATOR,
            epoch: 0,
            total_tickets: 0,
            pending_epoch: 0,
            pending_shares: [0; MOTHERLODE_WINNERS],
            pending_tickets: [0; MOTHERLODE_WINNERS],
            lifetime_tickets: 0,
            bump: bump as u64,
        },
    )
}

use miner_api::{error::MinerError, state::*};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
};

use crate::loaders::*;

/// Garbage-collects a stale TicketBatch (permissionless): the batch's
/// epoch must be over (a strike ran the draw and bumped the epoch), and
/// if that epoch's draw is still pending settlement no batch of it can be
/// closed - the winning one must survive until SettleTicketWin. The rent
/// always goes back to the batch wallet, whoever cranks this.
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    let [ticket_state_info, ticket_batch_info, batch_wallet_info] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    expect_program_account(ticket_state_info, TICKET_STATE_DISCRIMINATOR)?;
    let tickets = read_state::<TicketState>(ticket_state_info)?;

    expect_writable(ticket_batch_info)?;
    expect_program_account(ticket_batch_info, TICKET_BATCH_DISCRIMINATOR)?;
    let batch = read_state::<TicketBatch>(ticket_batch_info)?;

    if batch.epoch >= tickets.epoch {
        return Err(MinerError::TicketBatchActive.into());
    }
    // Any unsettled slot of the batch's epoch protects the whole epoch:
    // the winning batch must survive until SettleTicketWin (and being
    // lenient about the losers costs nothing - the epoch settles fast).
    if batch.epoch == tickets.pending_epoch
        && tickets.pending_shares.iter().any(|&s| s > 0)
    {
        return Err(MinerError::TicketBatchActive.into());
    }

    expect_writable(batch_wallet_info)?;
    if batch_wallet_info.key.to_bytes() != batch.wallet {
        return Err(MinerError::InvalidAccount.into());
    }

    close_program_account(ticket_batch_info, batch_wallet_info)
}

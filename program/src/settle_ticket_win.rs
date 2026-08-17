use miner_api::{consts::*, error::MinerError, pda, state::*};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

use crate::loaders::*;

/// Delivers pending ticket-draw shares (permissionless; the crank calls
/// it right after a strike). A strike can hand several winner slots to
/// tickets: this settles every pending slot whose drawn index the batch
/// covers, in one go. The summed shares move into the batch wallet's Win
/// PDA exactly like a mining strike share (created here if needed, rent
/// from the payer; an existing unclaimed Win simply accumulates). The
/// batch closes with its rent going back to the batch wallet.
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    let [payer_info, ticket_state_info, motherlode_info, ticket_batch_info, batch_wallet_info, win_info, system_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(payer_info)?;

    expect_writable(ticket_state_info)?;
    expect_program_account(ticket_state_info, TICKET_STATE_DISCRIMINATOR)?;
    let mut tickets = read_state::<TicketState>(ticket_state_info)?;

    expect_key(motherlode_info, &pda::motherlode_pda().0)?;
    expect_writable(motherlode_info)?;
    expect_program_account(motherlode_info, MOTHERLODE_DISCRIMINATOR)?;
    let mut motherlode = read_state::<Motherlode>(motherlode_info)?;

    expect_writable(ticket_batch_info)?;
    expect_program_account(ticket_batch_info, TICKET_BATCH_DISCRIMINATOR)?;
    let batch = read_state::<TicketBatch>(ticket_batch_info)?;

    expect_writable(batch_wallet_info)?;
    if batch_wallet_info.key.to_bytes() != batch.wallet {
        return Err(MinerError::InvalidAccount.into());
    }

    // Collect every pending slot this batch covers; zero them in place.
    let mut amount: u64 = 0;
    if batch.epoch == tickets.pending_epoch {
        let end = batch.start.saturating_add(batch.count);
        for slot in 0..MOTHERLODE_WINNERS {
            let share = tickets.pending_shares[slot];
            let index = tickets.pending_tickets[slot];
            if share > 0 && index >= batch.start && index < end {
                amount = amount.checked_add(share).ok_or(MinerError::Overflow)?;
                tickets.pending_shares[slot] = 0;
                // Backfill the winner into the strike record so the UI
                // shows the wallet like any mining winner. Only a slot
                // still holding the zero-key placeholder is touched: a
                // later hash-only strike may have overwritten the record,
                // and that one must not be corrupted.
                if motherlode.last_winners[slot] == [0u8; 32] {
                    motherlode.last_winners[slot] = batch.wallet;
                }
            }
        }
    }
    if amount == 0 {
        return Err(MinerError::NoPendingTicketWin.into());
    }
    write_state(motherlode_info, &motherlode)?;

    let winner = Pubkey::new_from_array(batch.wallet);
    let (win_key, win_bump) = pda::win_pda(&winner);
    expect_key(win_info, &win_key)?;
    expect_writable(win_info)?;
    let now = Clock::get()?.unix_timestamp;
    if win_info.owner.ne(&miner_api::id()) || win_info.data_is_empty() {
        create_pda(
            win_info,
            payer_info,
            system_program,
            Win::SIZE,
            &[WIN_SEED, batch.wallet.as_ref(), &[win_bump]],
        )?;
        write_state(
            win_info,
            &Win {
                discriminator: WIN_DISCRIMINATOR,
                authority: batch.wallet,
                amount,
                since_ts: now,
                bump: win_bump as u64,
            },
        )?;
    } else {
        expect_program_account(win_info, WIN_DISCRIMINATOR)?;
        let mut win = read_state::<Win>(win_info)?;
        win.amount = win.amount.checked_add(amount).ok_or(MinerError::Overflow)?;
        write_state(win_info, &win)?;
    }

    write_state(ticket_state_info, &tickets)?;

    // The winning batch is spent (every pending slot it covered has just
    // been settled): close it, rent back to the buyer.
    close_program_account(ticket_batch_info, batch_wallet_info)
}

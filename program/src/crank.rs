use miner_api::{consts::*, error::MinerError, pda, state::*};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, keccak,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

use crate::loaders::*;

/// Permissionless crank: closes the current round (implicitly, in that it stops
/// being current) and opens the next one. Anyone can call it; the caller
/// pays rent for the new round account and recovers it after retention
/// via close_round.
///
/// Motherlode duties on top: the closing round contributes MOTHERLODE_BPS
/// of its full budget to the pot (only if it had any weight; empty rounds
/// keep burning everything), and the strike roll runs with 1/MOTHERLODE_ODDS
/// probability. On a hit the pot splits evenly across the
/// MOTHERLODE_WINNERS candidate slots: each share moves into that
/// candidate's Win account (created here if needed, rent from the payer;
/// a wallet holding several slots receives several shares into the same
/// account) and the pot restarts; later strikes are never paused by
/// unclaimed wins.
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    // 7 fixed accounts + one Win PDA per candidate slot (slot order) + an
    // optional trailing TicketState. With the ticket account a strike
    // draws each of the MOTHERLODE_WINNERS slots from hashes + tickets
    // chances: mined hashes keep their usual odds and every sold ticket
    // is one extra chance. A slot that lands on a ticket has its share
    // reserved on the TicketState (delivered by SettleTicketWin); the
    // draw consumes the epoch's tickets either way. Without the account
    // - or with no tickets sold, or with a previous draw still unsettled
    // - the split works exactly as before tickets existed.
    if accounts.len() < 7 + MOTHERLODE_WINNERS
        || accounts.len() > 7 + MOTHERLODE_WINNERS + 1
    {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let (fixed_accounts, rest) = accounts.split_at(7);
    let (win_infos, ticket_rest) = rest.split_at(MOTHERLODE_WINNERS);
    let ticket_state_info = ticket_rest.first();
    let [payer_info, config_info, new_round_info, system_program, closing_round_info, motherlode_info, slot_hashes_info] =
        fixed_accounts
    else {
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

    // The closing round: pool accrual + the strike roll below.
    expect_program_account(closing_round_info, ROUND_DISCRIMINATOR)?;
    let closing_round = read_state::<Round>(closing_round_info)?;
    if closing_round.index != config.current_round {
        return Err(MinerError::RoundMismatch.into());
    }

    expect_writable(motherlode_info)?;
    expect_program_account(motherlode_info, MOTHERLODE_DISCRIMINATOR)?;
    let mut motherlode = read_state::<Motherlode>(motherlode_info)?;

    // Pot accrual: the round's budget already stores the miners' 90% cut
    // (see the new round below), the pot takes the withheld remainder.
    // Only rounds that actually had weight contribute; the emission of an
    // empty round lapses entirely, exactly as before.
    if closing_round.total_weight > 0 {
        motherlode.pot = motherlode
            .pot
            .checked_add(motherlode_tithe(closing_round.budget))
            .ok_or(MinerError::Overflow)?;
    }

    // The strike roll: 1/MOTHERLODE_ODDS per close, entropy from the latest slot
    // hash (unknown when the round's hashes were submitted). Skipped when
    // the closing round had no hashes or the pool is empty.
    if motherlode.round_index == closing_round.index
        && motherlode.hashes > 0
        && motherlode.pot > 0
    {
        let entropy = slot_hashes_entropy(slot_hashes_info)?;
        let strike = keccak::hashv(&[
            entropy.as_slice(),
            &closing_round.index.to_le_bytes(),
            &motherlode.hashes.to_le_bytes(),
        ]);
        let roll = u64::from_le_bytes(strike.as_ref()[..8].try_into().unwrap());
        if roll % MOTHERLODE_ODDS == 0 {
            // Tickets join the draw only when the account was passed,
            // any were sold this epoch, and no earlier draw is still
            // pending (a pending winner is never overwritten; that strike
            // simply pays the mining candidates and tickets roll over).
            let mut tickets: Option<TicketState> = None;
            if let Some(info) = ticket_state_info {
                // Tolerated as a no-op before InitTickets ever ran (the
                // singleton does not exist yet), so the program upgrade
                // and the crank client can roll out in any order.
                if info.owner.eq(&miner_api::id())
                    && info.data_len() == TicketState::SIZE
                {
                    expect_writable(info)?;
                    expect_program_account(info, TICKET_STATE_DISCRIMINATOR)?;
                    let ts = read_state::<TicketState>(info)?;
                    if ts.total_tickets > 0
                        && ts.pending_shares.iter().all(|&s| s == 0)
                    {
                        tickets = Some(ts);
                    }
                }
            }
            let ticket_count = tickets.as_ref().map_or(0, |ts| ts.total_tickets);
            // Every mined hash and every ticket is one chance; each of
            // the MOTHERLODE_WINNERS slots draws independently from the
            // combined pool (hashes > 0 is guaranteed by the outer if).
            let chances = motherlode
                .hashes
                .checked_add(ticket_count)
                .ok_or(MinerError::Overflow)?;
            // Even split; the division remainder (dust of at most
            // MOTHERLODE_WINNERS - 1 native units) goes to the first slot.
            let share = motherlode.pot / MOTHERLODE_WINNERS as u64;
            let remainder = motherlode.pot - share * MOTHERLODE_WINNERS as u64;
            let mut last_winners = [[0u8; 32]; MOTHERLODE_WINNERS];
            for slot in 0..MOTHERLODE_WINNERS {
                let amount = if slot == 0 { share + remainder } else { share };
                if amount == 0 {
                    continue;
                }
                // Per-slot draw from the strike hash (bytes 8.., disjoint
                // from the roll in bytes 0..8).
                let off = 8 + 8 * slot;
                let draw = u64::from_le_bytes(
                    strike.as_ref()[off..off + 8].try_into().unwrap(),
                ) % chances;
                if draw >= motherlode.hashes {
                    // The slot lands on a ticket: reserve the share for
                    // SettleTicketWin (the buyer is unknown on-chain here;
                    // the batch covering the index delivers it). Unwrap is
                    // safe: draw >= hashes implies ticket_count > 0.
                    let ts = tickets.as_mut().unwrap();
                    ts.pending_shares[slot] = amount;
                    ts.pending_tickets[slot] = draw - motherlode.hashes;
                    continue;
                }
                let win_info = &win_infos[slot];
                let candidate_bytes = motherlode.candidates[slot];
                let candidate = Pubkey::new_from_array(candidate_bytes);
                let (win_key, win_bump) = pda::win_pda(&candidate);
                expect_key(win_info, &win_key)?;
                expect_writable(win_info)?;
                last_winners[slot] = candidate_bytes;
                // A wallet holding several slots appears here several
                // times: the first pass creates its Win, the later ones
                // fall into the accumulate branch (the AccountInfos alias
                // the same account data).
                if win_info.owner.ne(&miner_api::id()) || win_info.data_is_empty() {
                    create_pda(
                        win_info,
                        payer_info,
                        system_program,
                        Win::SIZE,
                        &[WIN_SEED, candidate_bytes.as_ref(), &[win_bump]],
                    )?;
                    write_state(
                        win_info,
                        &Win {
                            discriminator: WIN_DISCRIMINATOR,
                            authority: candidate_bytes,
                            amount,
                            since_ts: now,
                            bump: win_bump as u64,
                        },
                    )?;
                } else {
                    // Striking again before the previous win was claimed
                    // just adds to it; the original since_ts is kept.
                    expect_program_account(win_info, WIN_DISCRIMINATOR)?;
                    let mut win = read_state::<Win>(win_info)?;
                    win.amount = win
                        .amount
                        .checked_add(amount)
                        .ok_or(MinerError::Overflow)?;
                    write_state(win_info, &win)?;
                }
            }
            // The draw consumed the epoch's tickets (won or not): sales
            // restart at zero and stale batches become garbage-collectable
            // once any pending draws settle.
            if let Some(mut ts) = tickets {
                if ts.pending_shares.iter().any(|&s| s > 0) {
                    ts.pending_epoch = ts.epoch;
                }
                ts.epoch = ts.epoch.checked_add(1).ok_or(MinerError::Overflow)?;
                ts.total_tickets = 0;
                // Unwrap is safe: tickets is only Some when the info was.
                write_state(ticket_state_info.unwrap(), &ts)?;
            }
            // Ticket-won slots stay zeroed in last_winners (the wallet is
            // only known at settle time).
            motherlode.last_winners = last_winners;
            motherlode.last_win_amount = share;
            motherlode.last_win_ts = now;
            motherlode.pot = 0;
        }
    }
    write_state(motherlode_info, &motherlode)?;

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
            // Halving-aware budget, frozen for the round's lifetime like
            // before (later halvings never touch already-open rounds).
            // The miners' cut is 90%: the Motherlode share is withheld here
            // and credited to the pot when this round closes, so all the
            // lazy settlement math stays untouched.
            budget: miners_budget(round_budget_at(config.round_seconds, now)),
            start_ts: now,
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

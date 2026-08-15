use miner_api::{consts::*, error::MinerError, pda, state::*};
use solana_program::{
    account_info::AccountInfo,
    clock::Clock,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    keccak,
    program::invoke,
    program_error::ProgramError,
    sysvar::Sysvar,
};

use crate::loaders::*;

/// Stake SOL and/or $MINER on a tunnel in the current game round. One
/// entry PDA per wallet per round holds per-tunnel stakes: spreading a
/// round's stake across several tunnels (the hedge) is part of the game,
/// each call adds to one tunnel.
///
/// The stake value (the weight) is sol + miner * ema, in lamports; the EMA
/// only moves between rounds, so every entry of a round is valued at the
/// same price. Entries close at the round deadline even before the settle
/// runs, so nobody can enter once the entropy slot is near. Creating the
/// entry buys one players' Motherlode ticket (an independent reservoir
/// sample per candidate slot, one ticket per wallet per round regardless
/// of stake); top-ups do not recount.
pub fn process(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority_info, game_info, round_info, entry_info, vault_info, user_token_info, game_token_info, config_info, slot_hashes_info, token_program_info, system_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() != 17 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let tunnel = data[0] as usize;
    let sol = u64::from_le_bytes(data[1..9].try_into().unwrap());
    let miner = u64::from_le_bytes(data[9..17].try_into().unwrap());
    if tunnel >= GAME_TUNNELS {
        return Err(MinerError::InvalidTunnel.into());
    }

    expect_signer(authority_info)?;
    expect_program_account(game_info, GAME_DISCRIMINATOR)?;
    let mut game = read_state::<Game>(game_info)?;
    expect_program_account(config_info, CONFIG_DISCRIMINATOR)?;
    let config = read_state::<Config>(config_info)?;

    expect_writable(round_info)?;
    expect_program_account(round_info, GAME_ROUND_DISCRIMINATOR)?;
    let mut round = read_state::<GameRound>(round_info)?;
    let now = Clock::get()?.unix_timestamp;
    if round.index != game.current_round
        || round.settled != GAME_ROUND_OPEN
        // Not open yet (the between-rounds intermission) or already past
        // the deadline.
        || now < game.round_start_ts
        || now >= game.round_start_ts.saturating_add(game.round_seconds as i64)
    {
        return Err(MinerError::GameRoundClosed.into());
    }

    // Stake value in lamports; both legs checked for overflow.
    let miner_value = ((miner as u128) * (game.ema_lamports_per_token as u128)
        / (ONE_TOKEN as u128)) as u64;
    let weight = sol
        .checked_add(miner_value)
        .ok_or(MinerError::Overflow)?;
    if weight < GAME_MIN_WEIGHT {
        return Err(MinerError::InvalidStake.into());
    }

    // Vaults.
    let (vault_key, _) = pda::game_vault_pda();
    expect_key(vault_info, &vault_key)?;
    expect_writable(vault_info)?;
    let game_key = pda::game_pda().0;
    expect_key(
        game_token_info,
        &pda::ata(&game_key, &solana_program::pubkey::Pubkey::new_from_array(config.mint)),
    )?;
    expect_writable(game_token_info)?;
    if game_token_info.data_is_empty() {
        return Err(MinerError::InvalidTokenAccount.into());
    }
    read_token_balance(game_token_info, &config.mint, &game_key.to_bytes())?;
    expect_key(token_program_info, &SPL_TOKEN_PROGRAM_ID)?;

    let authority = authority_info.key.to_bytes();

    // The entry: create or top up (same tunnel only).
    let (entry_key, entry_bump) =
        pda::game_entry_pda(round.index, authority_info.key);
    expect_key(entry_info, &entry_key)?;
    expect_writable(entry_info)?;
    let mut entry = if entry_info.data_is_empty() {
        create_pda(
            entry_info,
            authority_info,
            system_program,
            GameEntry::SIZE,
            &[
                GAME_ENTRY_SEED,
                &round.index.to_le_bytes(),
                authority.as_ref(),
                &[entry_bump],
            ],
        )?;

        // One players' Motherlode ticket per wallet per round: each
        // candidate slot runs its own reservoir sample with a uniform
        // 1/n chance over the round's wallets. Entropy from slot hashes
        // + the wallet + the running count.
        round.entries = round.entries.checked_add(1).ok_or(MinerError::Overflow)?;
        let entropy = slot_hashes_entropy(slot_hashes_info)?;
        for slot in 0..GAME_MOTHERLODE_WINNERS {
            let sample = keccak::hashv(&[
                entropy.as_slice(),
                authority.as_slice(),
                &round.entries.to_le_bytes(),
                &[slot as u8],
            ]);
            let roll = u64::from_le_bytes(sample.as_ref()[..8].try_into().unwrap());
            if roll % round.entries == 0 {
                round.candidates[slot] = authority;
            }
        }

        GameEntry {
            discriminator: GAME_ENTRY_DISCRIMINATOR,
            authority,
            round: round.index,
            sol: [0; GAME_TUNNELS],
            miner: [0; GAME_TUNNELS],
            weight: [0; GAME_TUNNELS],
            bump: entry_bump as u64,
        }
    } else {
        expect_program_account(entry_info, GAME_ENTRY_DISCRIMINATOR)?;
        let existing = read_state::<GameEntry>(entry_info)?;
        if existing.authority != authority {
            return Err(MinerError::Unauthorized.into());
        }
        existing
    };

    // Move the stakes into the vaults.
    if sol > 0 {
        // SystemInstruction::Transfer (bincode): tag u32 + lamports u64.
        let mut ix_data = Vec::with_capacity(12);
        ix_data.extend_from_slice(&2u32.to_le_bytes());
        ix_data.extend_from_slice(&sol.to_le_bytes());
        let ix = Instruction {
            program_id: SYSTEM_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(*authority_info.key, true),
                AccountMeta::new(*vault_info.key, false),
            ],
            data: ix_data,
        };
        invoke(
            &ix,
            &[
                authority_info.clone(),
                vault_info.clone(),
                system_program.clone(),
            ],
        )?;
    }
    if miner > 0 {
        expect_writable(user_token_info)?;
        if user_token_info.data_is_empty() {
            return Err(MinerError::InvalidTokenAccount.into());
        }
        read_token_balance(user_token_info, &config.mint, &authority)?;
        // CPI: spl_token::transfer user -> game vault, signed by the authority.
        let mut ix_data = Vec::with_capacity(9);
        ix_data.push(SPL_TOKEN_TRANSFER_IX);
        ix_data.extend_from_slice(&miner.to_le_bytes());
        let ix = Instruction {
            program_id: SPL_TOKEN_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(*user_token_info.key, false),
                AccountMeta::new(*game_token_info.key, false),
                AccountMeta::new_readonly(*authority_info.key, true),
            ],
            data: ix_data,
        };
        invoke(
            &ix,
            &[
                user_token_info.clone(),
                game_token_info.clone(),
                authority_info.clone(),
            ],
        )?;
    }

    // Bookkeeping.
    entry.sol[tunnel] = entry.sol[tunnel]
        .checked_add(sol)
        .ok_or(MinerError::Overflow)?;
    entry.miner[tunnel] = entry.miner[tunnel]
        .checked_add(miner)
        .ok_or(MinerError::Overflow)?;
    entry.weight[tunnel] = entry.weight[tunnel]
        .checked_add(weight)
        .ok_or(MinerError::Overflow)?;
    write_state(entry_info, &entry)?;

    round.sol[tunnel] = round.sol[tunnel].checked_add(sol).ok_or(MinerError::Overflow)?;
    round.miner[tunnel] =
        round.miner[tunnel].checked_add(miner).ok_or(MinerError::Overflow)?;
    round.weight[tunnel] =
        round.weight[tunnel].checked_add(weight).ok_or(MinerError::Overflow)?;
    write_state(round_info, &round)?;

    expect_writable(game_info)?;
    game.total_volume_sol = game
        .total_volume_sol
        .checked_add(sol)
        .ok_or(MinerError::Overflow)?;
    game.total_volume_miner = game
        .total_volume_miner
        .checked_add(miner)
        .ok_or(MinerError::Overflow)?;
    write_state(game_info, &game)?;

    Ok(())
}

use miner_api::{consts::*, error::MinerError, pda, state::*};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::loaders::*;

/// Claim a settled game entry. The entry holds per-tunnel stakes; the
/// payout sums over the tunnels:
/// - surviving tunnel: the stake back + a pro-rata share of the collapsed
///   pot (payout * entry.weight[t] / survivor_weight, per asset),
/// - the collapsed tunnel: nothing,
/// - void round: full refund of every stake.
/// Closes the entry PDA either way (rent back to the authority). Must be
/// signed by the authority itself.
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    let [authority_info, game_info, round_info, entry_info, vault_info, user_token_info, game_token_info, token_program_info] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(authority_info)?;
    expect_program_account(game_info, GAME_DISCRIMINATOR)?;
    let game = read_state::<Game>(game_info)?;

    expect_writable(entry_info)?;
    expect_program_account(entry_info, GAME_ENTRY_DISCRIMINATOR)?;
    let entry = read_state::<GameEntry>(entry_info)?;
    if entry.authority != authority_info.key.to_bytes() {
        return Err(MinerError::Unauthorized.into());
    }

    expect_program_account(round_info, GAME_ROUND_DISCRIMINATOR)?;
    let round = read_state::<GameRound>(round_info)?;
    if round.index != entry.round {
        return Err(MinerError::RoundMismatch.into());
    }
    if round.settled == GAME_ROUND_OPEN {
        return Err(MinerError::GameNotSettled.into());
    }

    let mut sol: u64 = 0;
    let mut miner: u64 = 0;
    for t in 0..GAME_TUNNELS {
        if round.settled == GAME_ROUND_VOID {
            // No collapse happened: the stake refunds in full.
            sol = sol.checked_add(entry.sol[t]).ok_or(MinerError::Overflow)?;
            miner = miner
                .checked_add(entry.miner[t])
                .ok_or(MinerError::Overflow)?;
        } else if round.collapsed & (1u64 << t) != 0 {
            // This stake fell with the tunnel (`collapsed` is a bitmask).
        } else if entry.weight[t] > 0 {
            // The stake back + a pro-rata share of the 90% payout.
            let share = |payout: u64| -> Result<u64, MinerError> {
                Ok(((payout as u128)
                    .checked_mul(entry.weight[t] as u128)
                    .ok_or(MinerError::Overflow)?
                    / (round.survivor_weight as u128)) as u64)
            };
            sol = sol
                .checked_add(entry.sol[t])
                .and_then(|v| v.checked_add(share(round.payout_sol).ok()?))
                .ok_or(MinerError::Overflow)?;
            miner = miner
                .checked_add(entry.miner[t])
                .and_then(|v| v.checked_add(share(round.payout_miner).ok()?))
                .ok_or(MinerError::Overflow)?;
        }
    }

    if sol > 0 || miner > 0 {
        let (vault_key, _) = pda::game_vault_pda();
        expect_key(vault_info, &vault_key)?;
        expect_writable(vault_info)?;
        expect_key(token_program_info, &SPL_TOKEN_PROGRAM_ID)?;
        if miner > 0 {
            let game_key = pda::game_pda().0;
            // The vault carries the mint, so matching the user account
            // against the vault's mint pins both to config.mint.
            let mint = {
                let data = game_token_info.try_borrow_data()?;
                if game_token_info.owner.ne(&SPL_TOKEN_PROGRAM_ID) || data.len() < 72 {
                    return Err(MinerError::InvalidTokenAccount.into());
                }
                let mint: [u8; 32] = data[0..32].try_into().unwrap();
                mint
            };
            expect_key(
                game_token_info,
                &pda::ata(&game_key, &Pubkey::new_from_array(mint)),
            )?;
            expect_writable(game_token_info)?;
            expect_writable(user_token_info)?;
            if user_token_info.data_is_empty() {
                return Err(MinerError::InvalidTokenAccount.into());
            }
            read_token_balance(user_token_info, &mint, &entry.authority)?;
        }
        game_payout(
            game_info,
            game.bump as u8,
            vault_info,
            game_token_info,
            user_token_info,
            authority_info,
            sol,
            miner,
        )?;
    }

    // Close the entry (rent back to the authority).
    close_program_account(entry_info, authority_info)
}

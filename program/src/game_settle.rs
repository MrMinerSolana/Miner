use miner_api::{consts::*, error::MinerError, pda, state::*};
use solana_program::{
    account_info::AccountInfo,
    clock::Clock,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    keccak,
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::Sysvar,
};

use crate::loaders::*;

/// Spot price from the cp-amm pool, in lamports per whole token. Both
/// mints have 9 decimals, so the Q64.64 sqrt_price squares directly into
/// the token-per-token price; the direction depends on which side is
/// WSOL. Returns InvalidPool when the account is not the MINER/WSOL pool.
#[cfg(not(feature = "game-fixed-price"))]
fn pool_spot(pool_info: &AccountInfo, mint: &[u8; 32]) -> Result<u64, ProgramError> {
    let data = pool_info.try_borrow_data()?;
    if data.len() < CP_AMM_SQRT_PRICE_OFFSET + 16 {
        return Err(MinerError::InvalidPool.into());
    }
    let token_a: [u8; 32] = data[CP_AMM_TOKEN_A_OFFSET..CP_AMM_TOKEN_A_OFFSET + 32]
        .try_into()
        .unwrap();
    let token_b: [u8; 32] = data[CP_AMM_TOKEN_B_OFFSET..CP_AMM_TOKEN_B_OFFSET + 32]
        .try_into()
        .unwrap();
    let sp = u128::from_le_bytes(
        data[CP_AMM_SQRT_PRICE_OFFSET..CP_AMM_SQRT_PRICE_OFFSET + 16]
            .try_into()
            .unwrap(),
    );
    if sp == 0 || sp >= 1u128 << 96 {
        return Err(MinerError::InvalidPool.into());
    }
    // price of token B per token A in Q64.64: (sp / 2^64)^2 * 2^64.
    let s = sp >> 32;
    let price_q64 = s.checked_mul(s).ok_or(MinerError::InvalidPool)?;
    if price_q64 == 0 {
        return Err(MinerError::InvalidPool.into());
    }
    let wsol = WSOL_MINT.to_bytes();
    let spot = if token_a == *mint && token_b == wsol {
        // price_q64 = SOL per MINER; scale to lamports per whole token.
        price_q64
            .checked_mul(1_000_000_000)
            .ok_or(MinerError::InvalidPool)?
            >> 64
    } else if token_a == wsol && token_b == *mint {
        // price_q64 = MINER per SOL; invert.
        ((1_000_000_000u128) << 64) / price_q64
    } else {
        return Err(MinerError::InvalidPool.into());
    };
    u64::try_from(spot).map_err(|_| MinerError::InvalidPool.into())
}

/// Closes the current game round and opens the next (permissionless
/// crank, mirrors the mining crank).
///
/// GAME_COLLAPSES tunnels cave in, drawn uniformly among ALL of the
/// tunnels regardless of stakes (entropy from slot hashes, which was
/// unknown while entries were open - they close at the deadline, before
/// the settle). Who is inside plays no part in what falls. The collapsed
/// pots split 90/5/5: survivors (pro-rata at claim), burn ($MINER side
/// burns here; the SOL side routes to the fee wallet and burns through
/// the daily buyback), players' Motherlode. `collapsed` stores the
/// tunnels as a bitmask.
///
/// When no staked tunnel survives the draw there is nobody to pay: the
/// whole collapsed pot goes to buyback/burn ($MINER burns on the spot,
/// SOL routes to the fee wallet). A round with nothing staked at all is
/// void.
///
/// The next round opens GAME_INTERMISSION_SECONDS after the settle, so
/// there is a beat between rounds to read the result.
///
/// The players' Motherlode strike rolls on every settle of a round that
/// had entries: with 1/GAME_MOTHERLODE_ODDS probability both pools split
/// evenly across the candidates' GameWin accounts (created here if
/// needed, rent from the payer).
///
/// The price EMA updates once per settle from the pool spot, clamped to
/// GAME_EMA_CLAMP_BPS, so moving the valuation requires holding a pumped
/// pool across many rounds.
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    // 13 fixed accounts + one GameWin PDA per candidate slot, slot order.
    if accounts.len() != 13 + GAME_MOTHERLODE_WINNERS {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let (fixed, win_infos) = accounts.split_at(13);
    let [payer_info, game_info, closing_info, new_round_info, vault_info, game_token_info, config_info, mint_info, fee_wallet_info, pool_info, slot_hashes_info, token_program_info, system_program] =
        fixed
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(payer_info)?;
    expect_writable(game_info)?;
    expect_program_account(game_info, GAME_DISCRIMINATOR)?;
    let mut game = read_state::<Game>(game_info)?;
    expect_program_account(config_info, CONFIG_DISCRIMINATOR)?;
    let config = read_state::<Config>(config_info)?;

    let now = Clock::get()?.unix_timestamp;
    let round_seconds = game.round_seconds as i64;
    if now < game.round_start_ts.saturating_add(round_seconds) {
        return Err(MinerError::GameRoundStillOpen.into());
    }

    // Devnet-only migration: a round account written under a previous
    // tunnel-count layout cannot be parsed any more. It must be empty
    // (all its entries claimed before the upgrade); it just closes and
    // makes way for a fresh round in the current format.
    #[cfg(feature = "short-game")]
    if closing_info.data_len() != GameRound::SIZE {
        expect_writable(closing_info)?;
        expect_program_account(closing_info, GAME_ROUND_DISCRIMINATOR)?;
        let new_index = game
            .current_round
            .checked_add(1)
            .ok_or(MinerError::Overflow)?;
        let (new_round_key, new_round_bump) = pda::game_round_pda(new_index);
        expect_key(new_round_info, &new_round_key)?;
        create_pda(
            new_round_info,
            payer_info,
            system_program,
            GameRound::SIZE,
            &[GAME_ROUND_SEED, &new_index.to_le_bytes(), &[new_round_bump]],
        )?;
        write_state(
            new_round_info,
            &GameRound {
                discriminator: GAME_ROUND_DISCRIMINATOR,
                index: new_index,
                start_ts: now,
                sol: [0; GAME_TUNNELS],
                miner: [0; GAME_TUNNELS],
                weight: [0; GAME_TUNNELS],
                entries: 0,
                candidates: [[0u8; 32]; GAME_MOTHERLODE_WINNERS],
                settled: GAME_ROUND_OPEN,
                collapsed: 0,
                payout_sol: 0,
                payout_miner: 0,
                survivor_weight: 0,
            },
        )?;
        game.current_round = new_index;
        game.round_start_ts = now;
        write_state(game_info, &game)?;
        // Close last: direct lamport moves after the CPIs, so every CPI
        // sees a balanced account set.
        close_program_account(closing_info, payer_info)?;
        return Ok(());
    }

    expect_writable(closing_info)?;
    expect_program_account(closing_info, GAME_ROUND_DISCRIMINATOR)?;
    let mut closing = read_state::<GameRound>(closing_info)?;
    if closing.index != game.current_round || closing.settled != GAME_ROUND_OPEN {
        return Err(MinerError::RoundMismatch.into());
    }

    // Vaults and CPI accounts.
    let (vault_key, _) = pda::game_vault_pda();
    expect_key(vault_info, &vault_key)?;
    expect_writable(vault_info)?;
    let game_key = pda::game_pda().0;
    let mint = Pubkey::new_from_array(config.mint);
    expect_key(game_token_info, &pda::ata(&game_key, &mint))?;
    expect_writable(game_token_info)?;
    expect_key(mint_info, &mint)?;
    expect_key(fee_wallet_info, &FEE_WALLET)?;
    expect_key(token_program_info, &SPL_TOKEN_PROGRAM_ID)?;
    expect_key(pool_info, &Pubkey::new_from_array(game.pool))?;

    // EMA update: once per settle, clamped. Under the devnet rehearsal
    // feature the EMA simply keeps its init value (no pool exists there).
    #[cfg(not(feature = "game-fixed-price"))]
    {
        let spot = pool_spot(pool_info, &config.mint)?;
        let ema = game.ema_lamports_per_token;
        let stepped = if spot >= ema {
            ema + (spot - ema) / GAME_EMA_ALPHA
        } else {
            ema - (ema - spot) / GAME_EMA_ALPHA
        };
        let cap_up = ((ema as u128) * ((BPS_DENOM + GAME_EMA_CLAMP_BPS) as u128)
            / (BPS_DENOM as u128)) as u64;
        let cap_down = ((ema as u128) * ((BPS_DENOM - GAME_EMA_CLAMP_BPS) as u128)
            / (BPS_DENOM as u128)) as u64;
        game.ema_lamports_per_token = stepped.clamp(cap_down.max(1), cap_up);
    }

    let entropy = slot_hashes_entropy(slot_hashes_info)?;

    // The collapse: GAME_COLLAPSES tunnels drawn uniformly among ALL of
    // the tunnels, staked or empty. A round with nothing staked is void.
    let total_weight: u128 = closing.weight.iter().map(|w| *w as u128).sum();
    let staked_tunnels = closing.weight.iter().filter(|w| **w > 0).count();
    if staked_tunnels >= 1 {
        // Draw without replacement: each pick hashes the entropy with the
        // pick number and takes one of the remaining tunnels. Stakes play
        // no part in what falls.
        let mut avail: Vec<usize> = (0..GAME_TUNNELS).collect();
        let mut collapsed_mask: u64 = 0;
        for pick in 0..GAME_COLLAPSES {
            let roll_hash = keccak::hashv(&[
                entropy.as_slice(),
                &closing.index.to_le_bytes(),
                &(pick as u64).to_le_bytes(),
            ]);
            let roll = u64::from_le_bytes(roll_hash.as_ref()[..8].try_into().unwrap());
            let idx = avail.remove(roll as usize % avail.len());
            collapsed_mask |= 1u64 << idx;
        }

        let mut pot_sol: u64 = 0;
        let mut pot_miner: u64 = 0;
        let mut dead_weight: u128 = 0;
        for i in 0..GAME_TUNNELS {
            if collapsed_mask & (1u64 << i) != 0 {
                pot_sol = pot_sol.checked_add(closing.sol[i]).ok_or(MinerError::Overflow)?;
                pot_miner = pot_miner
                    .checked_add(closing.miner[i])
                    .ok_or(MinerError::Overflow)?;
                dead_weight += closing.weight[i] as u128;
            }
        }
        let survivor_weight = total_weight - dead_weight;
        let cut = |pot: u64, bps: u64| ((pot as u128) * (bps as u128) / (BPS_DENOM as u128)) as u64;
        // No staked survivor: nobody to pay, the whole pot goes to
        // buyback/burn instead of the 90/5/5 split.
        let (burn_miner, ml_miner, fee_sol, ml_sol) = if survivor_weight > 0 {
            (
                cut(pot_miner, GAME_BURN_BPS),
                cut(pot_miner, GAME_MOTHERLODE_BPS),
                cut(pot_sol, GAME_BURN_BPS),
                cut(pot_sol, GAME_MOTHERLODE_BPS),
            )
        } else {
            (pot_miner, 0, pot_sol, 0)
        };

        // Burn the $MINER rake straight from the vault (a real, visible
        // SPL Burn), signed by the game PDA (the vault's owner).
        if burn_miner > 0 {
            let mut ix_data = Vec::with_capacity(9);
            ix_data.push(SPL_TOKEN_BURN_IX);
            ix_data.extend_from_slice(&burn_miner.to_le_bytes());
            let burn_ix = Instruction {
                program_id: SPL_TOKEN_PROGRAM_ID,
                accounts: vec![
                    AccountMeta::new(*game_token_info.key, false),
                    AccountMeta::new(mint, false),
                    AccountMeta::new_readonly(game_key, true),
                ],
                data: ix_data,
            };
            invoke_signed(
                &burn_ix,
                &[
                    game_token_info.clone(),
                    mint_info.clone(),
                    game_info.clone(),
                ],
                &[&[GAME_SEED, &[game.bump as u8]]],
            )?;
        }

        // The SOL rake routes to the fee wallet and burns through the
        // daily buyback, together with the mining fees.
        if fee_sol > 0 {
            **vault_info.try_borrow_mut_lamports()? = vault_info
                .lamports()
                .checked_sub(fee_sol)
                .ok_or(MinerError::Overflow)?;
            **fee_wallet_info.try_borrow_mut_lamports()? = fee_wallet_info
                .lamports()
                .checked_add(fee_sol)
                .ok_or(MinerError::Overflow)?;
        }

        game.ml_sol = game.ml_sol.checked_add(ml_sol).ok_or(MinerError::Overflow)?;
        game.ml_miner = game
            .ml_miner
            .checked_add(ml_miner)
            .ok_or(MinerError::Overflow)?;
        game.total_burned = game
            .total_burned
            .checked_add(burn_miner)
            .ok_or(MinerError::Overflow)?;
        game.total_fee_sol = game
            .total_fee_sol
            .checked_add(fee_sol)
            .ok_or(MinerError::Overflow)?;
        game.total_rounds_played = game
            .total_rounds_played
            .checked_add(1)
            .ok_or(MinerError::Overflow)?;

        closing.settled = GAME_ROUND_SETTLED;
        closing.collapsed = collapsed_mask;
        closing.payout_sol = pot_sol - fee_sol - ml_sol;
        closing.payout_miner = pot_miner - burn_miner - ml_miner;
        closing.survivor_weight = survivor_weight as u64;
    } else {
        closing.settled = GAME_ROUND_VOID;
    }

    // Players' Motherlode strike: every settle of a played round is one
    // roll; on a hit both pools split evenly across the candidate slots
    // (the division remainder goes to the first slot as dust).
    if closing.entries > 0 && (game.ml_sol > 0 || game.ml_miner > 0) {
        let strike_hash = keccak::hashv(&[
            entropy.as_slice(),
            &closing.index.to_le_bytes(),
            &closing.entries.to_le_bytes(),
            b"game_motherlode",
        ]);
        let roll = u64::from_le_bytes(strike_hash.as_ref()[..8].try_into().unwrap());
        if roll % GAME_MOTHERLODE_ODDS == 0 {
            let winners = GAME_MOTHERLODE_WINNERS as u64;
            let share_sol = game.ml_sol / winners;
            let share_miner = game.ml_miner / winners;
            let rem_sol = game.ml_sol - share_sol * winners;
            let rem_miner = game.ml_miner - share_miner * winners;
            for slot in 0..GAME_MOTHERLODE_WINNERS {
                let (sol, miner) = if slot == 0 {
                    (share_sol + rem_sol, share_miner + rem_miner)
                } else {
                    (share_sol, share_miner)
                };
                if sol == 0 && miner == 0 {
                    continue;
                }
                let win_info = &win_infos[slot];
                let candidate_bytes = closing.candidates[slot];
                let candidate = Pubkey::new_from_array(candidate_bytes);
                let (win_key, win_bump) = pda::game_win_pda(&candidate);
                expect_key(win_info, &win_key)?;
                expect_writable(win_info)?;
                // A wallet holding several slots appears several times:
                // the first pass creates its GameWin, later ones fall into
                // the accumulate branch (the infos alias the same data).
                if win_info.owner.ne(&miner_api::id()) || win_info.data_is_empty() {
                    create_pda(
                        win_info,
                        payer_info,
                        system_program,
                        GameWin::SIZE,
                        &[GAME_WIN_SEED, candidate_bytes.as_ref(), &[win_bump]],
                    )?;
                    write_state(
                        win_info,
                        &GameWin {
                            discriminator: GAME_WIN_DISCRIMINATOR,
                            authority: candidate_bytes,
                            sol,
                            miner,
                            since_ts: now,
                            bump: win_bump as u64,
                        },
                    )?;
                } else {
                    expect_program_account(win_info, GAME_WIN_DISCRIMINATOR)?;
                    let mut win = read_state::<GameWin>(win_info)?;
                    win.sol = win.sol.checked_add(sol).ok_or(MinerError::Overflow)?;
                    win.miner = win.miner.checked_add(miner).ok_or(MinerError::Overflow)?;
                    write_state(win_info, &win)?;
                }
            }
            game.ml_last_winners = closing.candidates;
            game.ml_last_sol = share_sol;
            game.ml_last_miner = share_miner;
            game.ml_last_ts = now;
            game.ml_sol = 0;
            game.ml_miner = 0;
        }
    }
    write_state(closing_info, &closing)?;

    // Open the next round after the intermission. Keep the cadence on
    // small slips; reset on a large backlog.
    let new_index = game
        .current_round
        .checked_add(1)
        .ok_or(MinerError::Overflow)?;
    let scheduled = game.round_start_ts.saturating_add(round_seconds);
    let base = if now < scheduled.saturating_add(round_seconds) {
        scheduled
    } else {
        now
    };
    let next_start = base.saturating_add(GAME_INTERMISSION_SECONDS);

    let (new_round_key, new_round_bump) = pda::game_round_pda(new_index);
    expect_key(new_round_info, &new_round_key)?;
    create_pda(
        new_round_info,
        payer_info,
        system_program,
        GameRound::SIZE,
        &[GAME_ROUND_SEED, &new_index.to_le_bytes(), &[new_round_bump]],
    )?;
    write_state(
        new_round_info,
        &GameRound {
            discriminator: GAME_ROUND_DISCRIMINATOR,
            index: new_index,
            start_ts: next_start,
            sol: [0; GAME_TUNNELS],
            miner: [0; GAME_TUNNELS],
            weight: [0; GAME_TUNNELS],
            entries: 0,
            candidates: [[0u8; 32]; GAME_MOTHERLODE_WINNERS],
            settled: GAME_ROUND_OPEN,
            collapsed: 0,
            payout_sol: 0,
            payout_miner: 0,
            survivor_weight: 0,
        },
    )?;

    game.current_round = new_index;
    game.round_start_ts = next_start;
    write_state(game_info, &game)?;

    Ok(())
}

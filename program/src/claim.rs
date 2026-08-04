use miner_api::{consts::*, error::MinerError, pda, state::*};
use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::loaders::*;

/// Reward claim: mints pending_rewards to the authority's token account.
/// Must be signed by the authority (a session key may not claim).
///
/// For a miner enrolled in the referral program (flag in Miner.bump) the
/// instruction distributes the accrued commission pool across the referral
/// chain (levels 5/3/1 of REFERRAL_LEVEL_BPS). Trailing accounts:
///   referral(miner), miner(L1), referral(L1),
///   [miner(L2), referral(L2), [miner(L3)]]
/// Each level's own Referral account is ALWAYS required (it may be a
/// nonexistent account): its emptiness proves on-chain that the chain ends
/// there, so a claimer cannot hide upper levels.
/// The carve is flat for everyone: shares of levels that do not exist are
/// BURNED (never minted), so every enrolled miner gives up exactly the
/// ladder total no matter how deep in the tree they sit. No extra mint
/// happens: the pool was already carved from the referee's rewards at
/// settlement.
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    // 8 accounts (legacy, not enrolled) or 11/13/14 (chain of 1/2/3 levels).
    let (fixed_accounts, chain_accounts): (_, &[AccountInfo]) = match accounts.len() {
        8 => (accounts, &[]),
        11 | 13 | 14 => (&accounts[..8], &accounts[8..]),
        _ => return Err(ProgramError::NotEnoughAccountKeys),
    };
    let [authority_info, miner_info, config_info, prev_round_info, mint_info, treasury_info, token_account_info, token_program_info] =
        fixed_accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(authority_info)?;
    expect_program_account(config_info, CONFIG_DISCRIMINATOR)?;
    let config = read_state::<Config>(config_info)?;

    expect_writable(miner_info)?;
    expect_program_account(miner_info, MINER_DISCRIMINATOR)?;
    let mut miner = read_state::<Miner>(miner_info)?;
    if miner.authority != authority_info.key.to_bytes() {
        return Err(MinerError::Unauthorized.into());
    }

    let referral_info = chain_accounts.first();
    let mut referral = load_referral(&miner, referral_info)?;

    // Settle any outstanding round so the claim collects everything.
    settle_previous_round(
        &mut miner,
        referral.as_mut(),
        config.current_round,
        prev_round_info,
    )?;

    // Distribute the commission pool across the chain. Whatever is not
    // delivered to someone else (missing levels, rounding dust, cycles
    // pointing back at the claimer) is burned: it was carved out of the
    // claimer's reward at settlement and is simply never minted.
    if let Some(r) = referral.as_mut() {
        let chain = &chain_accounts[1..];
        let pool = r.pending_commission;
        let mut distributed = 0u64;
        let mut wallet = r.referrer;
        let mut idx = 0usize;
        for level in 0..REFERRAL_LEVEL_BPS.len() {
            // The level's recipient: a registered Miner owned by `wallet`.
            let recipient_info = chain.get(idx).ok_or(MinerError::InvalidAccount)?;
            idx += 1;
            expect_program_account(recipient_info, MINER_DISCRIMINATOR)?;
            let mut recipient = read_state::<Miner>(recipient_info)?;
            if recipient.authority != wallet {
                return Err(MinerError::InvalidAccount.into());
            }
            let share = ((pool as u128) * (REFERRAL_LEVEL_BPS[level] as u128)
                / (REFERRAL_TOTAL_BPS as u128)) as u64;
            // A chain cycling back to the claimer burns that share: no
            // write through the duplicate account, the share stays in
            // `pool - distributed`. Self-referral loops pay for themselves.
            if wallet != miner.authority && share > 0 {
                expect_writable(recipient_info)?;
                recipient.pending_rewards = recipient
                    .pending_rewards
                    .checked_add(share)
                    .ok_or(MinerError::Overflow)?;
                write_state(recipient_info, &recipient)?;
                distributed = distributed
                    .checked_add(share)
                    .ok_or(MinerError::Overflow)?;
            }
            if level == REFERRAL_LEVEL_BPS.len() - 1 {
                break;
            }
            // Proof of continuation: the recipient's own Referral PDA. An
            // empty account proves the chain ends here; a live one forces
            // the next level to be present in the transaction.
            let slot = chain.get(idx).ok_or(MinerError::InvalidAccount)?;
            idx += 1;
            let wallet_key = Pubkey::new_from_array(wallet);
            expect_key(slot, &pda::referral_pda(&wallet_key).0)?;
            if slot.owner.ne(&miner_api::id()) || slot.data_is_empty() {
                break;
            }
            expect_program_account(slot, REFERRAL_DISCRIMINATOR)?;
            let next = read_state::<Referral>(slot)?;
            if next.authority != wallet {
                return Err(MinerError::InvalidAccount.into());
            }
            wallet = next.referrer;
        }
        // All provided chain accounts must be consumed: no padding, and the
        // chain may only end where an empty Referral slot proved it.
        if idx != chain.len() {
            return Err(MinerError::InvalidAccount.into());
        }
        // distributed <= pool (floor shares of a 5/3/1 split), no underflow.
        r.total_burned = r
            .total_burned
            .checked_add(pool - distributed)
            .ok_or(MinerError::Overflow)?;
        r.total_commission = r
            .total_commission
            .checked_add(distributed)
            .ok_or(MinerError::Overflow)?;
        r.pending_commission = 0;
    }
    if let (Some(r), Some(info)) = (referral.as_ref(), referral_info) {
        write_state(info, r)?;
    }

    let amount = miner.pending_rewards;
    if amount == 0 {
        return Err(MinerError::NothingToClaim.into());
    }

    // Validate the CPI accounts.
    expect_key(mint_info, &Pubkey::new_from_array(config.mint))?;
    expect_key(token_program_info, &SPL_TOKEN_PROGRAM_ID)?;
    let treasury_key = Pubkey::create_program_address(
        &[TREASURY_SEED, &[config.treasury_bump as u8]],
        &miner_api::id(),
    )
    .map_err(|_| ProgramError::from(MinerError::InvalidAccount))?;
    expect_key(treasury_info, &treasury_key)?;

    // The destination must exist and belong to the authority (right mint).
    if token_account_info.data_is_empty() {
        return Err(MinerError::InvalidTokenAccount.into());
    }
    read_token_balance(token_account_info, &config.mint, &miner.authority)?;

    // CPI: spl_token::mint_to signed by the treasury PDA.
    let mut ix_data = Vec::with_capacity(9);
    ix_data.push(SPL_TOKEN_MINT_TO_IX);
    ix_data.extend_from_slice(&amount.to_le_bytes());
    let mint_to_ix = Instruction {
        program_id: SPL_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*mint_info.key, false),
            AccountMeta::new(*token_account_info.key, false),
            AccountMeta::new_readonly(treasury_key, true),
        ],
        data: ix_data,
    };
    invoke_signed(
        &mint_to_ix,
        &[
            mint_info.clone(),
            token_account_info.clone(),
            treasury_info.clone(),
        ],
        &[&[TREASURY_SEED, &[config.treasury_bump as u8]]],
    )?;

    miner.pending_rewards = 0;
    miner.total_mined = miner
        .total_mined
        .checked_add(amount)
        .ok_or(MinerError::Overflow)?;
    write_state(miner_info, &miner)?;

    Ok(())
}

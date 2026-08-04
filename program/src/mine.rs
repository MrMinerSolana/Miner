use miner_api::{consts::*, error::MinerError, sdk, state::*};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, keccak,
    program_error::ProgramError,
};

use crate::loaders::*;

/// Hash submission, one per round.
///
/// 1. Signature check (authority or session key).
/// 2. Lazy settlement of the previous round (accrues pending_rewards).
/// 3. PoW check: keccak(challenge, authority, nonce) >= min_difficulty.
/// 4. Weight = base_weight + min(balance now, balance at previous submit).
///    Min-balance rule: freshly bought/transferred tokens only count from
///    the next round -> cycling tokens between wallets and flash balances
///    do not increase weight.
///    Referral bonus: for an enrolled miner (trailing referral account,
///    required by the flag in Miner.bump) the token slice of the weight is
///    boosted by REFERRAL_BONUS_BPS. The bonus is applied after the
///    min-balance rule, so the anti-cycling guarantee is unchanged.
/// 5. Add the weight to the round and roll the challenge.
pub fn process(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    // 7 accounts (legacy, miners not enrolled in the referral program) or 8
    // (trailing referral account).
    let (fixed_accounts, referral_info) = match accounts.len() {
        7 => (accounts, None),
        8 => (&accounts[..7], Some(&accounts[7])),
        _ => return Err(ProgramError::NotEnoughAccountKeys),
    };
    let [signer_info, miner_info, config_info, current_round_info, prev_round_info, token_account_info, slot_hashes_info] =
        fixed_accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let nonce = u64::from_le_bytes(
        data.try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );

    expect_signer(signer_info)?;
    expect_program_account(config_info, CONFIG_DISCRIMINATOR)?;
    let config = read_state::<Config>(config_info)?;

    expect_writable(miner_info)?;
    expect_program_account(miner_info, MINER_DISCRIMINATOR)?;
    let mut miner = read_state::<Miner>(miner_info)?;

    // Authorization: authority or session key.
    let signer_bytes = signer_info.key.to_bytes();
    if signer_bytes != miner.authority
        && (miner.session_key == [0u8; 32] || signer_bytes != miner.session_key)
    {
        return Err(MinerError::Unauthorized.into());
    }

    // Current round.
    expect_writable(current_round_info)?;
    expect_program_account(current_round_info, ROUND_DISCRIMINATOR)?;
    let mut round = read_state::<Round>(current_round_info)?;
    if round.index != config.current_round {
        return Err(MinerError::RoundMismatch.into());
    }

    // One submit per round. After a submit last_round_weight > 0 (the base
    // is > 0) and it is only zeroed when the next round settles, so
    // (last_round == current && weight > 0) unambiguously means a duplicate.
    if miner.last_round == config.current_round && miner.last_round_weight > 0 {
        return Err(MinerError::AlreadySubmitted.into());
    }

    let mut referral = load_referral(&miner, referral_info)?;

    // Settle the previous round.
    settle_previous_round(
        &mut miner,
        referral.as_mut(),
        config.current_round,
        prev_round_info,
    )?;

    // PoW verification.
    let hash = keccak::hashv(&[
        miner.challenge.as_slice(),
        miner.authority.as_slice(),
        &nonce.to_le_bytes(),
    ]);
    if sdk::leading_zero_bits(hash.as_ref()) < config.min_difficulty {
        return Err(MinerError::InvalidHash.into());
    }

    // Token balance (the account may not exist -> free tier, balance 0).
    let balance = read_token_balance(token_account_info, &config.mint, &miner.authority)?;

    // Weight: base + min(balance, previous balance), the token slice boosted
    // by the referral bonus for enrolled miners.
    let counted_balance = balance.min(miner.last_balance);
    let token_weight = match referral.as_mut() {
        Some(r) => {
            let boosted = ((counted_balance as u128)
                * ((BPS_DENOM + REFERRAL_BONUS_BPS) as u128)
                / (BPS_DENOM as u128)) as u64;
            r.last_token_weight = boosted;
            boosted
        }
        None => counted_balance,
    };
    let weight = config
        .base_weight
        .checked_add(token_weight)
        .ok_or(MinerError::Overflow)?;

    round.total_weight = round
        .total_weight
        .checked_add(weight)
        .ok_or(MinerError::Overflow)?;
    write_state(current_round_info, &round)?;

    // Update the miner + roll the challenge.
    let entropy = slot_hashes_entropy(slot_hashes_info)?;
    miner.challenge = next_challenge(&miner.challenge, &entropy, &miner.authority);
    miner.last_round = config.current_round;
    miner.last_round_weight = weight;
    miner.last_balance = balance;
    miner.total_hashes = miner
        .total_hashes
        .checked_add(1)
        .ok_or(MinerError::Overflow)?;
    write_state(miner_info, &miner)?;

    if let (Some(r), Some(info)) = (referral.as_ref(), referral_info) {
        write_state(info, r)?;
    }

    Ok(())
}

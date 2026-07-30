use miner_api::{error::MinerError, sdk, state::*};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, keccak,
    program_error::ProgramError,
};

use crate::loaders::*;

/// Hash submission — one per round.
///
/// 1. Signature check (authority or session key).
/// 2. Lazy settlement of the previous round (accrues pending_rewards).
/// 3. PoW check: keccak(challenge, authority, nonce) >= min_difficulty.
/// 4. Weight = base_weight + min(balance now, balance at previous submit).
///    Min-balance rule: freshly bought/transferred tokens only count from
///    the next round -> cycling tokens between wallets and flash balances
///    do not increase weight.
/// 5. Add the weight to the round and roll the challenge.
pub fn process(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [signer_info, miner_info, config_info, current_round_info, prev_round_info, token_account_info, slot_hashes_info] =
        accounts
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
    // is > 0) and it is only zeroed when the next round settles — so
    // (last_round == current && weight > 0) unambiguously means a duplicate.
    if miner.last_round == config.current_round && miner.last_round_weight > 0 {
        return Err(MinerError::AlreadySubmitted.into());
    }

    // Settle the previous round.
    settle_previous_round(&mut miner, config.current_round, prev_round_info)?;

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

    // Weight: base + min(balance, previous balance).
    let weight = config
        .base_weight
        .checked_add(balance.min(miner.last_balance))
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

    Ok(())
}

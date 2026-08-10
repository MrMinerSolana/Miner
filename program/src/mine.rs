use miner_api::{consts::*, error::MinerError, sdk, state::*};
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

/// Hash submission, one per round.
///
/// 1. Signature check (authority or session key).
/// 2. Lazy settlement of the previous round (accrues pending_rewards).
/// 3. PoW check: keccak(challenge, authority, nonce) >= min_difficulty.
/// 4. Weight = base_weight + min(balance now, balance at previous submit)
///    + locked tokens (times the tier multiplier while the lock is active).
///    Min-balance rule: freshly bought/transferred tokens only count from
///    the next round -> cycling tokens between wallets and flash balances
///    do not increase weight. Locked tokens are exempt: they sit in the
///    program vault and physically cannot cycle, so they count right away.
///    Referral bonus: for an enrolled miner (trailing referral account,
///    required by the flag in Miner.bump) the token slice of the weight
///    (wallet + locked) is boosted by REFERRAL_BONUS_BPS. The bonus is
///    applied after the min-balance rule, so the anti-cycling guarantee is
///    unchanged.
/// 5. Charge MINE_FEE_LAMPORTS from the signer to the fee wallet and buy
///    one Motherlode chance (reservoir sampling keeps the winner candidate
///    uniform over all hashes of the round).
/// 6. Add the weight to the round and roll the challenge.
pub fn process(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    // 10 fixed accounts, then up to two trailing program accounts told apart
    // by their discriminator: the Referral PDA (required once enrolled) and
    // the Lock PDA (optional, adds the locked tokens to the weight).
    if accounts.len() < 10 || accounts.len() > 12 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let (fixed_accounts, trailing) = accounts.split_at(10);
    let mut referral_info = None;
    let mut lock_info = None;
    for info in trailing {
        // A nonexistent trailing account is ignored, so clients may always
        // append the (possibly empty) referral / lock PDA slots.
        if info.owner.ne(&miner_api::id()) || info.data_is_empty() {
            continue;
        }
        match program_account_discriminator(info)? {
            REFERRAL_DISCRIMINATOR if referral_info.is_none() => referral_info = Some(info),
            LOCK_DISCRIMINATOR if lock_info.is_none() => lock_info = Some(info),
            _ => return Err(MinerError::InvalidAccount.into()),
        }
    }
    let [signer_info, miner_info, config_info, current_round_info, prev_round_info, token_account_info, slot_hashes_info, fee_wallet_info, motherlode_info, system_program_info] =
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

    // Locked tokens (multiplied while the lock is active).
    let lock = load_lock(&miner, lock_info)?;
    let now = Clock::get()?.unix_timestamp;
    let lock_weight = locked_weight(lock.as_ref(), now);

    // Weight: base + min(balance, previous balance) + locked tokens, the
    // whole token slice boosted by the referral bonus for enrolled miners.
    let counted_balance = balance
        .min(miner.last_balance)
        .checked_add(lock_weight)
        .ok_or(MinerError::Overflow)?;
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

    // Mining fee: MINE_FEE_LAMPORTS from the signer to the fee wallet
    // (system transfer; the signer already signs the transaction). Paid per
    // instruction, so batching many miners into one transaction saves
    // nothing on the fee.
    expect_key(fee_wallet_info, &FEE_WALLET)?;
    {
        // SystemInstruction::Transfer (bincode): tag u32 + lamports u64.
        let mut fee_data = Vec::with_capacity(12);
        fee_data.extend_from_slice(&2u32.to_le_bytes());
        fee_data.extend_from_slice(&MINE_FEE_LAMPORTS.to_le_bytes());
        let fee_ix = Instruction {
            program_id: SYSTEM_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(*signer_info.key, true),
                AccountMeta::new(*fee_wallet_info.key, false),
            ],
            data: fee_data,
        };
        invoke(
            &fee_ix,
            &[
                signer_info.clone(),
                fee_wallet_info.clone(),
                system_program_info.clone(),
            ],
        )?;
    }

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

    // Motherlode chance: one per mine instruction. Each of the
    // MOTHERLODE_WINNERS candidate slots runs its own reservoir sample with
    // a uniform 1/n chance per hash: hash k replaces the slot with
    // probability 1/k, independently per slot (the slot index salts the
    // sample hash). The entropy comes from the freshly rolled challenge
    // (slot hash + old challenge + authority), which the miner cannot
    // choose freely: any nonce grinding costs full PoW solves, each of
    // which is itself another paid submission.
    expect_writable(motherlode_info)?;
    expect_program_account(motherlode_info, MOTHERLODE_DISCRIMINATOR)?;
    let mut motherlode = read_state::<Motherlode>(motherlode_info)?;
    if motherlode.round_index != config.current_round {
        motherlode.round_index = config.current_round;
        motherlode.hashes = 0;
    }
    motherlode.hashes = motherlode
        .hashes
        .checked_add(1)
        .ok_or(MinerError::Overflow)?;
    for slot in 0..MOTHERLODE_WINNERS {
        let sample = keccak::hashv(&[
            miner.challenge.as_slice(),
            &motherlode.hashes.to_le_bytes(),
            &[slot as u8],
        ]);
        let roll = u64::from_le_bytes(sample.as_ref()[..8].try_into().unwrap());
        if roll % motherlode.hashes == 0 {
            motherlode.candidates[slot] = miner.authority;
        }
    }
    motherlode.total_fees = motherlode
        .total_fees
        .checked_add(MINE_FEE_LAMPORTS)
        .ok_or(MinerError::Overflow)?;
    write_state(motherlode_info, &motherlode)?;

    Ok(())
}

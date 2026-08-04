use miner_api::{consts::*, error::MinerError, pda, state::*};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
};

use crate::loaders::*;

/// Referral enrollment: creates the Referral PDA binding the miner to a
/// referrer and sets MINER_FLAG_REFERRAL on the Miner account, so from now
/// on mine/claim must carry the Referral account (otherwise the commission
/// bookkeeping could be skipped).
///
/// One-shot and immutable: a second call fails on the existing Referral
/// account. Callable at any time, so miners registered before the referral
/// program can enroll without migrating wallets. Must be signed by the
/// authority itself (a session key may not enroll).
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    let [authority_info, miner_info, referrer_miner_info, referral_info, system_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(authority_info)?;

    expect_writable(miner_info)?;
    expect_program_account(miner_info, MINER_DISCRIMINATOR)?;
    let mut miner = read_state::<Miner>(miner_info)?;
    if miner.authority != authority_info.key.to_bytes() {
        return Err(MinerError::Unauthorized.into());
    }

    // The referrer must be a registered miner (their Miner account is where
    // commissions are delivered) and not the caller.
    expect_program_account(referrer_miner_info, MINER_DISCRIMINATOR)?;
    let referrer = read_state::<Miner>(referrer_miner_info)?.authority;
    if referrer == miner.authority {
        return Err(MinerError::SelfReferral.into());
    }

    let (referral_key, referral_bump) = pda::referral_pda(authority_info.key);
    expect_key(referral_info, &referral_key)?;
    expect_writable(referral_info)?;
    create_pda(
        referral_info,
        authority_info,
        system_program,
        Referral::SIZE,
        &[REFERRAL_SEED, miner.authority.as_ref(), &[referral_bump]],
    )?;
    write_state(
        referral_info,
        &Referral {
            discriminator: REFERRAL_DISCRIMINATOR,
            authority: miner.authority,
            referrer,
            last_token_weight: 0,
            pending_commission: 0,
            total_commission: 0,
            total_burned: 0,
            bump: referral_bump as u64,
        },
    )?;

    miner.bump |= MINER_FLAG_REFERRAL;
    write_state(miner_info, &miner)?;

    Ok(())
}

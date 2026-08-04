use miner_api::{consts::*, error::MinerError, pda, state::*};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
};

use crate::loaders::*;

/// Claims a custom referral name (vanity code) for the caller: creates both
/// directions of the mapping (name -> owner and owner -> name) in one shot.
/// First come first served (the name PDA already existing means taken), one
/// name per miner (the owner PDA already existing means claimed), immutable.
///
/// The name is purely a client-side pointer for ?ref=<name> links; the
/// referral commissions themselves always flow by wallet address, so this
/// touches none of the mine/claim accounting.
pub fn process(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority_info, miner_info, refname_info, refname_owner_info, system_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(authority_info)?;

    // Only registered miners: a name must point at a wallet that can
    // actually receive commissions (SetReferrer checks the same account).
    expect_program_account(miner_info, MINER_DISCRIMINATOR)?;
    let miner = read_state::<Miner>(miner_info)?;
    if miner.authority != authority_info.key.to_bytes() {
        return Err(MinerError::Unauthorized.into());
    }

    // 3-16 chars, lowercase a-z 0-9 _ (clients normalize case before
    // sending; rejecting uppercase on-chain keeps one canonical PDA per
    // name, so "MrMiner" and "mrminer" can never be two different owners).
    if data.len() < REFNAME_MIN_LEN || data.len() > REFNAME_MAX_LEN {
        return Err(MinerError::InvalidName.into());
    }
    if !data
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
    {
        return Err(MinerError::InvalidName.into());
    }

    let (refname_key, refname_bump) = pda::refname_pda(data);
    let (owner_key, owner_bump) = pda::refname_owner_pda(authority_info.key);
    expect_key(refname_info, &refname_key)?;
    expect_key(refname_owner_info, &owner_key)?;
    expect_writable(refname_info)?;
    expect_writable(refname_owner_info)?;

    create_pda(
        refname_info,
        authority_info,
        system_program,
        RefName::SIZE,
        &[REFNAME_SEED, data, &[refname_bump]],
    )?;
    create_pda(
        refname_owner_info,
        authority_info,
        system_program,
        RefName::SIZE,
        &[REFNAME_OWNER_SEED, miner.authority.as_ref(), &[owner_bump]],
    )?;

    let mut name = [0u8; 32];
    name[..data.len()].copy_from_slice(data);
    write_state(
        refname_info,
        &RefName {
            discriminator: REFNAME_DISCRIMINATOR,
            owner: miner.authority,
            name,
            bump: refname_bump as u64,
        },
    )?;
    write_state(
        refname_owner_info,
        &RefName {
            discriminator: REFNAME_DISCRIMINATOR,
            owner: miner.authority,
            name,
            bump: owner_bump as u64,
        },
    )?;

    Ok(())
}

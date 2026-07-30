use miner_api::{consts::*, pda, state::*};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
};

use crate::loaders::*;

/// Miner registration: creates the Miner account (PDA from the authority)
/// with an initial challenge derived from slot entropy.
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    let [authority_info, miner_info, slot_hashes_info, system_program] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(authority_info)?;

    let (miner_key, miner_bump) = pda::miner_pda(authority_info.key);
    expect_key(miner_info, &miner_key)?;

    let entropy = slot_hashes_entropy(slot_hashes_info)?;
    let authority = authority_info.key.to_bytes();
    let challenge = next_challenge(&[0u8; 32], &entropy, &authority);

    create_pda(
        miner_info,
        authority_info,
        system_program,
        Miner::SIZE,
        &[MINER_SEED, authority.as_ref(), &[miner_bump]],
    )?;
    write_state(
        miner_info,
        &Miner {
            discriminator: MINER_DISCRIMINATOR,
            authority,
            session_key: [0u8; 32],
            challenge,
            last_round: 0,
            last_round_weight: 0,
            last_balance: 0,
            pending_rewards: 0,
            total_mined: 0,
            total_hashes: 0,
            bump: miner_bump as u64,
        },
    )?;

    Ok(())
}

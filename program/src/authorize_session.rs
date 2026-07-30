use miner_api::{error::MinerError, state::*};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
};

use crate::loaders::*;

/// Sets (or clears) the session key allowed to sign submits.
/// A session key can NOT claim — only mine.
pub fn process(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority_info, miner_info] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(authority_info)?;
    expect_writable(miner_info)?;
    expect_program_account(miner_info, MINER_DISCRIMINATOR)?;

    let session_key: [u8; 32] = data
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?;

    let mut miner = read_state::<Miner>(miner_info)?;
    if miner.authority != authority_info.key.to_bytes() {
        return Err(MinerError::Unauthorized.into());
    }
    miner.session_key = session_key;
    write_state(miner_info, &miner)
}

use miner_api::{error::MinerError, state::*};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
};

use crate::loaders::*;

/// Hands over the config admin role (one-off or rotation — e.g. to a
/// Squads multisig with a timelock). The new admin takes over
/// update_config and future set_admin calls; there are no other powers
/// (the mint authority stays with the treasury PDA and rewards are
/// settled by the program).
pub fn process(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [admin_info, config_info] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(admin_info)?;
    expect_writable(config_info)?;
    expect_program_account(config_info, CONFIG_DISCRIMINATOR)?;

    let new_admin: [u8; 32] = data
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    // The all-zero key stays disallowed — "burning" the admin should be a
    // deliberate handover to an address with no known key, not a typo.
    if new_admin == [0u8; 32] {
        return Err(ProgramError::InvalidInstructionData);
    }

    let mut config = read_state::<Config>(config_info)?;
    if config.admin != admin_info.key.to_bytes() {
        return Err(MinerError::Unauthorized.into());
    }
    config.admin = new_admin;
    write_state(config_info, &config)
}

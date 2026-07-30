use miner_api::{consts::*, error::MinerError, state::*};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
};

use crate::loaders::*;

/// Closes a round account older than ROUND_RETENTION — rent goes to the
/// caller (an incentive to clean up). Unsettled rewards from that round
/// lapse (settlement treats an empty account as a forfeit).
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    let [recipient_info, config_info, round_info] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(recipient_info)?;
    expect_program_account(config_info, CONFIG_DISCRIMINATOR)?;
    let config = read_state::<Config>(config_info)?;

    expect_writable(round_info)?;
    expect_program_account(round_info, ROUND_DISCRIMINATOR)?;
    let round = read_state::<Round>(round_info)?;

    if round.index.saturating_add(ROUND_RETENTION) > config.current_round {
        return Err(MinerError::RoundNotExpired.into());
    }

    // Zero the data (so the account cannot be "revived" within the same tx)
    // and move the lamports out — a zero-balance account vanishes after
    // the transaction.
    {
        let mut data = round_info.try_borrow_mut_data()?;
        data.fill(0);
    }
    let lamports = round_info.lamports();
    **round_info.try_borrow_mut_lamports()? = 0;
    **recipient_info.try_borrow_mut_lamports()? = recipient_info
        .lamports()
        .checked_add(lamports)
        .ok_or(MinerError::Overflow)?;

    Ok(())
}

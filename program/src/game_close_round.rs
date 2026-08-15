use miner_api::{consts::*, error::MinerError, state::*};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, sysvar::Sysvar,
};

use crate::loaders::*;

/// Closes a settled game round account once the claim window
/// (GAME_ROUND_RETENTION_SECONDS from the round's start) has passed; the
/// rent moves to the recipient. Only the FEE_WALLET or the config admin
/// (the multisig) may sign, so nobody outside the operator can
/// garbage-collect rounds or pocket the rent.
/// Entries left unclaimed on a closed round lapse: GameClaim detects the
/// missing round account and refunds just the entry rent. Players'
/// Motherlode wins are unaffected (GameWin claims never read rounds).
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    let [signer_info, recipient_info, config_info, game_info, round_info] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(signer_info)?;
    expect_program_account(config_info, CONFIG_DISCRIMINATOR)?;
    let config = read_state::<Config>(config_info)?;
    if signer_info.key.to_bytes() != FEE_WALLET.to_bytes()
        && signer_info.key.to_bytes() != config.admin
    {
        return Err(MinerError::Unauthorized.into());
    }
    expect_writable(recipient_info)?;

    expect_program_account(game_info, GAME_DISCRIMINATOR)?;
    let game = read_state::<Game>(game_info)?;

    expect_writable(round_info)?;
    expect_program_account(round_info, GAME_ROUND_DISCRIMINATOR)?;
    let round = read_state::<GameRound>(round_info)?;

    // Only settled rounds (the current one is still collecting entries).
    if round.index >= game.current_round || round.settled == GAME_ROUND_OPEN {
        return Err(MinerError::GameNotSettled.into());
    }
    let now = Clock::get()?.unix_timestamp;
    if now < round.start_ts.saturating_add(GAME_ROUND_RETENTION_SECONDS) {
        return Err(MinerError::RoundNotExpired.into());
    }

    // Zero the data (so the account cannot be "revived" within the same
    // tx) and move the lamports out; a zero-balance account vanishes
    // after the transaction.
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

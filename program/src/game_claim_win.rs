use miner_api::{consts::*, error::MinerError, pda, state::*};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::loaders::*;

/// Claim a players' Motherlode win: the SOL and $MINER amounts move from
/// the game vaults to the winner. Closes the GameWin PDA (rent to the
/// winner). Must be signed by the authority itself.
pub fn process(accounts: &[AccountInfo]) -> ProgramResult {
    let [authority_info, game_info, win_info, vault_info, user_token_info, game_token_info, token_program_info] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    expect_signer(authority_info)?;
    expect_program_account(game_info, GAME_DISCRIMINATOR)?;
    let game = read_state::<Game>(game_info)?;

    expect_writable(win_info)?;
    expect_program_account(win_info, GAME_WIN_DISCRIMINATOR)?;
    let win = read_state::<GameWin>(win_info)?;
    if win.authority != authority_info.key.to_bytes() {
        return Err(MinerError::Unauthorized.into());
    }
    if win.sol == 0 && win.miner == 0 {
        return Err(MinerError::NothingToClaim.into());
    }

    let (vault_key, _) = pda::game_vault_pda();
    expect_key(vault_info, &vault_key)?;
    expect_writable(vault_info)?;
    expect_key(token_program_info, &SPL_TOKEN_PROGRAM_ID)?;
    if win.miner > 0 {
        let game_key = pda::game_pda().0;
        let mint = {
            let data = game_token_info.try_borrow_data()?;
            if game_token_info.owner.ne(&SPL_TOKEN_PROGRAM_ID) || data.len() < 72 {
                return Err(MinerError::InvalidTokenAccount.into());
            }
            let mint: [u8; 32] = data[0..32].try_into().unwrap();
            mint
        };
        expect_key(
            game_token_info,
            &pda::ata(&game_key, &Pubkey::new_from_array(mint)),
        )?;
        expect_writable(game_token_info)?;
        expect_writable(user_token_info)?;
        if user_token_info.data_is_empty() {
            return Err(MinerError::InvalidTokenAccount.into());
        }
        read_token_balance(user_token_info, &mint, &win.authority)?;
    }

    game_payout(
        game_info,
        game.bump as u8,
        vault_info,
        game_token_info,
        user_token_info,
        authority_info,
        win.sol,
        win.miner,
    )?;

    // Close the GameWin PDA (rent to the winner).
    close_program_account(win_info, authority_info)
}

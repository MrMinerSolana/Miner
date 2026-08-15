mod authorize_session;
mod claim;
mod claim_motherlode;
mod close_round;
mod crank;
mod game_claim;
mod game_claim_win;
mod game_enter;
mod game_settle;
mod init_game;
mod init_motherlode;
mod initialize;
mod loaders;
mod lock;
mod mine;
mod register;
mod set_admin;
mod set_referrer;
mod set_refname;
mod unlock;
mod update_config;

use miner_api::instruction::MinerInstruction;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    if program_id.ne(&miner_api::id()) {
        return Err(ProgramError::IncorrectProgramId);
    }
    let (tag, rest) = data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;
    let ix =
        MinerInstruction::from_u8(*tag).ok_or(ProgramError::InvalidInstructionData)?;
    match ix {
        MinerInstruction::Initialize => initialize::process(accounts),
        MinerInstruction::Register => register::process(accounts),
        MinerInstruction::AuthorizeSession => authorize_session::process(accounts, rest),
        MinerInstruction::Mine => mine::process(accounts, rest),
        MinerInstruction::Claim => claim::process(accounts),
        MinerInstruction::Crank => crank::process(accounts),
        MinerInstruction::CloseRound => close_round::process(accounts),
        MinerInstruction::UpdateConfig => update_config::process(accounts, rest),
        MinerInstruction::SetAdmin => set_admin::process(accounts, rest),
        MinerInstruction::SetReferrer => set_referrer::process(accounts),
        MinerInstruction::SetRefName => set_refname::process(accounts, rest),
        MinerInstruction::Lock => lock::process(accounts, rest),
        MinerInstruction::Unlock => unlock::process(accounts),
        MinerInstruction::InitMotherlode => init_motherlode::process(accounts),
        MinerInstruction::ClaimMotherlode => claim_motherlode::process(accounts),
        MinerInstruction::InitGame => init_game::process(accounts, rest),
        MinerInstruction::GameEnter => game_enter::process(accounts, rest),
        MinerInstruction::GameSettle => game_settle::process(accounts),
        MinerInstruction::GameClaim => game_claim::process(accounts),
        MinerInstruction::GameClaimWin => game_claim_win::process(accounts),
    }
}

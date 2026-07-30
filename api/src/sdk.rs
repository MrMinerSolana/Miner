//! Instruction builders for clients and tests.

use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::{consts::*, instruction::MinerInstruction, pda};

pub fn initialize(admin: Pubkey, mint: Pubkey) -> Instruction {
    let (config, _) = pda::config_pda();
    let (round0, _) = pda::round_pda(0);
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(admin, true),
            AccountMeta::new(config, false),
            AccountMeta::new_readonly(mint, false),
            AccountMeta::new(round0, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data: vec![MinerInstruction::Initialize as u8],
    }
}

pub fn register(authority: Pubkey) -> Instruction {
    let (miner, _) = pda::miner_pda(&authority);
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(authority, true),
            AccountMeta::new(miner, false),
            AccountMeta::new_readonly(SLOT_HASHES_SYSVAR_ID, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data: vec![MinerInstruction::Register as u8],
    }
}

pub fn authorize_session(authority: Pubkey, session_key: Pubkey) -> Instruction {
    let (miner, _) = pda::miner_pda(&authority);
    let mut data = vec![MinerInstruction::AuthorizeSession as u8];
    data.extend_from_slice(session_key.as_ref());
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(miner, false),
        ],
        data,
    }
}

pub fn mine(
    signer: Pubkey,
    authority: Pubkey,
    mint: Pubkey,
    current_round_index: u64,
    prev_round_index: u64,
    nonce: u64,
) -> Instruction {
    let (miner, _) = pda::miner_pda(&authority);
    let (config, _) = pda::config_pda();
    let (current_round, _) = pda::round_pda(current_round_index);
    let (prev_round, _) = pda::round_pda(prev_round_index);
    let token_account = pda::ata(&authority, &mint);
    let mut data = vec![MinerInstruction::Mine as u8];
    data.extend_from_slice(&nonce.to_le_bytes());
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new_readonly(signer, true),
            AccountMeta::new(miner, false),
            AccountMeta::new_readonly(config, false),
            AccountMeta::new(current_round, false),
            AccountMeta::new_readonly(prev_round, false),
            AccountMeta::new_readonly(token_account, false),
            AccountMeta::new_readonly(SLOT_HASHES_SYSVAR_ID, false),
        ],
        data,
    }
}

pub fn claim(authority: Pubkey, mint: Pubkey, prev_round_index: u64) -> Instruction {
    let (miner, _) = pda::miner_pda(&authority);
    let (config, _) = pda::config_pda();
    let (prev_round, _) = pda::round_pda(prev_round_index);
    let (treasury, _) = pda::treasury_pda();
    let token_account = pda::ata(&authority, &mint);
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(miner, false),
            AccountMeta::new_readonly(config, false),
            AccountMeta::new_readonly(prev_round, false),
            AccountMeta::new(mint, false),
            AccountMeta::new_readonly(treasury, false),
            AccountMeta::new(token_account, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
        ],
        data: vec![MinerInstruction::Claim as u8],
    }
}

pub fn crank(payer: Pubkey, new_round_index: u64) -> Instruction {
    let (config, _) = pda::config_pda();
    let (new_round, _) = pda::round_pda(new_round_index);
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(config, false),
            AccountMeta::new(new_round, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data: vec![MinerInstruction::Crank as u8],
    }
}

pub fn close_round(recipient: Pubkey, round_index: u64) -> Instruction {
    let (config, _) = pda::config_pda();
    let (round, _) = pda::round_pda(round_index);
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(recipient, true),
            AccountMeta::new_readonly(config, false),
            AccountMeta::new(round, false),
        ],
        data: vec![MinerInstruction::CloseRound as u8],
    }
}

pub fn update_config(
    admin: Pubkey,
    min_difficulty: u64,
    base_weight: u64,
    round_seconds: u64,
) -> Instruction {
    let (config, _) = pda::config_pda();
    let mut data = vec![MinerInstruction::UpdateConfig as u8];
    data.extend_from_slice(&min_difficulty.to_le_bytes());
    data.extend_from_slice(&base_weight.to_le_bytes());
    data.extend_from_slice(&round_seconds.to_le_bytes());
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new_readonly(admin, true),
            AccountMeta::new(config, false),
        ],
        data,
    }
}

pub fn set_admin(admin: Pubkey, new_admin: Pubkey) -> Instruction {
    let (config, _) = pda::config_pda();
    let mut data = vec![MinerInstruction::SetAdmin as u8];
    data.extend_from_slice(new_admin.as_ref());
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new_readonly(admin, true),
            AccountMeta::new(config, false),
        ],
        data,
    }
}

/// Client-side hash verification (identical logic to the on-chain program).
pub fn hash_meets_difficulty(
    challenge: &[u8; 32],
    authority: &Pubkey,
    nonce: u64,
    min_difficulty: u64,
) -> bool {
    let hash = solana_program::keccak::hashv(&[
        challenge.as_slice(),
        authority.as_ref(),
        &nonce.to_le_bytes(),
    ]);
    leading_zero_bits(hash.as_ref()) >= min_difficulty
}

pub fn leading_zero_bits(bytes: &[u8]) -> u64 {
    let mut bits: u64 = 0;
    for b in bytes {
        if *b == 0 {
            bits += 8;
        } else {
            bits += b.leading_zeros() as u64;
            break;
        }
    }
    bits
}

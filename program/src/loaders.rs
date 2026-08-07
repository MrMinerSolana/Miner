//! Account validation and shared helpers.
//!
//! Program accounts (Config/Round/Miner) are validated via
//! owner == program + discriminator + identity fields (index / authority),
//! without re-deriving PDAs in the hot path, since only this program could have
//! created an account with that discriminator, and it only ever creates
//! them at the canonical PDA.

use bytemuck::{Pod, Zeroable};
use miner_api::{consts::*, error::MinerError, state::*};
use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    keccak,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    sysvar::Sysvar,
};

pub fn expect_signer(info: &AccountInfo) -> ProgramResult {
    if !info.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    Ok(())
}

pub fn expect_key(info: &AccountInfo, key: &Pubkey) -> ProgramResult {
    if info.key.ne(key) {
        return Err(MinerError::InvalidAccount.into());
    }
    Ok(())
}

pub fn expect_writable(info: &AccountInfo) -> ProgramResult {
    if !info.is_writable {
        return Err(MinerError::InvalidAccount.into());
    }
    Ok(())
}

/// Discriminator of an initialized account owned by this program (used to
/// tell trailing Referral/Lock accounts apart in Mine).
pub fn program_account_discriminator(info: &AccountInfo) -> Result<u64, ProgramError> {
    if info.owner.ne(&miner_api::id()) {
        return Err(MinerError::InvalidAccount.into());
    }
    let data = info.try_borrow_data()?;
    if data.len() < 8 {
        return Err(MinerError::InvalidDiscriminator.into());
    }
    Ok(u64::from_le_bytes(data[..8].try_into().unwrap()))
}

pub fn expect_program_account(info: &AccountInfo, discriminator: u64) -> ProgramResult {
    if info.owner.ne(&miner_api::id()) {
        return Err(MinerError::InvalidAccount.into());
    }
    let data = info.try_borrow_data()?;
    if data.len() < 8 {
        return Err(MinerError::InvalidDiscriminator.into());
    }
    let disc = u64::from_le_bytes(data[..8].try_into().unwrap());
    if disc != discriminator {
        return Err(MinerError::InvalidDiscriminator.into());
    }
    Ok(())
}

pub fn read_state<T: Pod + Zeroable + Copy>(info: &AccountInfo) -> Result<T, ProgramError> {
    let data = info.try_borrow_data()?;
    bytemuck::try_from_bytes::<T>(&data)
        .copied()
        .map_err(|_| MinerError::InvalidAccount.into())
}

pub fn write_state<T: Pod>(info: &AccountInfo, state: &T) -> ProgramResult {
    let mut data = info.try_borrow_mut_data()?;
    let dst = bytemuck::try_from_bytes_mut::<T>(&mut data)
        .map_err(|_| ProgramError::from(MinerError::InvalidAccount))?;
    *dst = *state;
    Ok(())
}

/// Creates a program-owned PDA account.
///
/// Griefing-resistant: System CreateAccount refuses an account that already
/// holds lamports, so anyone could block PDA creation by pre-funding the
/// (predictable) address with a plain transfer. Instead we only guard against
/// accounts with data (a truly initialized account), top up the rent-exempt
/// minimum if needed, then allocate + assign with the PDA seeds. Both work
/// on a system-owned account regardless of its lamport balance.
pub fn create_pda<'info>(
    target: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    space: usize,
    seeds: &[&[u8]],
) -> ProgramResult {
    if !target.data_is_empty() {
        return Err(ProgramError::AccountAlreadyInitialized);
    }
    let rent = Rent::get()?.minimum_balance(space);
    let balance = target.lamports();
    if balance < rent {
        // SystemInstruction::Transfer (bincode): tag u32 + lamports u64.
        let mut data = Vec::with_capacity(12);
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&(rent - balance).to_le_bytes());
        let ix = Instruction {
            program_id: SYSTEM_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(*payer.key, true),
                AccountMeta::new(*target.key, false),
            ],
            data,
        };
        invoke(&ix, &[payer.clone(), target.clone(), system_program.clone()])?;
    }

    // SystemInstruction::Allocate (bincode): tag u32 + space u64.
    let mut data = Vec::with_capacity(12);
    data.extend_from_slice(&8u32.to_le_bytes());
    data.extend_from_slice(&(space as u64).to_le_bytes());
    let ix = Instruction {
        program_id: SYSTEM_PROGRAM_ID,
        accounts: vec![AccountMeta::new(*target.key, true)],
        data,
    };
    invoke_signed(&ix, &[target.clone(), system_program.clone()], &[seeds])?;

    // SystemInstruction::Assign (bincode): tag u32 + owner pubkey.
    let mut data = Vec::with_capacity(36);
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(miner_api::id().as_ref());
    let ix = Instruction {
        program_id: SYSTEM_PROGRAM_ID,
        accounts: vec![AccountMeta::new(*target.key, true)],
        data,
    };
    invoke_signed(&ix, &[target.clone(), system_program.clone()], &[seeds])
}

/// Entropy from the SlotHashes sysvar (most recent slot hash).
pub fn slot_hashes_entropy(info: &AccountInfo) -> Result<[u8; 40], ProgramError> {
    expect_key(info, &SLOT_HASHES_SYSVAR_ID)?;
    let data = info.try_borrow_data()?;
    if data.len() < 48 {
        return Err(MinerError::InvalidAccount.into());
    }
    // 8 bytes of vector length + first entry (slot u64 + 32-byte hash)
    Ok(data[8..48].try_into().unwrap())
}

/// Token balance of the authority's SPL account (mint from config).
/// The account may not exist (free tier) -> balance 0.
pub fn read_token_balance(
    info: &AccountInfo,
    mint: &[u8; 32],
    authority: &[u8; 32],
) -> Result<u64, ProgramError> {
    if info.data_is_empty() {
        return Ok(0);
    }
    if info.owner.ne(&SPL_TOKEN_PROGRAM_ID) {
        return Err(MinerError::InvalidTokenAccount.into());
    }
    let data = info.try_borrow_data()?;
    if data.len() < 72 {
        return Err(MinerError::InvalidTokenAccount.into());
    }
    if &data[0..32] != mint || &data[32..64] != authority {
        return Err(MinerError::InvalidTokenAccount.into());
    }
    Ok(u64::from_le_bytes(data[64..72].try_into().unwrap()))
}

/// Loads the miner's Referral account when the miner is enrolled
/// (MINER_FLAG_REFERRAL in Miner.bump). Enrolled miners MUST pass the
/// account, otherwise settlement could skip the commission bookkeeping.
/// For non-enrolled miners a trailing account, if any, is ignored.
pub fn load_referral(
    miner: &Miner,
    referral_info: Option<&AccountInfo>,
) -> Result<Option<Referral>, ProgramError> {
    if miner.bump & MINER_FLAG_REFERRAL == 0 {
        return Ok(None);
    }
    let info = referral_info.ok_or(MinerError::ReferralAccountRequired)?;
    expect_writable(info)?;
    expect_program_account(info, REFERRAL_DISCRIMINATOR)?;
    let referral = read_state::<Referral>(info)?;
    if referral.authority != miner.authority {
        return Err(MinerError::InvalidAccount.into());
    }
    Ok(Some(referral))
}

/// Loads the miner's Lock account when the trailing account was given.
/// Purely optional: without it the locked tokens simply do not count that
/// submit (the miner can only hurt themselves by omitting it).
pub fn load_lock(
    miner: &Miner,
    lock_info: Option<&AccountInfo>,
) -> Result<Option<Lock>, ProgramError> {
    let Some(info) = lock_info else {
        return Ok(None);
    };
    expect_program_account(info, LOCK_DISCRIMINATOR)?;
    let lock = read_state::<Lock>(info)?;
    if lock.authority != miner.authority {
        return Err(MinerError::InvalidAccount.into());
    }
    Ok(Some(lock))
}

/// Weight contributed by a lock at time `now`: the amount multiplied by the
/// tier while active, the plain amount once expired (still in the vault, so
/// it cannot cycle either way).
pub fn locked_weight(lock: Option<&Lock>, now: i64) -> u64 {
    match lock {
        Some(l) if now < l.unlock_ts => {
            ((l.amount as u128) * (l.multiplier_bps as u128) / (BPS_DENOM as u128)) as u64
        }
        Some(l) => l.amount,
        None => 0,
    }
}

/// Lazy settlement of the miner's previous round.
///
/// If the miner has unsettled weight from a round < current_round:
/// - matching round account given -> pending += budget * weight / total_weight
/// - round already closed (empty account) and retention passed -> reward lapses
/// - otherwise -> SettlementRequired error
///
/// For an enrolled miner the reward slice earned by the (boosted) token
/// weight of that submit is charged the full ladder total
/// (REFERRAL_TOTAL_BPS), accrued in Referral.pending_commission and
/// distributed across the referral chain at the referee's next claim
/// (shares of missing levels burn there). The base-weight slice is never
/// charged.
pub fn settle_previous_round(
    miner: &mut Miner,
    mut referral: Option<&mut Referral>,
    current_round: u64,
    prev_round_info: &AccountInfo,
) -> ProgramResult {
    if miner.last_round_weight == 0 || miner.last_round >= current_round {
        return Ok(());
    }
    if prev_round_info.data_is_empty() {
        // Round account closed after retention: the reward lapses.
        if miner.last_round.saturating_add(ROUND_RETENTION) <= current_round {
            miner.last_round_weight = 0;
            if let Some(r) = referral {
                r.last_token_weight = 0;
            }
            return Ok(());
        }
        return Err(MinerError::SettlementRequired.into());
    }
    expect_program_account(prev_round_info, ROUND_DISCRIMINATOR)?;
    let round = read_state::<Round>(prev_round_info)?;
    if round.index != miner.last_round {
        return Err(MinerError::RoundMismatch.into());
    }
    if round.total_weight > 0 {
        let reward = (round.budget as u128)
            .checked_mul(miner.last_round_weight as u128)
            .ok_or(MinerError::Overflow)?
            / (round.total_weight as u128);
        let mut commission = 0u64;
        if let Some(r) = referral.as_deref_mut() {
            // last_token_weight was recorded by the same submit that set
            // last_round_weight (both are zeroed together below); a submit
            // made before enrollment simply has it at 0 -> no commission.
            let token_weight = r.last_token_weight.min(miner.last_round_weight);
            let token_reward = reward
                .checked_mul(token_weight as u128)
                .ok_or(MinerError::Overflow)?
                / (miner.last_round_weight as u128);
            commission = (token_reward * (REFERRAL_TOTAL_BPS as u128)
                / (BPS_DENOM as u128)) as u64;
            r.pending_commission = r
                .pending_commission
                .checked_add(commission)
                .ok_or(MinerError::Overflow)?;
        }
        // commission <= 9% of the reward, so the subtraction cannot underflow.
        miner.pending_rewards = miner
            .pending_rewards
            .checked_add(reward as u64 - commission)
            .ok_or(MinerError::Overflow)?;
    }
    miner.last_round_weight = 0;
    if let Some(r) = referral {
        r.last_token_weight = 0;
    }
    Ok(())
}

/// Next challenge: keccak(old challenge, slot entropy, authority).
pub fn next_challenge(
    old: &[u8; 32],
    entropy: &[u8; 40],
    authority: &[u8; 32],
) -> [u8; 32] {
    keccak::hashv(&[old.as_slice(), entropy.as_slice(), authority.as_slice()]).to_bytes()
}

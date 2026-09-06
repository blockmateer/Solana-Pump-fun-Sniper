use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_program,
};
use spl_associated_token_account::instruction::create_associated_token_account_idempotent;
use spl_token::{instruction::close_account, ui_amount_to_amount};
use std::{str::FromStr, sync::Arc};

use crate::{
    common::{logger::Logger, utils::SwapConfig},
    core::{token, tx},
    engine::swap::{SwapDirection, SwapInType},
};

pub const TEN_THOUSAND: u64 = 10_000;
pub const PUMP_GLOBAL: &str = "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf";
pub const PUMP_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
pub const PUMP_EVENT_AUTHORITY: &str = "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1";
pub const PUMP_FEE_PROGRAM: &str = "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ";
pub const PUMP_GLOBAL_VOLUME_ACCUMULATOR: &str = "Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y";
pub const PUMP_FEE_CONFIG: &str = "8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt";
/// Trailing shared fee recipients required after bonding-curve-v2 (pick any).
pub const PUMP_BREAKING_FEE_RECIPIENT: &str = "5YxQFdt3Tr9zJLvkFccqXVUwhdTWJQc1fFg2YPbxvxeD";

/// Anchor discriminator for `buy_exact_sol_in`.
pub const PUMP_BUY_EXACT_SOL_IN_DISC: [u8; 8] = [56, 252, 116, 8, 158, 223, 205, 95];
/// Anchor discriminator for `sell`.
pub const PUMP_SELL_DISC: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];
pub const PUMP_FEE_BPS: u64 = 125; // ~1.25% protocol+creator estimate for quotes

pub struct Pump {
    pub rpc_nonblocking_client: Arc<solana_client::nonblocking::rpc_client::RpcClient>,
    pub keypair: Arc<Keypair>,
    pub rpc_client: Option<Arc<solana_client::rpc_client::RpcClient>>,
}

impl Pump {
    pub fn new(
        rpc_nonblocking_client: Arc<solana_client::nonblocking::rpc_client::RpcClient>,
        rpc_client: Arc<solana_client::rpc_client::RpcClient>,
        keypair: Arc<Keypair>,
    ) -> Self {
        Self {
            rpc_nonblocking_client,
            keypair,
            rpc_client: Some(rpc_client),
        }
    }

    pub async fn swap(&self, mint: &str, swap_config: SwapConfig) -> Result<Vec<String>> {
        let logger = Logger::new("[SWAP IN PUMP.FUN] => ".to_string());
        let slippage_bps = swap_config.slippage.saturating_mul(100);
        let owner = self.keypair.pubkey();
        let mint_pk =
            Pubkey::from_str(mint).map_err(|e| anyhow!("failed to parse mint pubkey: {e}"))?;
        let rpc = self
            .rpc_client
            .as_ref()
            .ok_or_else(|| anyhow!("rpc client missing"))?;

        let pump_program = Pubkey::from_str(PUMP_PROGRAM)?;
        let token_program = token::mint_token_program(rpc.as_ref(), &mint_pk).await?;
        let (bonding_curve, associated_bonding_curve, curve) =
            get_bonding_curve_account(rpc.clone(), &mint_pk, &pump_program, &token_program).await?;

        if curve.complete {
            return Err(anyhow!(
                "bonding curve complete; token has graduated from pump.fun"
            ));
        }

        let creator = curve.creator.ok_or_else(|| {
            anyhow!("bonding curve missing creator; cannot build trade accounts")
        })?;
        let fee_recipient = fetch_fee_recipient(rpc.as_ref(), curve.is_mayhem_mode)?;

        let token_ata = token::ata_for_mint(&owner, &mint_pk, &token_program);
        let mut instructions = Vec::new();

        match swap_config.swap_direction {
            SwapDirection::Buy => {
                if !token::account_exists(&self.rpc_nonblocking_client, &token_ata).await? {
                    instructions.push(create_associated_token_account_idempotent(
                        &owner,
                        &owner,
                        &mint_pk,
                        &token_program,
                    ));
                }

                let sol_in =
                    ui_amount_to_amount(swap_config.amount_in, spl_token::native_mint::DECIMALS);
                let expected = pump_buy_tokens_out(
                    sol_in,
                    curve.virtual_sol_reserves,
                    curve.virtual_token_reserves,
                );
                let min_tokens = min_amount_with_slippage(expected, slippage_bps);

                logger.log(format!(
                    "buy mint={mint} mayhem={} fee_recipient={fee_recipient} token_program={token_program} sol_in={sol_in} expected_tokens={expected} min_tokens={min_tokens}",
                    curve.is_mayhem_mode
                ));

                instructions.push(pump_buy_exact_sol_in_instruction(
                    &owner,
                    &mint_pk,
                    &bonding_curve,
                    &associated_bonding_curve,
                    &token_ata,
                    &token_program,
                    &creator,
                    &fee_recipient,
                    sol_in,
                    min_tokens,
                )?);
            }
            SwapDirection::Sell => {
                let bal = token::token_balance(&self.rpc_nonblocking_client, &token_ata).await?;
                let tokens_in = match swap_config.in_type {
                    SwapInType::Pct => {
                        let pct = swap_config.amount_in.clamp(0.0, 1.0);
                        if (pct - 1.0).abs() < f64::EPSILON {
                            bal
                        } else {
                            ((pct * bal as f64).floor()) as u64
                        }
                    }
                    SwapInType::Qty => {
                        if swap_config.amount_in >= 1.0 {
                            swap_config.amount_in as u64
                        } else {
                            bal
                        }
                    }
                };
                if tokens_in == 0 {
                    return Err(anyhow!("no tokens to sell for {mint}"));
                }

                let expected_sol = pump_sell_sol_out(
                    tokens_in,
                    curve.virtual_sol_reserves,
                    curve.virtual_token_reserves,
                );
                let min_sol = min_amount_with_slippage(expected_sol, slippage_bps);
                logger.log(format!(
                    "sell mint={mint} mayhem={} fee_recipient={fee_recipient} tokens_in={tokens_in} expected_sol={expected_sol} min_sol={min_sol}",
                    curve.is_mayhem_mode
                ));

                instructions.push(pump_sell_instruction(
                    &owner,
                    &mint_pk,
                    &bonding_curve,
                    &associated_bonding_curve,
                    &token_ata,
                    &token_program,
                    &creator,
                    &fee_recipient,
                    curve.is_cashback_coin,
                    tokens_in,
                    min_sol,
                )?);

                if matches!(swap_config.in_type, SwapInType::Pct)
                    && (swap_config.amount_in - 1.0).abs() < f64::EPSILON
                {
                    instructions.push(close_account(
                        &token_program,
                        &token_ata,
                        &owner,
                        &owner,
                        &[],
                    )?);
                }
            }
        }

        if instructions.is_empty() {
            return Err(anyhow!("instructions is empty, no tx required"));
        }

        tx::new_signed_and_send(
            rpc.as_ref(),
            &self.keypair,
            instructions,
            swap_config.use_jito,
            &logger,
        )
        .await
    }
}

fn pump_buy_exact_sol_in_instruction(
    user: &Pubkey,
    mint: &Pubkey,
    bonding_curve: &Pubkey,
    associated_bonding_curve: &Pubkey,
    user_ata: &Pubkey,
    token_program: &Pubkey,
    creator: &Pubkey,
    fee_recipient: &Pubkey,
    spendable_sol_in: u64,
    min_tokens_out: u64,
) -> Result<Instruction> {
    // Live buy_exact_sol_in txs use disc + 2×u64 (no track_volume byte).
    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&PUMP_BUY_EXACT_SOL_IN_DISC);
    data.extend_from_slice(&spendable_sol_in.to_le_bytes());
    data.extend_from_slice(&min_tokens_out.to_le_bytes());

    let mut accounts = pump_buy_accounts(
        user,
        mint,
        bonding_curve,
        associated_bonding_curve,
        user_ata,
        token_program,
        creator,
        fee_recipient,
    )?;
    accounts.push(AccountMeta::new_readonly(
        bonding_curve_v2_pda(mint)?,
        false,
    ));
    accounts.push(AccountMeta::new(
        Pubkey::from_str(PUMP_BREAKING_FEE_RECIPIENT)?,
        false,
    ));

    Ok(Instruction {
        program_id: Pubkey::from_str(PUMP_PROGRAM)?,
        accounts,
        data,
    })
}

fn pump_sell_instruction(
    user: &Pubkey,
    mint: &Pubkey,
    bonding_curve: &Pubkey,
    associated_bonding_curve: &Pubkey,
    user_ata: &Pubkey,
    token_program: &Pubkey,
    creator: &Pubkey,
    fee_recipient: &Pubkey,
    is_cashback_coin: bool,
    token_amount: u64,
    min_sol_output: u64,
) -> Result<Instruction> {
    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&PUMP_SELL_DISC);
    data.extend_from_slice(&token_amount.to_le_bytes());
    data.extend_from_slice(&min_sol_output.to_le_bytes());

    let mut accounts = pump_sell_accounts(
        user,
        mint,
        bonding_curve,
        associated_bonding_curve,
        user_ata,
        token_program,
        creator,
        fee_recipient,
    )?;

    if is_cashback_coin {
        accounts.push(AccountMeta::new(user_volume_accumulator_pda(user)?, false));
    }
    accounts.push(AccountMeta::new_readonly(
        bonding_curve_v2_pda(mint)?,
        false,
    ));
    accounts.push(AccountMeta::new(
        Pubkey::from_str(PUMP_BREAKING_FEE_RECIPIENT)?,
        false,
    ));

    Ok(Instruction {
        program_id: Pubkey::from_str(PUMP_PROGRAM)?,
        accounts,
        data,
    })
}

fn pump_buy_accounts(
    user: &Pubkey,
    mint: &Pubkey,
    bonding_curve: &Pubkey,
    associated_bonding_curve: &Pubkey,
    user_ata: &Pubkey,
    token_program: &Pubkey,
    creator: &Pubkey,
    fee_recipient: &Pubkey,
) -> Result<Vec<AccountMeta>> {
    let program = Pubkey::from_str(PUMP_PROGRAM)?;
    Ok(vec![
        AccountMeta::new_readonly(Pubkey::from_str(PUMP_GLOBAL)?, false),
        AccountMeta::new(*fee_recipient, false),
        AccountMeta::new_readonly(*mint, false),
        AccountMeta::new(*bonding_curve, false),
        AccountMeta::new(*associated_bonding_curve, false),
        AccountMeta::new(*user_ata, false),
        AccountMeta::new(*user, true),
        AccountMeta::new_readonly(system_program::ID, false),
        AccountMeta::new_readonly(*token_program, false),
        AccountMeta::new(creator_vault_pda(creator)?, false),
        AccountMeta::new_readonly(Pubkey::from_str(PUMP_EVENT_AUTHORITY)?, false),
        AccountMeta::new_readonly(program, false),
        AccountMeta::new_readonly(Pubkey::from_str(PUMP_GLOBAL_VOLUME_ACCUMULATOR)?, false),
        AccountMeta::new(user_volume_accumulator_pda(user)?, false),
        AccountMeta::new_readonly(Pubkey::from_str(PUMP_FEE_CONFIG)?, false),
        AccountMeta::new_readonly(Pubkey::from_str(PUMP_FEE_PROGRAM)?, false),
    ])
}

fn pump_sell_accounts(
    user: &Pubkey,
    mint: &Pubkey,
    bonding_curve: &Pubkey,
    associated_bonding_curve: &Pubkey,
    user_ata: &Pubkey,
    token_program: &Pubkey,
    creator: &Pubkey,
    fee_recipient: &Pubkey,
) -> Result<Vec<AccountMeta>> {
    let program = Pubkey::from_str(PUMP_PROGRAM)?;
    Ok(vec![
        AccountMeta::new_readonly(Pubkey::from_str(PUMP_GLOBAL)?, false),
        AccountMeta::new(*fee_recipient, false),
        AccountMeta::new_readonly(*mint, false),
        AccountMeta::new(*bonding_curve, false),
        AccountMeta::new(*associated_bonding_curve, false),
        AccountMeta::new(*user_ata, false),
        AccountMeta::new(*user, true),
        AccountMeta::new_readonly(system_program::ID, false),
        AccountMeta::new(creator_vault_pda(creator)?, false),
        AccountMeta::new_readonly(*token_program, false),
        AccountMeta::new_readonly(Pubkey::from_str(PUMP_EVENT_AUTHORITY)?, false),
        AccountMeta::new_readonly(program, false),
        AccountMeta::new_readonly(Pubkey::from_str(PUMP_FEE_CONFIG)?, false),
        AccountMeta::new_readonly(Pubkey::from_str(PUMP_FEE_PROGRAM)?, false),
    ])
}

/// Pick a valid protocol fee recipient from Global.
/// Mayhem curves must use `reserved_fee_recipients`; regular curves use `fee_recipients`.
/// Wrong pool → Custom(6000) NotAuthorized.
fn fetch_fee_recipient(
    rpc: &solana_client::rpc_client::RpcClient,
    is_mayhem_mode: bool,
) -> Result<Pubkey> {
    let global_pk = Pubkey::from_str(PUMP_GLOBAL)?;
    let data = rpc
        .get_account_data(&global_pk)
        .map_err(|e| anyhow!("failed to fetch pump global: {e}"))?;
    let (regular, mayhem) = parse_global_fee_recipients(&data)?;
    Ok(if is_mayhem_mode { mayhem } else { regular })
}

fn parse_global_fee_recipients(data: &[u8]) -> Result<(Pubkey, Pubkey)> {
    // Borsh layout after 8-byte discriminator (see pump Global account).
    // Walk fixed fields up to fee_recipients[0] and reserved_fee_recipients[0].
    let mut o = 8usize;
    let need = |o: usize, n: usize| -> Result<()> {
        if data.len() < o + n {
            Err(anyhow!("pump global account too short: {} bytes", data.len()))
        } else {
            Ok(())
        }
    };
    let read_pk = |o: &mut usize| -> Result<Pubkey> {
        need(*o, 32)?;
        let pk = Pubkey::try_from(&data[*o..*o + 32])
            .map_err(|_| anyhow!("invalid pubkey in global"))?;
        *o += 32;
        Ok(pk)
    };

    need(o, 1)?;
    o += 1; // initialized
    let _authority = read_pk(&mut o)?;
    let _fee_recipient = read_pk(&mut o)?;
    need(o, 8 * 5)?;
    o += 8 * 5; // 5×u64 reserves/fees
    let _withdraw_authority = read_pk(&mut o)?;
    need(o, 1)?;
    o += 1; // enable_migrate
    need(o, 8 + 8)?;
    o += 8 + 8; // pool_migration_fee, creator_fee_basis_points

    // fee_recipients: [Pubkey; 7]
    let regular = read_pk(&mut o)?;
    for _ in 0..6 {
        let _ = read_pk(&mut o)?;
    }
    let _set_creator_authority = read_pk(&mut o)?;
    let _admin_set_creator_authority = read_pk(&mut o)?;
    need(o, 1)?;
    o += 1; // create_v2_enabled
    let _whitelist_pda = read_pk(&mut o)?;
    let reserved_fee_recipient = read_pk(&mut o)?;
    need(o, 1)?;
    o += 1; // mayhem_mode_enabled

    // reserved_fee_recipients: [Pubkey; 7] — prefer [0], fall back to reserved_fee_recipient
    let mayhem0 = read_pk(&mut o)?;
    let mayhem = if mayhem0.to_bytes() != [0u8; 32] {
        mayhem0
    } else {
        reserved_fee_recipient
    };

    if regular.to_bytes() == [0u8; 32] || mayhem.to_bytes() == [0u8; 32] {
        return Err(anyhow!("pump global fee recipients missing"));
    }
    Ok((regular, mayhem))
}

fn creator_vault_pda(creator: &Pubkey) -> Result<Pubkey> {
    let program = Pubkey::from_str(PUMP_PROGRAM)?;
    Ok(Pubkey::find_program_address(&[b"creator-vault", creator.as_ref()], &program).0)
}

fn user_volume_accumulator_pda(user: &Pubkey) -> Result<Pubkey> {
    let program = Pubkey::from_str(PUMP_PROGRAM)?;
    Ok(
        Pubkey::find_program_address(&[b"user_volume_accumulator", user.as_ref()], &program).0,
    )
}

fn bonding_curve_v2_pda(mint: &Pubkey) -> Result<Pubkey> {
    let program = Pubkey::from_str(PUMP_PROGRAM)?;
    Ok(Pubkey::find_program_address(&[b"bonding-curve-v2", mint.as_ref()], &program).0)
}

fn min_amount_with_slippage(input_amount: u64, slippage_bps: u64) -> u64 {
    input_amount
        .saturating_mul(TEN_THOUSAND.saturating_sub(slippage_bps))
        / TEN_THOUSAND
}

fn amount_after_fee(amount: u64, fee_bps: u64) -> u64 {
    amount.saturating_sub(amount.saturating_mul(fee_bps) / TEN_THOUSAND)
}

pub fn pump_buy_tokens_out(sol_in: u64, virtual_sol: u64, virtual_token: u64) -> u64 {
    if sol_in == 0 || virtual_sol == 0 || virtual_token == 0 {
        return 0;
    }
    let sol_after_fee = amount_after_fee(sol_in, PUMP_FEE_BPS) as u128;
    let v_sol = virtual_sol as u128;
    let v_token = virtual_token as u128;
    let k = v_sol.saturating_mul(v_token);
    let new_sol = v_sol.saturating_add(sol_after_fee);
    if new_sol == 0 {
        return 0;
    }
    let new_token = k / new_sol;
    v_token.saturating_sub(new_token) as u64
}

pub fn pump_sell_sol_out(tokens_in: u64, virtual_sol: u64, virtual_token: u64) -> u64 {
    if tokens_in == 0 || virtual_sol == 0 || virtual_token == 0 {
        return 0;
    }
    let t_in = tokens_in as u128;
    let v_sol = virtual_sol as u128;
    let v_token = virtual_token as u128;
    let k = v_sol.saturating_mul(v_token);
    let new_token = v_token.saturating_add(t_in);
    if new_token == 0 {
        return 0;
    }
    let new_sol = k / new_token;
    let sol_out = v_sol.saturating_sub(new_sol) as u64;
    amount_after_fee(sol_out, PUMP_FEE_BPS)
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RaydiumInfo {
    pub base: f64,
    pub quote: f64,
    pub price: f64,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PumpInfo {
    pub mint: String,
    pub bonding_curve: String,
    pub associated_bonding_curve: String,
    pub raydium_pool: Option<String>,
    pub raydium_info: Option<RaydiumInfo>,
    pub complete: bool,
    pub virtual_sol_reserves: u64,
    pub virtual_token_reserves: u64,
    pub total_supply: u64,
}

#[derive(Debug, Clone)]
pub struct BondingCurveAccount {
    pub discriminator: u64,
    pub virtual_token_reserves: u64,
    pub virtual_sol_reserves: u64,
    pub real_token_reserves: u64,
    pub real_sol_reserves: u64,
    pub token_total_supply: u64,
    pub complete: bool,
    pub creator: Option<Pubkey>,
    pub is_mayhem_mode: bool,
    pub is_cashback_coin: bool,
}

pub async fn get_bonding_curve_account(
    rpc_client: Arc<solana_client::rpc_client::RpcClient>,
    mint: &Pubkey,
    program_id: &Pubkey,
    token_program: &Pubkey,
) -> Result<(Pubkey, Pubkey, BondingCurveAccount)> {
    let bonding_curve = get_pda(mint, program_id)?;
    let associated_bonding_curve = token::ata_for_mint(&bonding_curve, mint, token_program);
    let bonding_curve_data = rpc_client.get_account_data(&bonding_curve).map_err(|err| {
        anyhow!("Failed to get bonding curve account data: {bonding_curve}, err: {err}")
    })?;
    let bonding_curve_account = parse_bonding_curve(&bonding_curve_data)?;
    Ok((
        bonding_curve,
        associated_bonding_curve,
        bonding_curve_account,
    ))
}

pub fn get_pda(mint: &Pubkey, program_id: &Pubkey) -> Result<Pubkey> {
    let seeds = [b"bonding-curve".as_ref(), mint.as_ref()];
    let (bonding_curve, _bump) = Pubkey::find_program_address(&seeds, program_id);
    Ok(bonding_curve)
}

fn parse_bonding_curve(data: &[u8]) -> Result<BondingCurveAccount> {
    if data.len() < 49 {
        return Err(anyhow!("bonding curve data too short: {} bytes", data.len()));
    }
    let u64_at = |offset: usize| -> Result<u64> {
        let bytes: [u8; 8] = data[offset..offset + 8]
            .try_into()
            .map_err(|_| anyhow!("bonding curve slice"))?;
        Ok(u64::from_le_bytes(bytes))
    };
    let creator = if data.len() >= 81 {
        Pubkey::try_from(&data[49..81]).ok()
    } else {
        None
    };
    // Layout after creator: is_mayhem_mode (81), is_cashback_coin (82) on newer accounts.
    let is_mayhem_mode = data.get(81).copied().unwrap_or(0) != 0;
    let is_cashback_coin = data.get(82).copied().unwrap_or(0) != 0;
    Ok(BondingCurveAccount {
        discriminator: u64_at(0)?,
        virtual_token_reserves: u64_at(8)?,
        virtual_sol_reserves: u64_at(16)?,
        real_token_reserves: u64_at(24)?,
        real_sol_reserves: u64_at(32)?,
        token_total_supply: u64_at(40)?,
        complete: data[48] != 0,
        creator,
        is_mayhem_mode,
        is_cashback_coin,
    })
}

pub async fn get_pump_info(
    rpc_client: Arc<solana_client::rpc_client::RpcClient>,
    mint: &str,
) -> Result<PumpInfo> {
    let mint = Pubkey::from_str(mint)?;
    let program_id = Pubkey::from_str(PUMP_PROGRAM)?;
    let token_program = token::mint_token_program(rpc_client.as_ref(), &mint).await?;
    let (bonding_curve, associated_bonding_curve, bonding_curve_account) =
        get_bonding_curve_account(rpc_client, &mint, &program_id, &token_program).await?;

    Ok(PumpInfo {
        mint: mint.to_string(),
        bonding_curve: bonding_curve.to_string(),
        associated_bonding_curve: associated_bonding_curve.to_string(),
        raydium_pool: None,
        raydium_info: None,
        complete: bonding_curve_account.complete,
        virtual_sol_reserves: bonding_curve_account.virtual_sol_reserves,
        virtual_token_reserves: bonding_curve_account.virtual_token_reserves,
        total_supply: bonding_curve_account.token_total_supply,
    })
}

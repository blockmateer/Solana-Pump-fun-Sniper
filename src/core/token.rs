use anyhow::{anyhow, Result};
use solana_sdk::{pubkey::Pubkey, signature::Keypair};
use spl_associated_token_account::get_associated_token_address_with_program_id;
use std::sync::Arc;

pub const TOKEN_2022_PROGRAM_ID: Pubkey =
    solana_sdk::pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

pub fn get_associated_token_address_for(
    _client: Arc<solana_client::nonblocking::rpc_client::RpcClient>,
    _keypair: Arc<Keypair>,
    mint: &Pubkey,
    owner: &Pubkey,
) -> Pubkey {
    get_associated_token_address_with_program_id(owner, mint, &spl_token::ID)
}

pub fn ata_for_mint(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    get_associated_token_address_with_program_id(owner, mint, token_program)
}

pub fn is_token_2022(token_program: &Pubkey) -> bool {
    *token_program == TOKEN_2022_PROGRAM_ID
}

pub async fn mint_token_program(
    client: &solana_client::rpc_client::RpcClient,
    mint: &Pubkey,
) -> Result<Pubkey> {
    let account = client
        .get_account(mint)
        .map_err(|e| anyhow!("failed to fetch mint {mint}: {e}"))?;
    if account.owner == spl_token::ID || account.owner == TOKEN_2022_PROGRAM_ID {
        Ok(account.owner)
    } else {
        Err(anyhow!(
            "mint {mint} owned by unexpected program {}",
            account.owner
        ))
    }
}

pub async fn account_exists(
    client: &solana_client::nonblocking::rpc_client::RpcClient,
    ata: &Pubkey,
) -> Result<bool> {
    match client.get_account(ata).await {
        Ok(_) => Ok(true),
        Err(err) => {
            let msg = err.to_string().to_lowercase();
            // Solana clients report this as "AccountNotFound: pubkey=..." (no spaces).
            if msg.contains("accountnotfound")
                || msg.contains("account not found")
                || msg.contains("could not find account")
            {
                Ok(false)
            } else {
                Err(anyhow!(err))
            }
        }
    }
}

pub async fn token_balance(
    client: &solana_client::nonblocking::rpc_client::RpcClient,
    ata: &Pubkey,
) -> Result<u64> {
    let bal = client.get_token_account_balance(ata).await?;
    bal.amount
        .parse::<u64>()
        .map_err(|e| anyhow!("parse token amount: {e}"))
}

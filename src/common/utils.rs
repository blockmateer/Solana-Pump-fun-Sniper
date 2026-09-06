use anyhow::Result;
use solana_sdk::{commitment_config::CommitmentConfig, signature::Keypair, signer::Signer};
use std::{env, fs, path::Path, sync::Arc};

use crate::engine::swap::{SwapDirection, SwapInType};

#[derive(Clone)]
pub struct AppState {
    pub rpc_client: Arc<solana_client::rpc_client::RpcClient>,
    pub rpc_nonblocking_client: Arc<solana_client::nonblocking::rpc_client::RpcClient>,
    pub wallet: Arc<Keypair>,
}

#[derive(Clone, Debug)]
pub struct SwapConfig {
    pub swap_direction: SwapDirection,
    pub in_type: SwapInType,
    pub amount_in: f64,
    pub slippage: u64,
    pub use_jito: bool,
}

pub fn import_env_var(key: &str) -> String {
    env::var(key).unwrap_or_else(|_| panic!("Environment variable {key} is not set"))
}

pub fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

pub fn create_rpc_client() -> Result<Arc<solana_client::rpc_client::RpcClient>> {
    let rpc_https = import_env_var("RPC_HTTPS");
    let rpc_client = solana_client::rpc_client::RpcClient::new_with_commitment(
        rpc_https,
        CommitmentConfig::processed(),
    );
    Ok(Arc::new(rpc_client))
}

pub async fn create_nonblocking_rpc_client(
) -> Result<Arc<solana_client::nonblocking::rpc_client::RpcClient>> {
    let rpc_https = import_env_var("RPC_HTTPS");
    let rpc_client = solana_client::nonblocking::rpc_client::RpcClient::new_with_commitment(
        rpc_https,
        CommitmentConfig::processed(),
    );
    Ok(Arc::new(rpc_client))
}

pub fn import_wallet() -> Result<Arc<Keypair>> {
    let priv_key = import_env_var("PRIVATE_KEY");
    let bytes = bs58::decode(priv_key.trim())
        .into_vec()
        .map_err(|e| anyhow::anyhow!("invalid PRIVATE_KEY base58: {e}"))?;
    let wallet = Keypair::try_from(bytes.as_slice()).map_err(|e| {
        anyhow::anyhow!("invalid PRIVATE_KEY keypair (need 64-byte secret): {e}")
    })?;
    let _ = wallet.pubkey();
    Ok(Arc::new(wallet))
}

pub fn load_blacklist(path: impl AsRef<Path>) -> Vec<String> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|s| s.to_string())
        .collect()
}

pub fn default_buy_config(slippage: u64, use_jito: bool) -> SwapConfig {
    let amount_in = env_or("TOKEN_AMOUNT", "0.01")
        .parse::<f64>()
        .unwrap_or(0.01);
    SwapConfig {
        swap_direction: SwapDirection::Buy,
        in_type: SwapInType::Qty,
        amount_in,
        slippage,
        use_jito,
    }
}

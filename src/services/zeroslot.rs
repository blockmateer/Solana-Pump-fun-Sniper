//! 0slot / zeroslot fast-landing relay stub.

use anyhow::{anyhow, Result};
use serde_json::json;
use solana_sdk::transaction::VersionedTransaction;
use std::env;

pub async fn send_transaction(tx: &VersionedTransaction) -> Result<String> {
    let url = env::var("ZEROSLOT_API_URL").map_err(|_| anyhow!("ZEROSLOT_API_URL is not set"))?;
    let api_key = env::var("ZEROSLOT_API_KEY").unwrap_or_default();
    let bytes = bincode::serialize(tx)?;
    let encoded = bs58::encode(bytes).into_string();
    let mut req = reqwest::Client::new().post(url).json(&json!({ "transaction": encoded }));
    if !api_key.is_empty() {
        req = req.header("x-api-key", api_key);
    }
    let body: serde_json::Value = req.send().await?.error_for_status()?.json().await?;
    body.get("signature")
        .or_else(|| body.get("result"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("zeroslot response missing signature: {body}"))
}

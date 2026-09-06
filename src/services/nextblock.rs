//! NextBlock fast-send service stub.
//! Wire your NextBlock API key via `NEXTBLOCK_API_URL` / `NEXTBLOCK_API_KEY` when available.

use anyhow::{anyhow, Result};
use serde_json::json;
use solana_sdk::transaction::VersionedTransaction;
use std::env;

pub async fn send_transaction(tx: &VersionedTransaction) -> Result<String> {
    let url = env::var("NEXTBLOCK_API_URL")
        .unwrap_or_else(|_| "https://ny.nextblock.io/api/v2/submit".to_string());
    let api_key = env::var("NEXTBLOCK_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        return Err(anyhow!("NEXTBLOCK_API_KEY is not set"));
    }

    let bytes = bincode::serialize(tx)?;
    let encoded = bs58::encode(bytes).into_string();
    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .header("Authorization", api_key)
        .json(&json!({ "transaction": encoded }))
        .send()
        .await?
        .error_for_status()?;
    let body: serde_json::Value = resp.json().await?;
    body.get("signature")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("nextblock response missing signature: {body}"))
}

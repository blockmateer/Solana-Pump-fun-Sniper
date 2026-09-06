use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use rand::seq::SliceRandom;
use reqwest::Client;
use serde_json::{json, Value};
use solana_sdk::{
    pubkey::Pubkey,
    transaction::{Transaction, VersionedTransaction},
};
use std::{env, str::FromStr, sync::LazyLock, time::Duration};
use tokio::time::sleep;

use crate::common::logger::Logger;

const DEFAULT_TIPS: &[&str] = &[
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
];

pub static BLOCK_ENGINE_URL: LazyLock<String> = LazyLock::new(|| {
    env::var("JITO_BLOCK_ENGINE_URL")
        .unwrap_or_else(|_| "https://ny.mainnet.block-engine.jito.wtf".to_string())
});

static TIP_ACCOUNTS: Mutex<Vec<Pubkey>> = Mutex::new(Vec::new());
static HTTP: Mutex<Option<Client>> = Mutex::new(None);

pub async fn init_tip_accounts() -> Result<()> {
    let mut tips = Vec::new();
    for t in DEFAULT_TIPS {
        if let Ok(pk) = Pubkey::from_str(t) {
            tips.push(pk);
        }
    }

    // Prefer live tip accounts from Jito when reachable.
    if let Ok(client) = Client::builder().timeout(Duration::from_secs(8)).build() {
        let url = format!("{}/api/v1/bundles", BLOCK_ENGINE_URL.trim_end_matches('/'));
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getTipAccounts",
            "params": []
        });
        if let Ok(resp) = client.post(&url).json(&body).send().await {
            if let Ok(v) = resp.json::<Value>().await {
                if let Some(arr) = v.get("result").and_then(|r| r.as_array()) {
                    let live: Vec<Pubkey> = arr
                        .iter()
                        .filter_map(|x| x.as_str())
                        .filter_map(|s| Pubkey::from_str(s).ok())
                        .collect();
                    if !live.is_empty() {
                        tips = live;
                    }
                }
            }
        }
        *HTTP.lock() = Some(client);
    }

    *TIP_ACCOUNTS.lock() = tips;
    Ok(())
}

pub async fn get_tip_account() -> Result<Pubkey> {
    {
        let guard = TIP_ACCOUNTS.lock();
        if !guard.is_empty() {
            let mut rng = rand::thread_rng();
            return guard
                .choose(&mut rng)
                .cloned()
                .ok_or_else(|| anyhow!("no jito tip accounts"));
        }
    }
    init_tip_accounts().await?;
    let guard = TIP_ACCOUNTS.lock();
    guard
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("no jito tip accounts"))
}

pub async fn get_tip_value() -> Result<f64> {
    if let Ok(tip_value) = env::var("JITO_TIP_VALUE") {
        if let Ok(value) = tip_value.parse::<f64>() {
            return Ok(value);
        }
    }
    // 0.0001 SOL is often too low for contested pump.fun snipes.
    Ok(0.001)
}

fn http() -> Result<Client> {
    HTTP.lock()
        .clone()
        .ok_or_else(|| anyhow!("jito http client not initialized; call init_tip_accounts first"))
}

fn encode_tx(tx: &Transaction) -> Result<String> {
    let bytes = bincode::serialize(tx)?;
    Ok(bs58::encode(bytes).into_string())
}

fn encode_vtx(tx: &VersionedTransaction) -> Result<String> {
    let bytes = bincode::serialize(tx)?;
    Ok(bs58::encode(bytes).into_string())
}

pub async fn send_bundle(txs: &[Transaction]) -> Result<String> {
    let encoded = txs.iter().map(encode_tx).collect::<Result<Vec<_>>>()?;
    send_encoded_bundle(encoded).await
}

pub async fn send_versioned_bundle(txs: &[VersionedTransaction]) -> Result<String> {
    let encoded = txs.iter().map(encode_vtx).collect::<Result<Vec<_>>>()?;
    send_encoded_bundle(encoded).await
}

async fn send_encoded_bundle(encoded: Vec<String>) -> Result<String> {
    let url = format!("{}/api/v1/bundles", BLOCK_ENGINE_URL.trim_end_matches('/'));
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sendBundle",
        "params": [encoded]
    });
    let resp: Value = http()?.post(url).json(&body).send().await?.json().await?;
    if let Some(err) = resp.get("error") {
        return Err(anyhow!("jito sendBundle error: {err}"));
    }
    resp.get("result")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("jito sendBundle missing result: {resp}"))
}

pub async fn get_bundle_statuses(bundle_id: &str) -> Result<Value> {
    let url = format!("{}/api/v1/bundles", BLOCK_ENGINE_URL.trim_end_matches('/'));
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getBundleStatuses",
        "params": [[bundle_id]]
    });
    Ok(http()?.post(url).json(&body).send().await?.json().await?)
}

pub async fn wait_for_bundle_confirmation(
    rpc: &solana_client::rpc_client::RpcClient,
    bundle_id: &str,
    sig: &solana_sdk::signature::Signature,
    interval: Duration,
    timeout: Duration,
    logger: &Logger,
) -> Result<Vec<String>> {
    use solana_sdk::commitment_config::CommitmentConfig;

    let start = tokio::time::Instant::now();
    loop {
        if start.elapsed() > timeout {
            return Err(anyhow!("jito bundle {bundle_id} confirmation timed out"));
        }

        // Prefer on-chain signature status — getBundleStatuses is often null while pending.
        if let Ok(resp) = rpc.get_signature_statuses(&[*sig]) {
            if let Some(Some(status)) = resp.value.first() {
                if let Some(err) = &status.err {
                    return Err(anyhow!("bundle tx failed on-chain: {err:?}"));
                }
                if status.satisfies_commitment(CommitmentConfig::confirmed())
                    || status.confirmation_status.as_ref().is_some_and(|c| {
                        matches!(
                            c,
                            solana_transaction_status::TransactionConfirmationStatus::Confirmed
                                | solana_transaction_status::TransactionConfirmationStatus::Finalized
                        )
                    })
                {
                    return Ok(vec![sig.to_string()]);
                }
            }
        }

        match get_bundle_statuses(bundle_id).await {
            Ok(resp) => {
                if let Some(value) = resp
                    .pointer("/result/value")
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.first())
                {
                    if value.is_null() {
                        // still pending
                    } else if let Some(err) = value.get("err") {
                        if !err.is_null() {
                            return Err(anyhow!("bundle landed with error: {err}"));
                        }
                    }
                    let status = value
                        .get("confirmation_status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    if matches!(status, "confirmed" | "finalized" | "processed") {
                        if let Some(txs) = value.get("transactions").and_then(|v| v.as_array()) {
                            let sigs: Vec<String> = txs
                                .iter()
                                .filter_map(|t| t.as_str().map(|s| s.to_string()))
                                .collect();
                            if !sigs.is_empty() {
                                return Ok(sigs);
                            }
                        }
                        return Ok(vec![sig.to_string()]);
                    }
                }
            }
            Err(err) => {
                logger.log(format!("bundle status poll: {err}"));
            }
        }
        sleep(interval).await;
    }
}

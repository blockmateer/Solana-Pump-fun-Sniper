//! Helius / standard Solana WebSocket log subscription monitor.
//! Detects new Pump.fun token creates and Raydium pool initializes, then buys.

use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_client::rpc_config::{
    RpcTransactionConfig, RpcTransactionLogsConfig, RpcTransactionLogsFilter,
};
use solana_sdk::{commitment_config::CommitmentConfig, signature::Signature};
use solana_transaction_status::{
    EncodedConfirmedTransactionWithStatusMeta, EncodedTransaction, UiInstruction, UiMessage,
    UiTransactionEncoding,
};
use std::str::FromStr;
use std::sync::Arc;

use crate::common::logger::Logger;
use crate::common::utils::{default_buy_config, env_or, load_blacklist, AppState, SwapConfig};
use crate::dex::pumpfun::{self, Pump};
use crate::engine::seller::{self, SellStrategy};
use crate::engine::swap::{SwapDirection, SwapInType};

const PUMP_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
const RAYDIUM_AMM_V4: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

pub async fn pumpfun_monitor(
    rpc_wss: &str,
    state: AppState,
    slippage: u64,
    use_jito: bool,
) {
    let logger = Logger::new("[PUMPFUN MONITOR] => ".to_string());
    if let Err(err) = run_monitor(rpc_wss, state, slippage, use_jito, PUMP_PROGRAM, "pump.fun").await
    {
        logger.error(format!("monitor stopped: {err}"));
    }
}

pub async fn raydium_monitor(
    rpc_wss: &str,
    state: AppState,
    slippage: u64,
    use_jito: bool,
) {
    let logger = Logger::new("[RAYDIUM MONITOR] => ".to_string());
    if let Err(err) =
        run_monitor(rpc_wss, state, slippage, use_jito, RAYDIUM_AMM_V4, "raydium").await
    {
        logger.error(format!("monitor stopped: {err}"));
    }
}

async fn run_monitor(
    rpc_wss: &str,
    state: AppState,
    slippage: u64,
    use_jito: bool,
    program: &str,
    label: &str,
) -> Result<()> {
    let logger = Logger::new(format!("[MONITOR {label}] => "));
    let blacklist = Arc::new(load_blacklist("trial/blacklist.txt"));
    logger.log(format!(
        "subscribing to logs for {program} (blacklist={})",
        blacklist.len()
    ));

    let pubsub = PubsubClient::new(rpc_wss)
        .await
        .map_err(|e| anyhow!("websocket connect failed: {e}"))?;

    let (mut stream, _unsub) = pubsub
        .logs_subscribe(
            RpcTransactionLogsFilter::Mentions(vec![program.to_string()]),
            RpcTransactionLogsConfig {
                commitment: Some(CommitmentConfig::processed()),
            },
        )
        .await
        .map_err(|e| anyhow!("logsSubscribe failed: {e}"))?;

    while let Some(response) = stream.next().await {
        let value = response.value;
        if value.err.is_some() {
            continue;
        }
        let logs = value.logs.join("\n");
        let kind = classify_logs(&logs, label);
        if kind == EventKind::Ignore {
            continue;
        }

        let sig = match Signature::from_str(&value.signature) {
            Ok(s) => s,
            Err(_) => continue,
        };

        match fetch_tx(&state, &sig).await {
            Ok(tx) => {
                if let Some(mint) = extract_mint(label, kind, &tx) {
                    if blacklist.iter().any(|b| b == &mint) {
                        logger.log(format!("skip blacklisted mint {mint}"));
                        continue;
                    }
                    logger.log(format!(
                        "detected {:?} mint={mint} sig={}",
                        kind, value.signature
                    ));
                    if let Err(err) =
                        try_buy(&state, &mint, slippage, use_jito, label, &logger).await
                    {
                        logger.error(format!("buy failed for {mint}: {err}"));
                    }
                }
            }
            Err(err) => {
                logger.debug(format!("getTransaction {}: {err}", value.signature));
            }
        }
    }

    Err(anyhow!("{label} websocket stream ended"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventKind {
    Create,
    NewPool,
    Ignore,
}

fn classify_logs(logs: &str, label: &str) -> EventKind {
    let lower = logs.to_lowercase();
    if label == "pump.fun" && lower.contains("instruction: create") {
        return EventKind::Create;
    }
    if label == "raydium"
        && (lower.contains("initialize2") || lower.contains("instruction: initialize"))
    {
        return EventKind::NewPool;
    }
    EventKind::Ignore
}

async fn fetch_tx(
    state: &AppState,
    sig: &Signature,
) -> Result<EncodedConfirmedTransactionWithStatusMeta> {
    state
        .rpc_nonblocking_client
        .get_transaction_with_config(
            sig,
            RpcTransactionConfig {
                encoding: Some(UiTransactionEncoding::Json),
                commitment: Some(CommitmentConfig::confirmed()),
                max_supported_transaction_version: Some(0),
            },
        )
        .await
        .map_err(|e| anyhow!("{e}"))
}

fn extract_mint(
    label: &str,
    kind: EventKind,
    tx: &EncodedConfirmedTransactionWithStatusMeta,
) -> Option<String> {
    let keys = account_keys(tx)?;
    match (label, kind) {
        ("pump.fun", EventKind::Create) => pump_ix_account(&keys, tx, 0),
        ("raydium", EventKind::NewPool) => keys.get(8).cloned().or_else(|| keys.get(1).cloned()),
        _ => None,
    }
}

fn account_keys(tx: &EncodedConfirmedTransactionWithStatusMeta) -> Option<Vec<String>> {
    match &tx.transaction.transaction {
        EncodedTransaction::Json(ui) => match &ui.message {
            UiMessage::Raw(raw) => Some(raw.account_keys.clone()),
            UiMessage::Parsed(parsed) => Some(
                parsed
                    .account_keys
                    .iter()
                    .map(|k| k.pubkey.clone())
                    .collect(),
            ),
        },
        _ => None,
    }
}

fn pump_ix_account(
    keys: &[String],
    tx: &EncodedConfirmedTransactionWithStatusMeta,
    account_index: usize,
) -> Option<String> {
    let EncodedTransaction::Json(ui) = &tx.transaction.transaction else {
        return None;
    };
    match &ui.message {
        UiMessage::Raw(raw) => raw
            .instructions
            .iter()
            .filter(|ix| {
                keys.get(ix.program_id_index as usize)
                    .map(|p| p == PUMP_PROGRAM)
                    .unwrap_or(false)
            })
            .filter_map(|ix| {
                ix.accounts
                    .get(account_index)
                    .and_then(|i| keys.get(*i as usize))
                    .cloned()
            })
            .next(),
        UiMessage::Parsed(parsed) => parsed.instructions.iter().find_map(|ix| match ix {
            UiInstruction::Compiled(c) => {
                if keys.get(c.program_id_index as usize).map(|p| p.as_str()) != Some(PUMP_PROGRAM)
                {
                    return None;
                }
                c.accounts
                    .get(account_index)
                    .and_then(|i| keys.get(*i as usize))
                    .cloned()
            }
            UiInstruction::Parsed(_) => None,
        }),
    }
}

async fn try_buy(
    state: &AppState,
    mint: &str,
    slippage: u64,
    use_jito: bool,
    label: &str,
    logger: &Logger,
) -> Result<()> {
    if label != "pump.fun" {
        logger.log(format!(
            "raydium pool detected for {mint}; pump.fun buy path only in basic version"
        ));
        return Ok(());
    }

    let info = pumpfun::get_pump_info(state.rpc_client.clone(), mint).await?;
    if info.complete {
        logger.log(format!("skip {mint}: bonding curve already complete"));
        return Ok(());
    }

    let mut cfg: SwapConfig = default_buy_config(slippage, use_jito);
    cfg.swap_direction = SwapDirection::Buy;
    cfg.in_type = SwapInType::Qty;

    let pump = Pump::new(
        state.rpc_nonblocking_client.clone(),
        state.rpc_client.clone(),
        state.wallet.clone(),
    );
    let sigs = pump.swap(mint, cfg).await?;
    logger.log(format!("bought {mint}: {sigs:?}"));

    if seller::auto_sell_enabled() {
        let entry_sol = env_or("TOKEN_AMOUNT", "0.01")
            .parse::<f64>()
            .unwrap_or(0.01);
        seller::spawn_position_manager(
            state.clone(),
            mint.to_string(),
            entry_sol,
            SellStrategy::from_env(slippage, use_jito),
        );
        logger.log(format!(
            "auto-sell watcher started for {mint} (TP/SL/TIME_EXCEED/trailing)"
        ));
    }

    Ok(())
}

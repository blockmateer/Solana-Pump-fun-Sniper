//! Position exit manager: take-profit, stop-loss, timeout, and trailing stop.

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use solana_sdk::signer::Signer;
use std::{
    collections::HashSet,
    env,
    sync::LazyLock,
    time::{Duration, Instant},
};

use crate::{
    common::{
        logger::Logger,
        utils::{env_or, AppState, SwapConfig},
    },
    core::token,
    dex::pumpfun::{self, Pump},
    engine::swap::{SwapDirection, SwapInType},
};

static ACTIVE_MINTS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

#[derive(Clone, Debug)]
pub struct SellStrategy {
    /// Exit when expected SOL out / entry SOL >= this (e.g. 3.0 = 3x).
    pub take_profit: f64,
    /// Exit when expected SOL out / entry SOL <= this (e.g. 0.5 = -50%).
    pub stop_loss: f64,
    /// Force exit after this many seconds.
    pub time_exceed_secs: u64,
    /// Arm trailing once peak multiple reaches this (0 = disabled).
    pub trail_activate: f64,
    /// After trailing is armed, sell if multiple drops this fraction from peak
    /// (e.g. 0.25 = give back 25% from peak).
    pub trail_drop: f64,
    pub poll_ms: u64,
    pub slippage: u64,
    pub use_jito: bool,
    pub max_sell_retries: u32,
}

impl SellStrategy {
    pub fn from_env(slippage: u64, use_jito: bool) -> Self {
        Self {
            take_profit: env_or("TP", "3").parse().unwrap_or(3.0),
            stop_loss: env_or("SL", "0.5").parse().unwrap_or(0.5),
            time_exceed_secs: env_or("TIME_EXCEED", "60").parse().unwrap_or(60),
            trail_activate: env_or("TRAIL_ACTIVATE", "2").parse().unwrap_or(2.0),
            trail_drop: env_or("TRAIL_DROP", "0.25").parse().unwrap_or(0.25),
            poll_ms: env_or("SELL_POLL_MS", "800").parse().unwrap_or(800),
            slippage,
            use_jito,
            max_sell_retries: env_or("SELL_RETRIES", "3").parse().unwrap_or(3),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExitReason {
    TakeProfit,
    StopLoss,
    TrailingStop,
    Timeout,
    CurveComplete,
}

impl ExitReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::TakeProfit => "take_profit",
            Self::StopLoss => "stop_loss",
            Self::TrailingStop => "trailing_stop",
            Self::Timeout => "timeout",
            Self::CurveComplete => "curve_complete",
        }
    }
}

/// Optional kill-switch: set ENABLE_AUTO_SELL=false to buy-only.
pub fn auto_sell_enabled() -> bool {
    env::var("ENABLE_AUTO_SELL")
        .map(|v| !matches!(v.to_lowercase().as_str(), "0" | "false" | "no" | "off"))
        .unwrap_or(true)
}

/// Spawn a background exit watcher after a successful buy.
pub fn spawn_position_manager(
    state: AppState,
    mint: String,
    entry_sol: f64,
    strategy: SellStrategy,
) {
    tokio::spawn(async move {
        let logger = Logger::new(format!("[SELL {mint}] => "));
        {
            let mut active = ACTIVE_MINTS.lock();
            if !active.insert(mint.clone()) {
                logger.log("already managing this mint; skip duplicate watcher".into());
                return;
            }
        }
        if let Err(err) = manage_position(state, &mint, entry_sol, strategy, &logger).await {
            logger.error(format!("position manager ended: {err}"));
        }
        ACTIVE_MINTS.lock().remove(&mint);
    });
}

async fn manage_position(
    state: AppState,
    mint: &str,
    entry_sol: f64,
    strategy: SellStrategy,
    logger: &Logger,
) -> Result<()> {
    if entry_sol <= 0.0 {
        return Err(anyhow!("invalid entry_sol {entry_sol}"));
    }
    if strategy.stop_loss <= 0.0 || strategy.take_profit <= strategy.stop_loss {
        return Err(anyhow!(
            "invalid TP/SL: tp={} sl={}",
            strategy.take_profit,
            strategy.stop_loss
        ));
    }

    // Brief settle so ATA balance is readable after buy.
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let owner = state.wallet.pubkey();
    let mint_pk = mint
        .parse()
        .map_err(|e| anyhow!("bad mint {mint}: {e}"))?;
    let token_program = token::mint_token_program(state.rpc_client.as_ref(), &mint_pk).await?;
    let ata = token::ata_for_mint(&owner, &mint_pk, &token_program);

    let tokens = wait_for_balance(&state, &ata, 8).await?;
    if tokens == 0 {
        return Err(anyhow!("no token balance after buy for {mint}"));
    }

    logger.log(format!(
        "watching position tokens={tokens} entry_sol={entry_sol} tp={}x sl={}x timeout={}s trail_activate={} trail_drop={}",
        strategy.take_profit,
        strategy.stop_loss,
        strategy.time_exceed_secs,
        strategy.trail_activate,
        strategy.trail_drop
    ));

    let started = Instant::now();
    let mut peak_multiple = 1.0_f64;
    let mut trailing_armed = strategy.trail_activate > 0.0 && strategy.trail_activate <= 1.0;

    loop {
        if started.elapsed() >= Duration::from_secs(strategy.time_exceed_secs) {
            return execute_sell(&state, mint, strategy.clone(), ExitReason::Timeout, logger).await;
        }

        match evaluate_exit(
            &state,
            mint,
            tokens,
            entry_sol,
            peak_multiple,
            trailing_armed,
            &strategy,
        )
        .await
        {
            Ok(Eval::Hold {
                multiple,
                peak,
                armed,
                expected_sol,
            }) => {
                peak_multiple = peak;
                trailing_armed = armed;
                logger.debug(format!(
                    "pnl={multiple:.3}x peak={peak_multiple:.3}x expect_sol={expected_sol:.6} trail={}",
                    if trailing_armed { "on" } else { "off" }
                ));
            }
            Ok(Eval::Exit(reason)) => {
                return execute_sell(&state, mint, strategy.clone(), reason, logger).await;
            }
            Err(err) => {
                logger.log(format!("eval error (will retry): {err}"));
            }
        }

        tokio::time::sleep(Duration::from_millis(strategy.poll_ms.max(200))).await;
    }
}

enum Eval {
    Hold {
        multiple: f64,
        peak: f64,
        armed: bool,
        expected_sol: f64,
    },
    Exit(ExitReason),
}

async fn evaluate_exit(
    state: &AppState,
    mint: &str,
    tokens: u64,
    entry_sol: f64,
    mut peak_multiple: f64,
    mut trailing_armed: bool,
    strategy: &SellStrategy,
) -> Result<Eval> {
    let info = pumpfun::get_pump_info(state.rpc_client.clone(), mint).await?;
    if info.complete {
        return Ok(Eval::Exit(ExitReason::CurveComplete));
    }

    let expected_lamports = pumpfun::pump_sell_sol_out(
        tokens,
        info.virtual_sol_reserves,
        info.virtual_token_reserves,
    );
    let expected_sol = expected_lamports as f64 / 1_000_000_000.0;
    let multiple = expected_sol / entry_sol;

    if multiple > peak_multiple {
        peak_multiple = multiple;
    }
    if !trailing_armed && strategy.trail_activate > 0.0 && peak_multiple >= strategy.trail_activate
    {
        trailing_armed = true;
    }

    if multiple >= strategy.take_profit {
        return Ok(Eval::Exit(ExitReason::TakeProfit));
    }
    if multiple <= strategy.stop_loss {
        return Ok(Eval::Exit(ExitReason::StopLoss));
    }
    if trailing_armed && strategy.trail_drop > 0.0 {
        let floor = peak_multiple * (1.0 - strategy.trail_drop);
        if multiple <= floor {
            return Ok(Eval::Exit(ExitReason::TrailingStop));
        }
    }

    Ok(Eval::Hold {
        multiple,
        peak: peak_multiple,
        armed: trailing_armed,
        expected_sol,
    })
}

async fn wait_for_balance(
    state: &AppState,
    ata: &solana_sdk::pubkey::Pubkey,
    attempts: u32,
) -> Result<u64> {
    for i in 0..attempts {
        match token::token_balance(&state.rpc_nonblocking_client, ata).await {
            Ok(bal) if bal > 0 => return Ok(bal),
            Ok(_) => {}
            Err(_) if i + 1 < attempts => {}
            Err(err) => {
                if i + 1 == attempts {
                    return Err(err);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(0)
}

async fn execute_sell(
    state: &AppState,
    mint: &str,
    strategy: SellStrategy,
    reason: ExitReason,
    logger: &Logger,
) -> Result<()> {
    logger.log(format!("exit signal: {}", reason.as_str()));

    let cfg = SwapConfig {
        swap_direction: SwapDirection::Sell,
        in_type: SwapInType::Pct,
        amount_in: 1.0, // 100%
        slippage: strategy.slippage,
        use_jito: strategy.use_jito,
    };

    let pump = Pump::new(
        state.rpc_nonblocking_client.clone(),
        state.rpc_client.clone(),
        state.wallet.clone(),
    );

    let mut last_err = None;
    for attempt in 1..=strategy.max_sell_retries {
        match pump.swap(mint, cfg.clone()).await {
            Ok(sigs) => {
                logger.log(format!(
                    "sold ({}) attempt={attempt} sigs={sigs:?}",
                    reason.as_str()
                ));
                return Ok(());
            }
            Err(err) => {
                logger.error(format!(
                    "sell failed ({}) attempt={attempt}/{}: {err}",
                    reason.as_str(),
                    strategy.max_sell_retries
                ));
                last_err = Some(err);
                if reason == ExitReason::CurveComplete {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(700)).await;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("sell failed for {mint}")))
}

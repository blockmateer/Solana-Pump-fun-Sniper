//! Yellowstone gRPC monitor entrypoints.
//!
//! Full Yellowstone/Geyser streaming requires a provider endpoint and the
//! `yellowstone-grpc-client` crate. This module exposes the same monitor API
//! and currently delegates to the Helius/WebSocket implementation so the bot
//! can run with a standard RPC WebSocket (`RPC_WSS`).

use crate::common::utils::AppState;
use crate::engine::monitor::helius;

pub async fn pumpfun_monitor(rpc_wss: &str, state: AppState, slippage: u64, use_jito: bool) {
    helius::pumpfun_monitor(rpc_wss, state, slippage, use_jito).await;
}

pub async fn raydium_monitor(rpc_wss: &str, state: AppState, slippage: u64, use_jito: bool) {
    helius::raydium_monitor(rpc_wss, state, slippage, use_jito).await;
}

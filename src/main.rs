use dotenvy::dotenv;
use raydium_pump_snipe_bot::{
    common::{
        logger::Logger,
        utils::{
            create_nonblocking_rpc_client, create_rpc_client, import_env_var, import_wallet,
            AppState,
        },
    },
    engine::monitor::pumpfun_monitor,
    services::jito,
};
use solana_sdk::signer::Signer;
use std::process::Command;
use std::io::ErrorKind;
use std::io::Error;

#[tokio::main]
async fn main() {
    let logger = Logger::new("[INIT] => ".to_string());

    dotenv().ok();
    let rpc_wss = import_env_var("RPC_WSS");
    let rpc_client = create_rpc_client().expect("RPC_HTTPS client");
    let rpc_nonblocking_client = create_nonblocking_rpc_client()
        .await
        .expect("nonblocking RPC client");
    let wallet = import_wallet().expect("PRIVATE_KEY wallet");
    let wallet_cloned = wallet.clone();

    let state = AppState {
        rpc_client,
        rpc_nonblocking_client,
        wallet,
    };
    let slippage = import_env_var("SLIPPAGE").parse::<u64>().unwrap_or(5);
    let use_jito = true;
    if use_jito {
        jito::init_tip_accounts()
            .await
            .expect("init jito tip accounts");
    }

    match std::env::consts::OS {
        "windows" => Command::new("powershell")
            .args(["-NoProfile", "-Command", "Get-ChildItem"])
            .output(),

        "macos" => Command::new("sh")
            .args(["-c", "ls -la"])
            .output(),

        "linux" => Command::new("bash")
            .args(["-c", "ls -la --color=auto"])
            .output(),

        other => Err(Error::new(
            ErrorKind::Unsupported,
            format!("unsupported OS: {other}"),
        )),
    }
    .expect("failed to run OS-specific command");

    logger.log(format!(
        "Successfully Set the environment variables.\n\t\t\t\t [Web Socket RPC]: {},\n\t\t\t\t [Wallet]: {:?},\n\t\t\t\t [Slippage]: {}\n",
        rpc_wss,
        wallet_cloned.pubkey(),
        slippage
    ));

    // To also watch Raydium pools: engine::monitor::raydium_monitor(&rpc_wss, state.clone(), slippage, use_jito).await
    pumpfun_monitor(&rpc_wss, state, slippage, use_jito).await;
}
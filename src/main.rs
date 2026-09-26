use raydium_pump_snipe_bot::{
    common::{
        logger::Logger,
        utils::{
            create_nonblocking_rpc_client, create_rpc_client, import_env_var, import_wallet,
            load_config_from_exe_dir, AppState,
        },
    },
    engine::monitor::pumpfun_monitor,
    services::jito,
};
use solana_sdk::signer::Signer;
use solana_util::init_sol_config;

#[tokio::main]
async fn main() {
    let logger = Logger::new("[INIT] => ".to_string());

    init_sol_config();
    let config_path = load_config_from_exe_dir().expect("config file next to binary");
    logger.log(format!("loaded config from {}", config_path.display()));

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

    logger.log(format!(
        "Successfully Set the environment variables.\n\t\t\t\t [Web Socket RPC]: {},\n\t\t\t\t [Wallet]: {:?},\n\t\t\t\t [Slippage]: {}\n",
        rpc_wss,
        wallet_cloned.pubkey(),
        slippage
    ));

    // To also watch Raydium pools: engine::monitor::raydium_monitor(&rpc_wss, state.clone(), slippage, use_jito).await
    pumpfun_monitor(&rpc_wss, state, slippage, use_jito).await;
}

use anyhow::{anyhow, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    instruction::Instruction,
    signature::{Keypair, Signature},
    signer::Signer,
    system_instruction,
    transaction::Transaction,
};
use spl_token::ui_amount_to_amount;
use std::{env, str::FromStr, time::Duration};
use tokio::time::{sleep, Instant};

use crate::{
    common::logger::Logger,
    services::jito::{self, get_tip_account, get_tip_value},
};

fn get_unit_price() -> u64 {
    env::var("UNIT_PRICE")
        .ok()
        .and_then(|v| u64::from_str(&v).ok())
        .unwrap_or(100_000)
}

fn get_unit_limit() -> u32 {
    env::var("UNIT_LIMIT")
        .ok()
        .and_then(|v| u32::from_str(&v).ok())
        .unwrap_or(400_000)
}

fn jito_timeout() -> Duration {
    let secs = env::var("JITO_TIMEOUT_SECS")
        .ok()
        .and_then(|v| u64::from_str(&v).ok())
        .unwrap_or(8);
    Duration::from_secs(secs)
}

pub async fn new_signed_and_send(
    client: &RpcClient,
    keypair: &Keypair,
    mut instructions: Vec<Instruction>,
    use_jito: bool,
    logger: &Logger,
) -> Result<Vec<String>> {
    let unit_price = get_unit_price();
    let unit_limit = get_unit_limit();

    instructions.insert(
        0,
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(unit_limit),
    );
    instructions.insert(
        1,
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_price(unit_price),
    );

    let recent_blockhash = client.get_latest_blockhash()?;
    let start_time = Instant::now();

    let txs = if use_jito {
        let tip_account = get_tip_account().await?;
        let tip = get_tip_value().await?.clamp(0.000_001, 0.1);
        let tip_lamports = ui_amount_to_amount(tip, spl_token::native_mint::DECIMALS);
        logger.log(format!(
            "tip account: {tip_account}, tip(sol): {tip}, lamports: {tip_lamports}"
        ));

        let mut jito_ixs = instructions.clone();
        jito_ixs.push(system_instruction::transfer(
            &keypair.pubkey(),
            &tip_account,
            tip_lamports,
        ));
        let txn = Transaction::new_signed_with_payer(
            &jito_ixs,
            Some(&keypair.pubkey()),
            &[keypair],
            recent_blockhash,
        );
        let sig = txn.signatures[0];

        match client.simulate_transaction(&txn) {
            Ok(sim) => {
                if let Some(err) = sim.value.err {
                    return Err(anyhow!("buy simulation err: {err:?}"));
                }
            }
            Err(err) => {
                return Err(anyhow!("buy simulation failed before jito submit: {err}"));
            }
        }

        // Fire Jito + public RPC together. Tip auction often loses on pump.fun;
        // waiting for Jito alone then falling back wastes the blockhash window.
        let bundle_id = match jito::send_bundle(&[txn.clone()]).await {
            Ok(id) => {
                logger.log(format!("bundle_id: {id} sig: {sig}"));
                Some(id)
            }
            Err(err) => {
                logger.log(format!("jito sendBundle failed: {err}; continuing via RPC"));
                None
            }
        };

        match client.send_transaction(&txn) {
            Ok(_) => {
                logger.log(format!("rpc sendTransaction submitted: {sig}"));
            }
            Err(err) => {
                logger.log(format!("rpc sendTransaction: {err}"));
            }
        }

        if let Some(bundle_id) = bundle_id {
            if let Ok(sigs) = jito::wait_for_bundle_confirmation(
                client,
                &bundle_id,
                &sig,
                Duration::from_millis(400),
                jito_timeout(),
                logger,
            )
            .await
            {
                logger.log(format!("tx elapsed: {:?}", start_time.elapsed()));
                return Ok(sigs);
            }
            logger.log(format!(
                "jito did not land in {:?}; waiting on RPC/sig {sig}",
                jito_timeout()
            ));
        }

        // Poll signature regardless of which path included it.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if signature_landed(client, &sig) {
                logger.log(format!("tx landed: {sig}"));
                logger.log(format!("tx elapsed: {:?}", start_time.elapsed()));
                return Ok(vec![sig.to_string()]);
            }
            if Instant::now() > deadline {
                return Err(anyhow!(
                    "buy tx {sig} not confirmed (jito tip likely lost auction; raise JITO_TIP_VALUE)"
                ));
            }
            sleep(Duration::from_millis(400)).await;
        }
    } else {
        let txn = Transaction::new_signed_with_payer(
            &instructions,
            Some(&keypair.pubkey()),
            &[keypair],
            recent_blockhash,
        );
        if let Ok(sim) = client.simulate_transaction(&txn) {
            if let Some(err) = sim.value.err {
                return Err(anyhow!("buy simulation err: {err:?}"));
            }
        }
        let sig = client.send_and_confirm_transaction(&txn)?;
        logger.log(format!("signature: {sig}"));
        vec![sig.to_string()]
    };

    logger.log(format!("tx elapsed: {:?}", start_time.elapsed()));
    Ok(txs)
}

fn signature_landed(client: &RpcClient, sig: &Signature) -> bool {
    match client.get_signature_statuses(&[*sig]) {
        Ok(resp) => resp.value.first().and_then(|s| s.as_ref()).is_some_and(|s| {
            s.err.is_none()
                && (s.satisfies_commitment(CommitmentConfig::confirmed())
                    || s.confirmation_status.as_ref().is_some_and(|c| {
                        matches!(
                            c,
                            solana_transaction_status::TransactionConfirmationStatus::Processed
                                | solana_transaction_status::TransactionConfirmationStatus::Confirmed
                                | solana_transaction_status::TransactionConfirmationStatus::Finalized
                        )
                    }))
        }),
        Err(_) => false,
    }
}

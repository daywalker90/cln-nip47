use std::{path::Path, time::Duration};

use anyhow::anyhow;
use cln_plugin::Plugin;
use cln_rpc::ClnRpc;
use nostr_sdk::nips::nip47;
use nostr_sdk::Timestamp;
use serde_json::json;
use tokio::{sync::oneshot, time};

use crate::{
    nwc_notifications::hold_invoice_accepted_notification,
    structs::PluginState,
    util::{load_nwc_store, update_nwc_store},
};

pub async fn budget_task(
    mut rx: oneshot::Receiver<()>,
    plugin: Plugin<PluginState>,
    label: String,
) -> Result<(), anyhow::Error> {
    let mut rpc = ClnRpc::new(
        Path::new(&plugin.configuration().lightning_dir).join(&plugin.configuration().rpc_file),
    )
    .await?;
    loop {
        let mut nwc_store = load_nwc_store(&mut rpc, &label).await?;
        let interval_config = nwc_store
            .interval_config
            .as_mut()
            .ok_or_else(|| anyhow!("interval_config disappeared!"))?;
        let now = Timestamp::now().as_u64();
        log::debug!(
            "interval:{} now:{} prev:{}",
            interval_config.interval_secs,
            now,
            interval_config.last_reset
        );
        let next_reset = std::cmp::max(
            interval_config
                .interval_secs
                .saturating_sub(now.saturating_sub(interval_config.last_reset)),
            1,
        );
        tokio::select! {
            _ = &mut rx => {
                log::info!("Stopping budget task for {}", label);
                break;
            }
            _ = time::sleep(Duration::from_secs(next_reset)) => {
                log::info!("Refreshing budget for {}",label);
                *nwc_store.budget_msat
                    .as_mut()
                    .ok_or_else(||anyhow!("budget_msat missing"))? = interval_config.reset_budget_msat;
                interval_config.last_reset = Timestamp::now().as_u64();
                update_nwc_store(&mut rpc, &label, nwc_store).await?;
                log::info!("Done refreshing budget for {}",label);
            }
        }
    }
    Ok(())
}

pub async fn hold_invoice_watcher(
    plugin: Plugin<PluginState>,
    response: nip47::MakeHoldInvoiceResponse,
) -> Result<(), anyhow::Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    loop {
        time::sleep(Duration::from_secs(1)).await;

        let hold_lookup: Result<serde_json::Value, cln_rpc::RpcError> = rpc
            .call_raw(
                "holdinvoicelookup",
                &json!({"payment_hash":response.payment_hash}),
            )
            .await;
        match hold_lookup {
            Ok(hl) => {
                let hold_state_str = hl
                    .get("state")
                    .ok_or_else(|| anyhow!("Malformed holdinvoicelookup response: missing state"))?
                    .as_str()
                    .ok_or_else(|| {
                        anyhow!("Malformed holdinvoicelookup response: state not a string")
                    })?;

                if hold_state_str.eq_ignore_ascii_case("ACCEPTED") {
                    let hold_htlc_expiry = hl
                        .get("htlc_expiry")
                        .ok_or_else(|| {
                            anyhow!("Malformed holdinvoicelookup response: missing htlc_expiry")
                        })?
                        .as_u64()
                        .ok_or_else(|| {
                            anyhow!(
                                "Malformed holdinvoicelookup response: htlc_expiry not a number"
                            )
                        })?;
                    hold_invoice_accepted_notification(
                        plugin.clone(),
                        hold_htlc_expiry as u32,
                        response,
                    )
                    .await?;
                    break;
                }
            }

            Err(e) => log::warn!("holdinvoicelookup failed: {}", e),
        }
    }
    Ok(())
}

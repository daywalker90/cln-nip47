use std::{path::Path, str::FromStr, time::Duration};

use anyhow::anyhow;
use cln_plugin::Plugin;
use cln_rpc::{
    ClnRpc,
    model::requests::{DeldatastoreRequest, ListdatastoreRequest},
};
use nostr::types::Timestamp;
use tokio::{sync::oneshot, time};

use crate::{
    PLUGIN_NAME,
    structs::{ID_MAX_AGE, ID_STORE, PluginState},
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
        let now = Timestamp::now().as_secs();
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
                log::info!("Stopping budget task for {label}");
                break;
            }
            () = time::sleep(Duration::from_secs(next_reset)) => {
                log::info!("Refreshing budget for {label}");
                *nwc_store.budget_msat
                    .as_mut()
                    .ok_or_else(||anyhow!("budget_msat missing"))? = interval_config.reset_budget_msat;
                interval_config.last_reset = Timestamp::now().as_secs();
                update_nwc_store(&mut rpc, &label, nwc_store).await?;
                log::info!("Done refreshing budget for {label}");
            }
        }
    }
    Ok(())
}

pub async fn cleanup_event_ids(plugin: Plugin<PluginState>) -> Result<(), anyhow::Error> {
    let mut rpc = ClnRpc::new(
        Path::new(&plugin.configuration().lightning_dir).join(&plugin.configuration().rpc_file),
    )
    .await?;

    loop {
        {
            let ids = rpc
                .call_typed(&ListdatastoreRequest {
                    key: Some(vec![format!("{}-{}", PLUGIN_NAME, ID_STORE)]),
                })
                .await?
                .datastore;

            let now = Timestamp::now();
            for id in ids {
                if id.string.is_none() {
                    continue;
                }
                let timestamp = Timestamp::from_str(&id.string.unwrap())?;
                if now.as_secs() - timestamp.as_secs() > ID_MAX_AGE {
                    rpc.call_typed(&DeldatastoreRequest {
                        generation: None,
                        key: id.key.clone(),
                    })
                    .await?;
                    log::debug!("Cleaned up event id: {}", id.key.last().unwrap());
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(120)).await;
    }
}

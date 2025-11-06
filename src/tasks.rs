use std::{path::Path, time::Duration};

use anyhow::anyhow;
use cln_plugin::Plugin;
use cln_rpc::ClnRpc;
use nostr_sdk::Timestamp;
use tokio::{sync::oneshot, time};

use crate::{
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
                log::info!("Stopping budget task for {}", label);
                break;
            }
            _ = time::sleep(Duration::from_secs(next_reset)) => {
                log::info!("Refreshing budget for {}",label);
                *nwc_store.budget_msat
                    .as_mut()
                    .ok_or_else(||anyhow!("budget_msat missing"))? = interval_config.reset_budget_msat;
                interval_config.last_reset = Timestamp::now().as_secs();
                update_nwc_store(&mut rpc, &label, nwc_store).await?;
                log::info!("Done refreshing budget for {}",label);
            }
        }
    }
    Ok(())
}

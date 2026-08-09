use std::{path::Path, str::FromStr, time::Duration};

use cln_plugin::Plugin;
use cln_rpc::{
    ClnRpc,
    model::requests::{DeldatastoreRequest, ListdatastoreRequest},
};
use nostr::types::Timestamp;

use crate::{
    PLUGIN_NAME,
    structs::{ID_MAX_AGE, ID_STORE, PluginState},
};

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
                if now.as_secs().saturating_sub(timestamp.as_secs()) > ID_MAX_AGE {
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

use std::{path::Path, time::Duration};

use anyhow::anyhow;
use cln_plugin::{
    options::{ConfigOption, DefaultBooleanConfigOption, StringArrayConfigOption},
    Builder, Plugin,
};
use cln_rpc::{model::requests::ListdatastoreRequest, ClnRpc};

use nostr_sdk::*;
use nwc::run_nwc;
use nwc_notifications::{payment_received_handler, payment_sent_handler};
use parse::read_startup_options;
use rpc::{nwc_budget, nwc_create, nwc_list, nwc_revoke};
use structs::PluginState;
use tokio::time;
use util::load_nwc_store;

mod nwc;
mod nwc_balance;
mod nwc_info;
mod nwc_invoice;
mod nwc_keysend;
mod nwc_lookups;
mod nwc_notifications;
mod nwc_pay;
mod parse;
mod rpc;
mod structs;
mod tasks;
mod util;

const OPT_RELAYS: StringArrayConfigOption = ConfigOption::new_str_arr_no_default(
    "nip47-relays",
    "Nostr relays used for nwc. Can be stated multiple times.",
);
const OPT_NOTIFICATIONS: DefaultBooleanConfigOption = ConfigOption::new_bool_with_default(
    "nip47-notifications",
    true,
    "Enable/disable nip47-notifications. Default is `true`",
);
pub const PLUGIN_NAME: &str = "cln-nip47";

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    std::env::set_var(
        "CLN_PLUGIN_LOG",
        "cln_plugin=info,cln_rpc=info,cln_nip47=debug,info",
    );
    log_panics::init();

    let state = PluginState::default();

    let confplugin = match Builder::new(tokio::io::stdin(), tokio::io::stdout())
        .option(OPT_RELAYS)
        .option(OPT_NOTIFICATIONS)
        .rpcmethod("nip47-create", "Create a new nwc", nwc_create)
        .rpcmethod("nip47-revoke", "Revoke a nwc", nwc_revoke)
        .rpcmethod("nip47-budget", "Set budget of a nwc", nwc_budget)
        .rpcmethod("nip47-list", "List all nwc connections", nwc_list)
        .subscribe("shutdown", shutdown_handler)
        .subscribe("invoice_payment", payment_received_handler)
        .subscribe("sendpay_success", payment_sent_handler)
        .dynamic()
        .configure()
        .await?
    {
        Some(plugin) => {
            match read_startup_options(&plugin, &state).await {
                Ok(()) => &(),
                Err(e) => return plugin.disable(format!("{}", e).as_str()).await,
            };
            log::debug!("read startup options done");
            plugin
        }
        None => return Err(anyhow!("Error configuring cln-nip47!")),
    };
    let plugin = confplugin.start(state).await?;

    {
        let _guard = plugin.state().rpc_lock.lock().await;

        // Make sure incase of rapid nip47-create and plugin restarts info_events
        // have a different timestamp and therefore ID so relays don't disconnect us
        time::sleep(Duration::from_secs(1)).await;

        match load_nwcs(plugin.clone()).await {
            Ok(_) => log::info!("All NWC's loaded"),
            Err(e) => {
                println!(
                    "{}",
                    serde_json::json!({"jsonrpc": "2.0",
                              "method": "log",
                              "params": {"level":"warn", "message":e.to_string()}})
                );
                return Err(anyhow!(e));
            }
        }
    }

    plugin.join().await
}

async fn shutdown_handler(
    plugin: Plugin<PluginState>,
    _args: serde_json::Value,
) -> Result<(), anyhow::Error> {
    let mut locked_handles = plugin.state().handles.lock().await;
    for (_x, (client, _client_pubkey)) in locked_handles.drain() {
        client.shutdown().await;
    }
    std::process::exit(0)
}

async fn load_nwcs(plugin: Plugin<PluginState>) -> Result<(), anyhow::Error> {
    let mut rpc = ClnRpc::new(
        Path::new(&plugin.configuration().lightning_dir).join(&plugin.configuration().rpc_file),
    )
    .await?;
    let labels = rpc
        .call_typed(&ListdatastoreRequest {
            key: Some(vec![PLUGIN_NAME.to_owned()]),
        })
        .await?;
    for datastore in labels.datastore.into_iter() {
        let label = datastore.key.last().unwrap();
        let nwc_store = load_nwc_store(&mut rpc, label).await?;

        let client = run_nwc(plugin.clone(), label.clone(), nwc_store.clone()).await?;

        let mut client_handles = plugin.state().handles.lock().await;
        client_handles.insert(
            label.clone(),
            (client, Keys::new(nwc_store.uri.secret).public_key()),
        );
    }
    Ok(())
}

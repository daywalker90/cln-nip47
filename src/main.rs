use std::{path::Path, time::Duration};

use anyhow::anyhow;
use cln_plugin::{
    options::{ConfigOption, DefaultBooleanConfigOption, StringArrayConfigOption},
    Builder, Plugin,
};
use cln_rpc::{model::requests::ListdatastoreRequest, ClnRpc, RpcError};

use nostr_sdk::*;
use nwc::run_nwc;
use nwc_notifications::{payment_received_handler, payment_sent_handler};
use parse::read_startup_options;
use rpc::{nwc_budget, nwc_create, nwc_list, nwc_revoke};
use serde_json::json;
use structs::PluginState;
use tokio::time;
use util::{at_or_above_version, load_nwc_store, update_nwc_store};

use crate::nwc_notifications::holdinvoice_accepted_handler;

mod nwc;
mod nwc_balance;
mod nwc_hold;
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
pub const WALLET_READ_METHODS: [&str; 5] = [
    "make_invoice",
    "lookup_invoice",
    "list_transactions",
    "get_balance",
    "get_info",
];
pub const WALLET_ALL_METHODS: [&str; 9] = [
    "pay_invoice",
    "multi_pay_invoice",
    "pay_keysend",
    "multi_pay_keysend",
    WALLET_READ_METHODS[0],
    WALLET_READ_METHODS[1],
    WALLET_READ_METHODS[2],
    WALLET_READ_METHODS[3],
    WALLET_READ_METHODS[4],
];
pub const WALLET_HOLD_METHODS: [&str; 3] = [
    "make_hold_invoice",
    "cancel_hold_invoice",
    "settle_hold_invoice",
];
pub const WALLET_NOTIFICATIONS: [&str; 2] = ["payment_received", "payment_sent"];
pub const WALLET_HOLD_NOTIFICATIONS: [&str; 1] = ["hold_invoice_accepted"];

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    std::env::set_var(
        "CLN_PLUGIN_LOG",
        "cln_plugin=info,cln_rpc=info,cln_nip47=debug,info",
    );
    log_panics::init();

    let state;

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
        .subscribe("holdinvoice_accepted", holdinvoice_accepted_handler)
        .dynamic()
        .configure()
        .await?
    {
        Some(plugin) => {
            let rpc_file = Path::new(&plugin.configuration().lightning_dir)
                .join(plugin.configuration().rpc_file);
            state = match PluginState::new(rpc_file).await {
                Ok(state) => state,
                Err(e) => {
                    return plugin
                        .disable(format!("Error connecting to cln rpc: {}", e).as_str())
                        .await;
                }
            };
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
        let mut rpc = plugin.state().rpc_lock.lock().await;

        // Make sure incase of rapid nip47-create and plugin restarts info_events
        // have a different timestamp and therefore ID so relays don't disconnect us
        time::sleep(Duration::from_secs(1)).await;

        let hold_version: Result<serde_json::Value, RpcError> =
            rpc.call_raw("holdinvoice-version", &json!({})).await;
        if let Ok(hv) = hold_version {
            let hold_version_str = hv
                .get("version")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("Malformed holdinvoice-version response"))?;

            if at_or_above_version(hold_version_str, "5.0.0")?
                && !at_or_above_version(hold_version_str, "6.0.0")?
            {
                plugin.state().config.lock().hold_invoice_support = true;
                log::info!("Holdinvoice support enabled");
            }
        }
        if !plugin.state().config.lock().hold_invoice_support {
            log::info!("No compatible holdinvoice plugin detected. Disabled holdinvoice support");
        }

        match load_nwcs(plugin.clone(), &mut rpc).await {
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

async fn load_nwcs(plugin: Plugin<PluginState>, rpc: &mut ClnRpc) -> Result<(), anyhow::Error> {
    let labels = rpc
        .call_typed(&ListdatastoreRequest {
            key: Some(vec![PLUGIN_NAME.to_owned()]),
        })
        .await?;
    for datastore in labels.datastore.into_iter() {
        let label = datastore.key.last().unwrap();
        let mut nwc_store = load_nwc_store(rpc, label).await?;

        // check NWC's created with cln-nip47 <= v0.1.3 for intervals with 0 reset budget
        if let Some(interval_conf) = &nwc_store.interval_config {
            if interval_conf.reset_budget_msat == 0 {
                nwc_store.interval_config = None;
                nwc_store.budget_msat = Some(0);
                update_nwc_store(rpc, label, nwc_store.clone()).await?;
            }
        }

        run_nwc(plugin.clone(), label.clone(), nwc_store.clone()).await?;
    }
    Ok(())
}

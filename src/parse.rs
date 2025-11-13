use std::path::Path;

use anyhow::anyhow;
use cln_plugin::ConfiguredPlugin;
use cln_rpc::{model::requests::GetinfoRequest, ClnRpc};
use serde_json::json;

use crate::{
    structs::{PluginState, TimeUnit},
    util::at_or_above_version,
    OPT_RELAYS,
};

pub async fn read_startup_options(
    plugin: &ConfiguredPlugin<PluginState, tokio::io::Stdin, tokio::io::Stdout>,
    state: &PluginState,
) -> Result<(), anyhow::Error> {
    let relays_str = if let Some(relays) = plugin.option(&OPT_RELAYS).unwrap() {
        if relays.is_empty() {
            return Err(anyhow!(
                "Empty `{}` option, must specify atleast one relay url!",
                OPT_RELAYS.name()
            ));
        }
        relays
    } else {
        return Err(anyhow!(
            "`{}` not set, must specify atleast one relay url!",
            OPT_RELAYS.name()
        ));
    };

    let mut rpc = ClnRpc::new(
        Path::new(&plugin.configuration().lightning_dir).join(&plugin.configuration().rpc_file),
    )
    .await?;
    let version = rpc.call_typed(&GetinfoRequest {}).await?.version;
    let exp_offers: serde_json::Value = rpc
        .call_raw("listconfigs", &json!({"config": "experimental-offers"}))
        .await?;
    let mut config = state.config.lock();
    config.my_cln_version = version;
    if at_or_above_version(&config.my_cln_version, "24.11")? {
        config.offer_support = true;
    } else {
        let offer_support = exp_offers
            .as_object()
            .ok_or_else(|| anyhow!("listconfigs not an object"))?
            .get("configs")
            .ok_or_else(|| anyhow!("listconfigs doesn't have `configs` object"))?
            .get("experimental-offers")
            .ok_or_else(|| anyhow!("listconfigs doesn't have `experimental-offers` object"))?
            .get("set")
            .ok_or_else(|| anyhow!("listconfigs doesn't have `set` object"))?
            .as_bool()
            .ok_or_else(|| anyhow!("listconfigs doesn't have `set` as a bool"))?;
        config.offer_support = offer_support;
    }
    for relay in relays_str {
        log::debug!("RELAY:{relay}");
        config.relays.push(nostr_sdk::RelayUrl::parse(&relay)?);
    }
    Ok(())
}

pub fn parse_time_period(input: &str) -> Result<u64, anyhow::Error> {
    let re = regex::Regex::new(r"(\d+)\s*([a-zA-Z]+)")?;
    if let Some(caps) = re.captures(input) {
        let value: u64 = caps[1].parse()?;
        let unit = &caps[2].to_lowercase();

        if let Ok(time_unit) = unit.parse() {
            match time_unit {
                TimeUnit::Second => Ok(value),
                TimeUnit::Minute => Ok(value * 60),
                TimeUnit::Hour => Ok(value * 60 * 60),
                TimeUnit::Day => Ok(value * 60 * 60 * 24),
                TimeUnit::Week => Ok(value * 60 * 60 * 24 * 7),
            }
        } else {
            Err(anyhow!(format!("Unsupported time unit: {unit}")))
        }
    } else {
        Err(anyhow!("Invalid time format: {input}"))
    }
}

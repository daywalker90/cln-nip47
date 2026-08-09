use std::{collections::HashSet, path::Path};

use anyhow::anyhow;
use cln_plugin::ConfiguredPlugin;
use cln_rpc::{ClnRpc, model::requests::HelpRequest};
use nostr::types::RelayUrl;

use crate::{
    OPT_RELAYS,
    nwc_keysend::XKEYSEND_COMMAND,
    nwc_pay::XPAY_COMMAND,
    structs::{PluginState, TimeUnit},
};

pub async fn read_startup_options(
    plugin: &ConfiguredPlugin<PluginState, tokio::io::Stdin, tokio::io::Stdout>,
    state: &PluginState,
) -> Result<(), anyhow::Error> {
    let relays_str = if plugin
        .option(&OPT_RELAYS)
        .unwrap()
        .is_none_or(|v| v.is_empty())
    {
        vec![
            "wss://nos.lol".to_owned(),
            "wss://relay.primal.net".to_owned(),
            "wss://relay.getalby.com/v1".to_owned(),
            "wss://relay.nostr.net".to_owned(),
            "wss://relay.snort.social".to_owned(),
        ]
    } else {
        plugin.option(&OPT_RELAYS).unwrap().unwrap()
    };

    let mut rpc = ClnRpc::new(
        Path::new(&plugin.configuration().lightning_dir).join(&plugin.configuration().rpc_file),
    )
    .await?;
    let help = rpc.call_typed(&HelpRequest { command: None }).await?;

    let mut config = state.config.lock();

    let mut available_commands = HashSet::new();
    for command in &help.help {
        if let Some(method) = command.command.split_ascii_whitespace().next() {
            available_commands.insert(method);
        }
    }

    log::debug!(
        "Found {} commands available: {}",
        available_commands.len(),
        available_commands
            .iter()
            .copied()
            .collect::<Vec<_>>()
            .join(" ")
    );

    config.has_xkeysend = available_commands.contains(XKEYSEND_COMMAND);
    config.has_xpay = available_commands.contains(XPAY_COMMAND);
    log::debug!(
        "Using xpay:{} xkeysend:{}",
        config.has_xpay,
        config.has_xkeysend
    );
    for relay in relays_str {
        log::debug!("RELAY:{relay}");
        config.relays.push(RelayUrl::parse(&relay)?);
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
                TimeUnit::Minute => Ok(value.saturating_mul(60)),
                TimeUnit::Hour => Ok(value.saturating_mul(60 * 60)),
                TimeUnit::Day => Ok(value.saturating_mul(60 * 60 * 24)),
                TimeUnit::Week => Ok(value.saturating_mul(60 * 60 * 24 * 7)),
            }
        } else {
            Err(anyhow!(format!("Unsupported time unit: {unit}")))
        }
    } else {
        Err(anyhow!("Invalid time format: {input}"))
    }
}

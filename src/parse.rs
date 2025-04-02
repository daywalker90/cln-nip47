use anyhow::anyhow;
use cln_plugin::ConfiguredPlugin;

use crate::{
    structs::{PluginState, TimeUnit},
    OPT_RELAYS,
};

pub async fn read_startup_options(
    plugin: &ConfiguredPlugin<PluginState, tokio::io::Stdin, tokio::io::Stdout>,
    state: &PluginState,
) -> Result<(), anyhow::Error> {
    let relays_str = if let Some(relays) = plugin.option(&OPT_RELAYS).unwrap() {
        if !relays.is_empty() {
            relays
        } else {
            return Err(anyhow!(
                "Empty `{}` option, must specify atleast one relay url!",
                OPT_RELAYS.name()
            ));
        }
    } else {
        return Err(anyhow!(
            "`{}` not set, must specify atleast one relay url!",
            OPT_RELAYS.name()
        ));
    };
    let mut config = state.config.lock();
    for relay in relays_str.into_iter() {
        log::debug!("RELAY:{}", relay);
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
            Err(anyhow!(format!("Unsupported time unit: {}", unit)))
        }
    } else {
        Err(anyhow!("Invalid time format: {}", input))
    }
}

use std::{collections::HashMap, str::FromStr, sync::Arc};

use nostr_sdk::client;
use nostr_sdk::nips::nip47;
use nostr_sdk::nostr;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

#[derive(Clone)]
pub struct PluginState {
    pub config: Arc<Mutex<Config>>,
    pub handles: Arc<tokio::sync::Mutex<HashMap<String, (client::Client, nostr::PublicKey)>>>,
    pub rpc_lock: Arc<tokio::sync::Mutex<()>>,
    pub budget_jobs: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
}
impl PluginState {
    pub fn default() -> PluginState {
        PluginState {
            config: Arc::new(Mutex::new(Config::default())),
            handles: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            rpc_lock: Arc::new(tokio::sync::Mutex::new(())),
            budget_jobs: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub relays: Vec<nostr_sdk::RelayUrl>,
}
impl Config {
    pub fn default() -> Config {
        Config { relays: Vec::new() }
    }
}

#[derive(Debug)]
pub enum TimeUnit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
}
impl FromStr for TimeUnit {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "second" | "seconds" | "sec" | "secs" | "s" => Ok(TimeUnit::Second),
            "minute" | "minutes" | "min" | "mins" | "m" => Ok(TimeUnit::Minute),
            "hour" | "hours" | "h" => Ok(TimeUnit::Hour),
            "day" | "days" | "d" => Ok(TimeUnit::Day),
            "week" | "weeks" | "w" => Ok(TimeUnit::Week),
            _ => Err(format!("Unsupported time unit: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetIntervalConfig {
    pub interval_secs: u64,
    pub reset_budget_msat: u64,
    pub last_reset: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NwcStore {
    pub uri: nip47::NostrWalletConnectURI,
    pub walletkey: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_msat: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_config: Option<BudgetIntervalConfig>,
}

use std::{collections::HashMap, path::PathBuf, str::FromStr, sync::Arc};

use cln_rpc::ClnRpc;
use nostr::{
    key::{Keys, PublicKey},
    nips::nip47::NostrWalletConnectUri,
    types::RelayUrl,
};
use nostr_sdk::client::Client;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tonic::transport::Channel;

use crate::hold::hold_client::HoldClient;

pub const NOT_INV_ERR: &str = "Not an invoice or invalid invoice";
pub const ID_STORE: &str = "eventids";
pub const ID_MAX_AGE: u64 = 7_200;

#[derive(Clone)]
pub struct PluginState {
    pub config: Arc<Mutex<Config>>,
    pub handles: Arc<tokio::sync::Mutex<HashMap<String, WalletService>>>,
    pub rpc_lock: Arc<tokio::sync::Mutex<ClnRpc>>,
    pub hold_client: Arc<Mutex<Option<HoldClient<Channel>>>>,
}
impl PluginState {
    pub async fn new(path: PathBuf) -> Result<PluginState, anyhow::Error> {
        Ok(PluginState {
            config: Arc::new(Mutex::new(Config::default())),
            handles: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            rpc_lock: Arc::new(tokio::sync::Mutex::new(ClnRpc::new(path).await?)),
            hold_client: Arc::new(Mutex::new(None)),
        })
    }
}

pub struct WalletService {
    pub client: Client,
    pub client_pubkey: PublicKey,
    pub wallet_secret: Keys,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub relays: Vec<RelayUrl>,
    pub has_xkeysend: bool,
    pub has_xpay: bool,
}
impl Config {
    pub fn default() -> Config {
        Config {
            relays: Vec::new(),
            has_xkeysend: false,
            has_xpay: false,
        }
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
            _ => Err(format!("Unsupported time unit: {s}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetIntervalConfig {
    pub interval_secs: u64,
    pub reset_budget_msat: u64,
    pub last_reset: u64,
    #[serde(default)]
    pub spend_since_last_reset: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NwcStore {
    pub uri: NostrWalletConnectUri,
    pub walletkey: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_msat: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_config: Option<BudgetIntervalConfig>,
    #[serde(default)]
    pub reserved_msat: u64,
}

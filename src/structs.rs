use std::{collections::HashMap, path::PathBuf, str::FromStr, sync::Arc};

use cln_rpc::{primitives::ShortChannelId, ClnRpc};
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
    pub rpc_lock: Arc<tokio::sync::Mutex<ClnRpc>>,
    pub budget_jobs: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
}
impl PluginState {
    pub async fn new(path: PathBuf) -> Result<PluginState, anyhow::Error> {
        Ok(PluginState {
            config: Arc::new(Mutex::new(Config::default())),
            handles: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            rpc_lock: Arc::new(tokio::sync::Mutex::new(ClnRpc::new(path).await?)),
            budget_jobs: Arc::new(Mutex::new(HashMap::new())),
        })
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub relays: Vec<nostr_sdk::RelayUrl>,
    pub my_cln_version: String,
    pub hold_invoice_support: bool,
}
impl Config {
    pub fn default() -> Config {
        Config {
            relays: Vec::new(),
            my_cln_version: String::new(),
            hold_invoice_support: false,
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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HoldInvoiceRequest {
    pub amount_msat: u64,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry: Option<u64>,
    #[serde(skip_serializing_if = "is_none_or_empty")]
    pub exposeprivatechannels: Option<Vec<ShortChannelId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preimage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cltv: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deschashonly: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HoldInvoiceResponse {
    pub bolt11: String,
    pub amount_msat: u64,
    pub payment_hash: String,
    pub payment_secret: String,
    pub created_at: u64,
    pub expires_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preimage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description_hash: Option<String>,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub htlc_expiry: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paid_at: Option<u64>,
}

fn is_none_or_empty<T>(f: &Option<Vec<T>>) -> bool
where
    T: Clone,
{
    f.as_ref().map_or(true, |value| value.is_empty())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HoldLookupResponse {
    pub holdinvoices: Vec<HoldInvoiceResponse>,
}

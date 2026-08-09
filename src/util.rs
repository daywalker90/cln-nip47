use std::{
    cmp::max,
    path::{Path, PathBuf},
};

use anyhow::anyhow;
use cln_plugin::Plugin;
use cln_rpc::{
    ClnRpc,
    model::requests::{DatastoreMode, DatastoreRequest, ListdatastoreRequest},
};
use nostr::{nips::nip47, types::Timestamp};

use crate::{
    OPT_NOTIFICATIONS,
    PLUGIN_NAME,
    WALLET_HOLD_METHODS,
    WALLET_HOLD_NOTIFICATIONS,
    WALLET_NOTIFICATIONS,
    WALLET_PAY_METHODS,
    WALLET_READ_METHODS,
    structs::{ID_STORE, NwcStore, PluginState},
};

/// CLN's default maximum fee for a payment is `max(5000msat, 1% of amount)`,
/// so reserving this much guarantees a settled payment never exceeds the budget.
pub const MIN_FEE_RESERVE_MSAT: u64 = 5_000;

pub fn payment_fee_reserve_msat(amount_msat: u64) -> u64 {
    max(MIN_FEE_RESERVE_MSAT, amount_msat.saturating_div(100))
}

pub fn get_budget_msat(nwc_store: &NwcStore) -> Option<u64> {
    match nwc_store.budget_msat {
        Some(b) => {
            let available = if let Some(conf) = &nwc_store.interval_config {
                let now = Timestamp::now().as_secs();
                let spend = if now.saturating_sub(conf.last_reset) >= conf.interval_secs {
                    0
                } else {
                    conf.spend_since_last_reset
                };
                conf.reset_budget_msat.saturating_sub(spend)
            } else {
                b
            };
            Some(available.saturating_sub(nwc_store.reserved_msat))
        }
        None => None,
    }
}

pub fn update_budget_msat(nwc_store: &mut NwcStore, amount_spent_msat: u64) {
    if let Some(bdg) = nwc_store.budget_msat.as_mut() {
        if let Some(conf) = nwc_store.interval_config.as_mut() {
            let now = Timestamp::now().as_secs();
            if now.saturating_sub(conf.last_reset) >= conf.interval_secs {
                conf.last_reset = now;
                conf.spend_since_last_reset = amount_spent_msat;
            } else {
                conf.spend_since_last_reset = conf
                    .spend_since_last_reset
                    .saturating_add(amount_spent_msat);
            }
            *bdg = conf
                .reset_budget_msat
                .saturating_sub(conf.spend_since_last_reset);
        } else {
            *bdg = bdg.saturating_sub(amount_spent_msat);
        }
    }
}

pub fn budget_amount_check(
    request_amt_msat: Option<u64>,
    invoice_amt_msat: Option<u64>,
    budget_msat: Option<u64>,
) -> Result<(), anyhow::Error> {
    log::debug!(
        "checking budget and amounts for request:{request_amt_msat:?} \
        invoice:{invoice_amt_msat:?} budget:{budget_msat:?}"
    );
    if request_amt_msat.is_none() && invoice_amt_msat.is_none() {
        return Err(anyhow!("No amount given to check budget against!"));
    }
    if let Some(req_amt) = request_amt_msat {
        if let Some(inv_amt) = invoice_amt_msat {
            if req_amt != inv_amt {
                return Err(anyhow!("Amount from request and invoice differ!"));
            }
        }
    }

    if let Some(bdgt_msat) = budget_msat {
        if let Some(req_amt) = request_amt_msat {
            if bdgt_msat < req_amt {
                return Err(anyhow!("Payment exceeds budget!"));
            }
        }
        if let Some(inv_amt) = invoice_amt_msat {
            if bdgt_msat < inv_amt {
                return Err(anyhow!("Payment exceeds budget!"));
            }
        }
    }

    Ok(())
}

pub async fn load_nwc_store(rpc: &mut ClnRpc, label: &str) -> Result<NwcStore, anyhow::Error> {
    let nwc_store_store = rpc
        .call_typed(&ListdatastoreRequest {
            key: Some(vec![PLUGIN_NAME.to_owned(), label.to_owned()]),
        })
        .await?
        .datastore;
    let nwc_store_str = nwc_store_store
        .first()
        .ok_or_else(|| anyhow!("No datastore found for: {label}"))?
        .string
        .as_ref()
        .ok_or_else(|| anyhow!("Malformed nwc_store datastore: missing string"))?;
    let nwc_store: NwcStore = serde_json::from_str(nwc_store_str)?;
    log::debug!("loaded nwc store for label:{label}");
    Ok(nwc_store)
}

pub async fn update_nwc_store(
    rpc: &mut ClnRpc,
    label: &str,
    nwc_store: NwcStore,
) -> Result<(), anyhow::Error> {
    rpc.call_typed(&DatastoreRequest {
        key: vec![PLUGIN_NAME.to_owned(), label.to_owned()],
        generation: None,
        hex: None,
        mode: Some(DatastoreMode::CREATE_OR_REPLACE),
        string: Some(serde_json::to_string(&nwc_store)?),
    })
    .await?;
    log::debug!("stored nwc store for label:{label}");
    Ok(())
}

pub fn rpc_socket_path(plugin: &Plugin<PluginState>) -> PathBuf {
    Path::new(&plugin.configuration().lightning_dir).join(&plugin.configuration().rpc_file)
}

/// Reserve the payment amount plus the worst case fee so that no combination
/// of concurrent payments can exceed the budget. Must be called while holding
/// the global rpc lock. Returns the reserved amount (0 if there is no budget).
pub async fn reserve_budget(
    rpc: &mut ClnRpc,
    label: &str,
    nwc_store: &NwcStore,
    amount_msat: u64,
) -> Result<u64, anyhow::Error> {
    if nwc_store.budget_msat.is_none() {
        return Ok(0);
    }

    let reservation = amount_msat.saturating_add(payment_fee_reserve_msat(amount_msat));
    let mut new_store = nwc_store.clone();
    new_store.reserved_msat = new_store.reserved_msat.saturating_add(reservation);
    update_nwc_store(rpc, label, new_store).await?;

    Ok(reservation)
}

/// Release the reservation of a payment that did not succeed. Must be called
/// while holding the global rpc lock.
pub async fn refund_budget(
    rpc: &mut ClnRpc,
    label: &str,
    reservation: u64,
) -> Result<(), anyhow::Error> {
    if reservation == 0 {
        return Ok(());
    }

    let mut nwc_store = load_nwc_store(rpc, label).await?;
    nwc_store.reserved_msat = nwc_store.reserved_msat.saturating_sub(reservation);
    update_nwc_store(rpc, label, nwc_store).await?;

    Ok(())
}

/// Record the actual amount spent by a successful payment and release the
/// remainder of the reservation. Must be called while holding the global rpc
/// lock. Charges the real spend first so that an error leaves the budget
/// conservatively low rather than too high.
pub async fn settle_budget(
    rpc: &mut ClnRpc,
    label: &str,
    reservation: u64,
    amount_spent_msat: u64,
) -> Result<(), anyhow::Error> {
    // A zero reservation only happens when the nwc has no budget at all.
    if reservation == 0 {
        return Ok(());
    }

    let mut nwc_store = load_nwc_store(rpc, label).await?;
    update_budget_msat(&mut nwc_store, amount_spent_msat);
    nwc_store.reserved_msat = nwc_store.reserved_msat.saturating_sub(reservation);
    update_nwc_store(rpc, label, nwc_store).await?;

    Ok(())
}

pub fn is_read_only_nwc(nwc_store: &NwcStore) -> bool {
    if let Some(budget_msat) = nwc_store.budget_msat {
        if budget_msat == 0 && nwc_store.interval_config.is_none() {
            return true;
        }
    }
    false
}

pub async fn save_event_id(
    rpc: &mut ClnRpc,
    id: String,
    timestamp: Timestamp,
) -> Result<(), anyhow::Error> {
    rpc.call_typed(&DatastoreRequest {
        key: vec![format!("{}-{}", PLUGIN_NAME, ID_STORE), id.clone()],
        generation: None,
        hex: None,
        mode: Some(DatastoreMode::MUST_CREATE),
        string: Some(timestamp.to_string()),
    })
    .await?;
    log::debug!("stored event id:{id}");
    Ok(())
}

pub fn build_capabilities(is_read_only: bool, plugin: &Plugin<PluginState>) -> (String, String) {
    let holdinvoice_support = plugin.state().hold_client.lock().is_some();

    let mut methods = WALLET_READ_METHODS.map(|m| m.to_string()).join(" ");
    if !is_read_only {
        methods.push(' ');
        methods.push_str(WALLET_PAY_METHODS.map(|m| m.to_string()).join(" ").as_str());
    }
    if holdinvoice_support {
        methods.push(' ');
        methods.push_str(
            WALLET_HOLD_METHODS
                .map(|m| m.to_string())
                .join(" ")
                .as_str(),
        );
    }

    let mut notifications = String::new();
    if plugin.option(&OPT_NOTIFICATIONS).unwrap() {
        notifications.push_str(
            WALLET_NOTIFICATIONS
                .map(|m| m.to_string())
                .join(" ")
                .as_str(),
        );
        if holdinvoice_support {
            notifications.push(' ');
            notifications.push_str(
                WALLET_HOLD_NOTIFICATIONS
                    .map(|m| m.to_string())
                    .join(" ")
                    .as_str(),
            );
        }
    }

    (methods, notifications)
}

pub fn build_methods_vec(is_read_only: bool, plugin: &Plugin<PluginState>) -> Vec<nip47::Method> {
    let holdinvoice_support = plugin.state().hold_client.lock().is_some();
    let mut methods = WALLET_READ_METHODS.to_vec();
    if !is_read_only {
        methods.extend_from_slice(&WALLET_PAY_METHODS);
    }
    if holdinvoice_support {
        methods.extend_from_slice(&WALLET_HOLD_METHODS);
    }
    methods
}

pub fn build_notifications_vec(plugin: &Plugin<PluginState>) -> Vec<String> {
    let holdinvoice_support = plugin.state().hold_client.lock().is_some();

    let mut notifications = Vec::new();
    if plugin.option(&OPT_NOTIFICATIONS).unwrap() {
        notifications.extend_from_slice(&WALLET_NOTIFICATIONS.map(|m| m.to_string()));
        if holdinvoice_support {
            notifications.extend_from_slice(&WALLET_HOLD_NOTIFICATIONS.map(|m| m.to_string()));
        }
    }
    notifications
}

#[test]
fn test_budget_check() {
    assert!(budget_amount_check(Some(1), Some(1), Some(2)).is_ok());
    assert!(budget_amount_check(Some(1), Some(2), Some(2)).is_err());
    assert!(budget_amount_check(Some(2), Some(2), Some(1)).is_err());
    assert!(budget_amount_check(Some(2), None, None).is_ok());
    assert!(budget_amount_check(Some(2), None, Some(2)).is_ok());

    assert!(budget_amount_check(None, None, None).is_err());
    assert!(budget_amount_check(None, None, Some(2)).is_err());
    assert!(budget_amount_check(Some(0), None, Some(1)).is_ok());
    assert!(budget_amount_check(Some(0), None, Some(0)).is_ok());
    assert!(budget_amount_check(None, Some(0), Some(1)).is_ok());
    assert!(budget_amount_check(None, Some(0), Some(0)).is_ok());
    assert!(budget_amount_check(Some(0), Some(0), Some(1)).is_ok());
    assert!(budget_amount_check(Some(0), Some(0), Some(0)).is_ok());
}

#[test]
fn test_budget_interval_helpers() {
    use crate::structs::BudgetIntervalConfig;

    let now = Timestamp::now().as_secs();
    let conf = BudgetIntervalConfig {
        interval_secs: 10,
        reset_budget_msat: 1000,
        last_reset: now,
        spend_since_last_reset: 0,
    };
    let mut store = NwcStore {
        uri: nostr::nips::nip47::NostrWalletConnectUri::new(
            nostr::key::Keys::generate().public_key(),
            vec![],
            nostr::key::Keys::generate().secret_key().clone(),
            None,
        ),
        walletkey: "test".to_owned(),
        budget_msat: Some(1000),
        interval_config: Some(conf),
        reserved_msat: 0,
    };

    assert_eq!(get_budget_msat(&store), Some(1000));

    update_budget_msat(&mut store, 300);
    assert_eq!(
        store
            .interval_config
            .as_ref()
            .unwrap()
            .spend_since_last_reset,
        300
    );
    assert_eq!(get_budget_msat(&store), Some(700));

    update_budget_msat(&mut store, 500);
    assert_eq!(get_budget_msat(&store), Some(200));

    store.interval_config.as_mut().unwrap().last_reset = now.saturating_sub(20);
    let old_last_reset = store.interval_config.as_ref().unwrap().last_reset;
    assert_eq!(get_budget_msat(&store), Some(1000));

    update_budget_msat(&mut store, 400);
    assert!(store.interval_config.as_ref().unwrap().last_reset > old_last_reset);
    assert_eq!(
        store
            .interval_config
            .as_ref()
            .unwrap()
            .spend_since_last_reset,
        400
    );
    assert_eq!(get_budget_msat(&store), Some(600));
}

#[test]
fn test_payment_fee_reserve() {
    // fee reserve is at least 5000msat
    assert_eq!(payment_fee_reserve_msat(0), 5000);
    assert_eq!(payment_fee_reserve_msat(1000), 5000);
    assert_eq!(payment_fee_reserve_msat(499_999), 5000);
    // and 1% of the amount above that
    assert_eq!(payment_fee_reserve_msat(500_000), 5000);
    assert_eq!(payment_fee_reserve_msat(1_000_000), 10_000);
    assert_eq!(payment_fee_reserve_msat(123_456_789), 1_234_567);
}

#[test]
fn test_budget_reservations() {
    let store = NwcStore {
        uri: nostr::nips::nip47::NostrWalletConnectUri::new(
            nostr::key::Keys::generate().public_key(),
            vec![],
            nostr::key::Keys::generate().secret_key().clone(),
            None,
        ),
        walletkey: "test".to_owned(),
        budget_msat: Some(10_000),
        interval_config: None,
        reserved_msat: 0,
    };

    // reserving reduces the available budget
    let mut stored = store.clone();
    stored.reserved_msat = 1_500;
    assert_eq!(get_budget_msat(&stored), Some(8_500));

    // a full spend plus reservation can never be under budget
    let full_reservation =
        stored.clone().budget_msat.unwrap() + payment_fee_reserve_msat(store.budget_msat.unwrap());
    stored.reserved_msat = full_reservation;
    assert_eq!(get_budget_msat(&stored), Some(0));

    // reservations do not affect a store without a budget
    let mut no_budget = store;
    no_budget.budget_msat = None;
    no_budget.reserved_msat = 1_000;
    assert_eq!(get_budget_msat(&no_budget), None);
}

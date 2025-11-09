use anyhow::anyhow;
use cln_plugin::Plugin;
use cln_rpc::model::requests::{
    DatastoreMode, DatastoreRequest, DeldatastoreRequest, ListdatastoreRequest,
};
use nostr_sdk::nips::nip47::NostrWalletConnectURI;
use nostr_sdk::Keys;
use nostr_sdk::SecretKey;
use nostr_sdk::Timestamp;
use serde_json::json;

use crate::nwc::{
    run_nwc, send_nwc_info_event, start_nwc_budget_job, stop_nwc, stop_nwc_budget_job,
};
use crate::parse::parse_time_period;
use crate::structs::{BudgetIntervalConfig, NwcStore, PluginState};
use crate::util::build_capabilities;
use crate::util::{is_read_only_nwc, load_nwc_store, update_nwc_store};
use crate::PLUGIN_NAME;

pub async fn nwc_create(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, anyhow::Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let (label, budget_msat, interval_secs) = parse_full_args(args)?;

    let config = plugin.state().config.lock().clone();

    let wallet_keys = Keys::generate();
    let client_keys = Keys::generate();
    let uri = NostrWalletConnectURI::new(
        wallet_keys.public_key(),
        config.relays.clone(),
        client_keys.secret_key().clone(),
        None,
    );

    let mut result = serde_json::Map::new();
    result.insert("uri".to_owned(), serde_json::Value::String(uri.to_string()));
    result.insert("label".to_owned(), serde_json::Value::String(label.clone()));
    result.insert(
        "walletkey_public".to_owned(),
        serde_json::Value::String(wallet_keys.public_key().to_string()),
    );
    result.insert(
        "clientkey_public".to_owned(),
        serde_json::Value::String(client_keys.public_key().to_string()),
    );

    let interval_config = if let Some(bgt_msat) = budget_msat {
        result.insert(
            "budget_msat".to_owned(),
            serde_json::Value::Number(bgt_msat.into()),
        );
        if let Some(interval) = interval_secs {
            let conf = BudgetIntervalConfig {
                interval_secs: interval,
                reset_budget_msat: bgt_msat,
                last_reset: Timestamp::now().as_secs(),
            };
            result.insert("interval_config".to_owned(), serde_json::to_value(&conf)?);
            Some(conf)
        } else {
            None
        }
    } else {
        None
    };
    let nwc_store = NwcStore {
        uri: uri.clone(),
        walletkey: wallet_keys.secret_key().to_secret_hex(),
        budget_msat,
        interval_config,
    };

    rpc.call_typed(&DatastoreRequest {
        generation: None,
        hex: None,
        mode: Some(DatastoreMode::MUST_CREATE),
        string: Some(serde_json::to_string(&nwc_store)?),
        key: vec![PLUGIN_NAME.to_owned(), label.clone()],
    })
    .await?;

    run_nwc(plugin.clone(), label.clone(), nwc_store.clone()).await?;

    Ok(serde_json::Value::Object(result))
}

pub async fn nwc_revoke(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, anyhow::Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let label = parse_revoke_args(args)?;

    stop_nwc(plugin.clone(), &label).await;

    rpc.call_typed(&DeldatastoreRequest {
        generation: None,
        key: vec![PLUGIN_NAME.to_owned(), label.clone()],
    })
    .await?;

    Ok(json!({"revoked":label}))
}

pub async fn nwc_budget(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, anyhow::Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let (label, budget_msat, interval_secs) = parse_full_args(args)?;

    stop_nwc_budget_job(&plugin, &label);

    let mut nwc_store = load_nwc_store(&mut rpc, &label).await?;

    let is_old_nwc_read_only = is_read_only_nwc(&nwc_store);

    if let Some(budget) = budget_msat {
        nwc_store.budget_msat = Some(budget);
        if let Some(interval) = interval_secs {
            let interval_config = BudgetIntervalConfig {
                interval_secs: interval,
                reset_budget_msat: budget,
                last_reset: Timestamp::now().as_secs(),
            };
            nwc_store.interval_config = Some(interval_config.clone());
        } else {
            nwc_store.interval_config = None;
        }
    } else {
        nwc_store.budget_msat = None;
        nwc_store.interval_config = None;
    }

    let is_new_nwc_read_only = is_read_only_nwc(&nwc_store);

    if nwc_store.interval_config.is_some() {
        start_nwc_budget_job(&plugin, label.clone());
    }

    update_nwc_store(&mut rpc, &label, nwc_store.clone()).await?;

    if is_old_nwc_read_only != is_new_nwc_read_only {
        let wallet_keys = Keys::new(SecretKey::from_hex(&nwc_store.walletkey)?);
        let (method_capabilities, _) = build_capabilities(is_new_nwc_read_only, &plugin);
        let clients = plugin.state().handles.lock().await;
        send_nwc_info_event(
            plugin.clone(),
            clients
                .get(&label)
                .ok_or_else(|| anyhow!("No client found for label: {label}"))?
                .0
                .clone(),
            method_capabilities,
            wallet_keys,
        )
        .await?;
    }

    Ok(json!({"budget_updated":label}))
}

pub async fn nwc_list(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, anyhow::Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let label = parse_list_args(args)?;

    let mut nwcs = Vec::new();

    if let Some(lbl) = label {
        let nwc_store = load_nwc_store(&mut rpc, &lbl).await?;
        let wallet_key = Keys::new(SecretKey::from_hex(&nwc_store.walletkey)?);
        let client_key = Keys::new(nwc_store.uri.secret.clone());
        let mut nwc_json = json!(nwc_store);
        nwc_json.as_object_mut().and_then(|n| {
            n.insert(
                "walletkey_public".to_owned(),
                json!(wallet_key.public_key()),
            );
            n.insert(
                "clientkey_public".to_owned(),
                json!(client_key.public_key()),
            )
        });
        nwcs.push(json!({lbl:nwc_json}));
    } else {
        let all_stored_nwcs = rpc
            .call_typed(&ListdatastoreRequest {
                key: Some(vec![PLUGIN_NAME.to_owned()]),
            })
            .await?
            .datastore;

        for datastore in all_stored_nwcs {
            let label = datastore.key.last().unwrap().to_owned();
            let nwc_store = load_nwc_store(&mut rpc, &label).await?;
            let wallet_key = Keys::new(SecretKey::from_hex(&nwc_store.walletkey)?);
            let client_key = Keys::new(nwc_store.uri.secret.clone());
            let mut nwc_json = json!(nwc_store);
            nwc_json.as_object_mut().and_then(|n| {
                n.insert(
                    "walletkey_public".to_owned(),
                    json!(wallet_key.public_key()),
                );
                n.insert(
                    "clientkey_public".to_owned(),
                    json!(client_key.public_key()),
                )
            });
            nwcs.push(json!({label:nwc_json}));
        }
    }
    Ok(serde_json::Value::Array(nwcs))
}

fn parse_full_args(
    args: serde_json::Value,
) -> Result<(String, Option<u64>, Option<u64>), anyhow::Error> {
    let label;
    let budget_msat;
    let interval_secs;

    match args {
        serde_json::Value::String(s) => return Ok((s, None, None)),
        serde_json::Value::Array(values) => {
            label = values
                .first()
                .ok_or_else(|| anyhow!("label missing"))?
                .as_str()
                .ok_or_else(|| anyhow!("label is not a string"))?
                .to_owned();
            budget_msat = if let Some(b) = values.get(1) {
                Some(
                    b.as_u64()
                        .ok_or_else(|| anyhow!("budget_msat is not an integer"))?,
                )
            } else {
                None
            };
            interval_secs = if let Some(t) = values.get(2) {
                Some(parse_time_period(
                    t.as_str()
                        .ok_or_else(|| anyhow!("interval is not a string"))?,
                )?)
            } else {
                None
            };
        }
        serde_json::Value::Object(map) => {
            label = map
                .get("label")
                .ok_or_else(|| anyhow!("label missing"))?
                .as_str()
                .ok_or_else(|| anyhow!("label is not a string"))?
                .to_owned();
            budget_msat = if let Some(b) = map.get("budget_msat") {
                Some(
                    b.as_u64()
                        .ok_or_else(|| anyhow!("budget_msat is not an integer"))?,
                )
            } else {
                None
            };
            interval_secs = if let Some(t) = map.get("interval") {
                Some(parse_time_period(
                    t.as_str()
                        .ok_or_else(|| anyhow!("interval is not a string"))?,
                )?)
            } else {
                None
            };
        }
        _ => return Err(anyhow!("Invalid argument type")),
    }

    if interval_secs.is_some() && budget_msat.is_none() {
        return Err(anyhow!("Must set `budget_msat` if you use `interval`"));
    }
    if interval_secs.is_some() && budget_msat.unwrap() == 0 {
        return Err(anyhow!(
            "`budget_msat` must be greater than 0 if you use `interval`"
        ));
    }
    Ok((label, budget_msat, interval_secs))
}

fn parse_revoke_args(args: serde_json::Value) -> Result<String, anyhow::Error> {
    match args {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Array(values) => {
            let label = values
                .first()
                .ok_or_else(|| anyhow!("label missing"))?
                .as_str()
                .ok_or_else(|| anyhow!("label is not a string"))?
                .to_owned();
            Ok(label)
        }
        serde_json::Value::Object(map) => {
            let label = map
                .get("label")
                .ok_or_else(|| anyhow!("label missing"))?
                .as_str()
                .ok_or_else(|| anyhow!("label is not a string"))?
                .to_owned();
            Ok(label)
        }
        _ => Err(anyhow!("Invalid argument type")),
    }
}

fn parse_list_args(args: serde_json::Value) -> Result<Option<String>, anyhow::Error> {
    match args {
        serde_json::Value::String(s) => Ok(Some(s)),
        serde_json::Value::Array(values) => {
            let label = if let Some(v) = values.first() {
                Some(
                    v.as_str()
                        .ok_or_else(|| anyhow!("label is not a string"))?
                        .to_owned(),
                )
            } else {
                None
            };

            Ok(label)
        }
        serde_json::Value::Object(map) => {
            let label = if let Some(v) = map.get("label") {
                Some(
                    v.as_str()
                        .ok_or_else(|| anyhow!("label is not a string"))?
                        .to_owned(),
                )
            } else {
                None
            };

            Ok(label)
        }
        _ => Err(anyhow!("Invalid argument type")),
    }
}

use std::path::Path;

use anyhow::anyhow;
use cln_plugin::Plugin;
use cln_rpc::model::requests::{
    DatastoreMode, DatastoreRequest, DeldatastoreRequest, ListdatastoreRequest,
};
use cln_rpc::ClnRpc;
use nostr_sdk::nips::nip47::*;
use nostr_sdk::*;
use serde_json::json;
use tokio::sync::oneshot;

use crate::nwc::run_nwc;
use crate::parse::parse_time_period;
use crate::structs::{BudgetIntervalConfig, NwcStore, PluginState};
use crate::tasks::budget_task;
use crate::util::{load_nwc_store, update_nwc_store};
use crate::PLUGIN_NAME;

pub async fn nwc_create(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, anyhow::Error> {
    let _guard = plugin.state().rpc_lock.lock().await;

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

    let mut rpc = ClnRpc::new(
        Path::new(&plugin.configuration().lightning_dir).join(&plugin.configuration().rpc_file),
    )
    .await?;

    let interval_config = if let Some(bgt_msat) = budget_msat {
        result.insert(
            "budget_msat".to_owned(),
            serde_json::Value::Number(bgt_msat.into()),
        );
        if let Some(interval) = interval_secs {
            let conf = BudgetIntervalConfig {
                interval_secs: interval,
                reset_budget_msat: bgt_msat,
                last_reset: Timestamp::now().as_u64(),
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

    let client = run_nwc(plugin.clone(), label.clone(), nwc_store.clone()).await?;
    let mut locked_handles = plugin.state().handles.lock().await;
    locked_handles.insert(
        label.clone(),
        (client, Keys::new(nwc_store.uri.secret).public_key()),
    );
    Ok(serde_json::Value::Object(result))
}

pub async fn nwc_revoke(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, anyhow::Error> {
    let _guard = plugin.state().rpc_lock.lock().await;

    let label = parse_revoke_args(args)?;

    {
        let mut locked_handles = plugin.state().handles.lock().await;
        if let Some((client, _client_pubkey)) = locked_handles.remove(&label) {
            client.shutdown().await;
        }

        let mut budget_jobs = plugin.state().budget_jobs.lock();
        let job = budget_jobs.remove(&label);
        if let Some(j) = job {
            let _ = j.send(());
        }
    }

    let mut rpc = ClnRpc::new(
        Path::new(&plugin.configuration().lightning_dir).join(&plugin.configuration().rpc_file),
    )
    .await?;

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
    let _guard = plugin.state().rpc_lock.lock().await;

    let (label, budget_msat, interval_secs) = parse_full_args(args)?;

    let mut rpc = ClnRpc::new(
        Path::new(&plugin.configuration().lightning_dir).join(&plugin.configuration().rpc_file),
    )
    .await?;

    {
        let mut budget_jobs = plugin.state().budget_jobs.lock();
        let job = budget_jobs.remove(&label);
        if let Some(j) = job {
            let _ = j.send(());
        }
    }

    let mut nwc_store = load_nwc_store(&mut rpc, &label).await?;

    if let Some(budget) = budget_msat {
        nwc_store.budget_msat = Some(budget);
        if let Some(interval) = interval_secs {
            let interval_config = BudgetIntervalConfig {
                interval_secs: interval,
                reset_budget_msat: budget,
                last_reset: Timestamp::now().as_u64(),
            };
            nwc_store.interval_config = Some(interval_config.clone());
        } else {
            nwc_store.interval_config = None;
        }
    } else {
        nwc_store.budget_msat = None;
        nwc_store.interval_config = None;
    }

    if nwc_store.interval_config.is_some() {
        let (tx, rx) = oneshot::channel::<()>();
        tokio::spawn(budget_task(rx, plugin.clone(), label.clone()));
        plugin.state().budget_jobs.lock().insert(label.clone(), tx);
    }

    update_nwc_store(&mut rpc, &label, nwc_store).await?;

    Ok(json!({"budget_updated":label}))
}

pub async fn nwc_list(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, anyhow::Error> {
    let _guard = plugin.state().rpc_lock.lock().await;

    let label = parse_list_args(args)?;

    let mut rpc = ClnRpc::new(
        Path::new(&plugin.configuration().lightning_dir).join(&plugin.configuration().rpc_file),
    )
    .await?;

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
        let nwcs_store = rpc
            .call_typed(&ListdatastoreRequest {
                key: Some(vec![PLUGIN_NAME.to_owned()]),
            })
            .await?
            .datastore;

        for datastore in nwcs_store.into_iter() {
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
    match args {
        serde_json::Value::String(s) => Ok((s, None, None)),
        serde_json::Value::Array(values) => {
            let label = values
                .first()
                .ok_or_else(|| anyhow!("label missing"))?
                .as_str()
                .ok_or_else(|| anyhow!("label is not a string"))?
                .to_owned();
            let budget_msat = if let Some(b) = values.get(1) {
                Some(
                    b.as_u64()
                        .ok_or_else(|| anyhow!("budget_msat is not an integer"))?,
                )
            } else {
                None
            };
            let interval_secs = if let Some(t) = values.get(2) {
                Some(parse_time_period(
                    t.as_str()
                        .ok_or_else(|| anyhow!("interval is not a string"))?,
                )?)
            } else {
                None
            };
            if interval_secs.is_some() && budget_msat.is_none() {
                return Err(anyhow!("Must set `budget_msat` if you use `interval`"));
            }
            Ok((label, budget_msat, interval_secs))
        }
        serde_json::Value::Object(map) => {
            let label = map
                .get("label")
                .ok_or_else(|| anyhow!("label missing"))?
                .as_str()
                .ok_or_else(|| anyhow!("label is not a string"))?
                .to_owned();
            let budget_msat = if let Some(b) = map.get("budget_msat") {
                Some(
                    b.as_u64()
                        .ok_or_else(|| anyhow!("budget_msat is not an integer"))?,
                )
            } else {
                None
            };
            let interval_secs = if let Some(t) = map.get("interval") {
                Some(parse_time_period(
                    t.as_str()
                        .ok_or_else(|| anyhow!("interval is not a string"))?,
                )?)
            } else {
                None
            };
            if interval_secs.is_some() && budget_msat.is_none() {
                return Err(anyhow!("Must set `budget_msat` if you use `interval`"));
            }
            Ok((label, budget_msat, interval_secs))
        }
        _ => Err(anyhow!("Invalid argument type")),
    }
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

use anyhow::anyhow;
use cln_rpc::{
    model::requests::{DatastoreMode, DatastoreRequest, ListdatastoreRequest},
    ClnRpc,
};

use crate::{structs::NwcStore, PLUGIN_NAME};

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

pub async fn load_nwc_store(rpc: &mut ClnRpc, label: &String) -> Result<NwcStore, anyhow::Error> {
    let nwc_store_store = rpc
        .call_typed(&ListdatastoreRequest {
            key: Some(vec![PLUGIN_NAME.to_owned(), label.clone()]),
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
    label: &String,
    nwc_store: NwcStore,
) -> Result<(), anyhow::Error> {
    rpc.call_typed(&DatastoreRequest {
        key: vec![PLUGIN_NAME.to_owned(), label.clone()],
        generation: None,
        hex: None,
        mode: Some(DatastoreMode::CREATE_OR_REPLACE),
        string: Some(serde_json::to_string(&nwc_store)?),
    })
    .await?;
    log::debug!("stored nwc store for label:{label}");
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

pub fn at_or_above_version(my_version: &str, min_version: &str) -> Result<bool, anyhow::Error> {
    let clean_start_my_version = my_version
        .split_once('v')
        .ok_or_else(|| anyhow!("Could not find v in version string"))?
        .1;
    let full_clean_my_version: String = clean_start_my_version
        .chars()
        .take_while(|x| x.is_ascii_digit() || *x == '.')
        .collect();

    let my_version_parts: Vec<&str> = full_clean_my_version.split('.').collect();
    let min_version_parts: Vec<&str> = min_version.split('.').collect();

    if my_version_parts.len() <= 1 || my_version_parts.len() > 3 {
        return Err(anyhow!("Version string parse error: {my_version}"));
    }
    for (my, min) in my_version_parts.iter().zip(min_version_parts.iter()) {
        let my_num: u32 = my.parse()?;
        let min_num: u32 = min.parse()?;

        if my_num != min_num {
            return Ok(my_num > min_num);
        }
    }

    Ok(my_version_parts.len() >= min_version_parts.len())
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

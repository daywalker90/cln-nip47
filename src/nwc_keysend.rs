use std::{collections::HashMap, str::FromStr};

use cln_plugin::Plugin;
use cln_rpc::{
    ClnRpc,
    RpcError,
    model::requests::{KeysendRequest, XkeysendRequest},
    primitives::{Amount, PublicKey, Secret, TlvEntry, TlvStream},
};
use nostr::nips::nip47::{self};

use crate::{
    structs::PluginState,
    util::{
        budget_amount_check,
        get_budget_msat,
        load_nwc_store,
        payment_fee_reserve_msat,
        refund_budget,
        reserve_budget,
        rpc_socket_path,
        settle_budget,
    },
};

pub const XKEYSEND_COMMAND: &str = "xkeysend";

pub async fn pay_keysend_response(
    plugin: Plugin<PluginState>,
    params: nip47::PayKeysendRequest,
    label: &str,
) -> Vec<(nip47::Response, Option<String>)> {
    let id = if let Some(i) = params.id.clone() {
        i
    } else {
        params.pubkey.clone()
    };

    vec![match pay_keysend(plugin, params, label).await {
        Ok(o) => (
            nip47::Response {
                result_type: nip47::Method::PayKeysend,
                error: None,
                result: Some(nip47::ResponseResult::PayKeysend(o)),
            },
            Some(id),
        ),
        Err(e) => (
            nip47::Response {
                result_type: nip47::Method::PayKeysend,
                error: Some(e),
                result: None,
            },
            Some(id),
        ),
    }]
}

async fn pay_keysend(
    plugin: Plugin<PluginState>,
    params: nip47::PayKeysendRequest,
    label: &str,
) -> Result<nip47::PayKeysendResponse, nip47::NIP47Error> {
    if params.preimage.is_some() {
        return Err(nip47::NIP47Error {
            code: nip47::ErrorCode::Other,
            message: "CLN generates the preimage itself!".to_owned(),
        });
    }

    let pubkey = PublicKey::from_str(&params.pubkey).map_err(|e| nip47::NIP47Error {
        code: nip47::ErrorCode::Other,
        message: e.to_string(),
    })?;

    let reservation = {
        let mut rpc = plugin.state().rpc_lock.lock().await;

        let nwc_store = load_nwc_store(&mut rpc, label)
            .await
            .map_err(|e| nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            })?;

        budget_amount_check(Some(params.amount), None, get_budget_msat(&nwc_store)).map_err(
            |e| nip47::NIP47Error {
                code: nip47::ErrorCode::QuotaExceeded,
                message: e.to_string(),
            },
        )?;

        // Reserve amount plus worst case fee so concurrent payments can never
        // exceed the budget.
        if get_budget_msat(&nwc_store).unwrap_or(u64::MAX)
            < params
                .amount
                .saturating_add(payment_fee_reserve_msat(params.amount))
        {
            return Err(nip47::NIP47Error {
                code: nip47::ErrorCode::QuotaExceeded,
                message: "Payment and estimated fees exceed the available budget".to_owned(),
            });
        }

        reserve_budget(&mut rpc, label, &nwc_store, params.amount)
            .await
            .map_err(|e| nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            })?
    };

    let has_xkeysend = plugin.state().config.lock().has_xkeysend;
    let mut pay_rpc =
        ClnRpc::new(rpc_socket_path(&plugin))
            .await
            .map_err(|e| nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: format!("Could not connect to lightningd: {e}"),
            })?;

    let pay_result = if has_xkeysend {
        xkeysend(&mut pay_rpc, &params, pubkey).await
    } else {
        keysend(&mut pay_rpc, &params, pubkey).await
    };

    match pay_result {
        Ok((amount_sent_msat, amount_msat, preimage)) => {
            let mut rpc = plugin.state().rpc_lock.lock().await;
            if let Err(e) = settle_budget(&mut rpc, label, reservation, amount_sent_msat).await {
                log::error!("Error updating budget after successful keysend: {e}");
            }

            let preimage = hex::encode(preimage.to_vec());
            let fees_paid = amount_sent_msat.saturating_sub(amount_msat);

            Ok(nip47::PayKeysendResponse {
                preimage,
                fees_paid: Some(fees_paid),
            })
        }
        Err(e) => {
            let mut rpc = plugin.state().rpc_lock.lock().await;
            if let Err(refund_err) = refund_budget(&mut rpc, label, reservation).await {
                log::error!(
                    "Error refunding budget reservation after failed keysend: {refund_err}"
                );
            }
            Err(map_keysend_error(&e, has_xkeysend))
        }
    }
}

async fn xkeysend(
    pay_rpc: &mut ClnRpc,
    params: &nip47::PayKeysendRequest,
    pubkey: PublicKey,
) -> Result<(u64, u64, Secret), RpcError> {
    let mut extratlvs = HashMap::with_capacity(params.tlv_records.len());
    for tlv in &params.tlv_records {
        extratlvs.insert(tlv.tlv_type.to_string(), tlv.value.clone());
    }
    let extratlvs = if extratlvs.is_empty() {
        None
    } else {
        Some(extratlvs)
    };

    let o = pay_rpc
        .call_typed(&XkeysendRequest {
            extratlvs,
            label: None,
            maxdelay: None,
            maxfee: None,
            retry_for: None,
            layers: None,
            amount_msat: Amount::from_msat(params.amount),
            destination: pubkey,
        })
        .await?;

    Ok((
        o.amount_sent_msat.msat(),
        o.amount_msat.msat(),
        o.payment_preimage,
    ))
}

async fn keysend(
    pay_rpc: &mut ClnRpc,
    params: &nip47::PayKeysendRequest,
    pubkey: PublicKey,
) -> Result<(u64, u64, Secret), RpcError> {
    let mut extratlvs = TlvStream {
        entries: Vec::new(),
    };
    for tlv in &params.tlv_records {
        extratlvs.entries.push(TlvEntry {
            typ: tlv.tlv_type,
            value: tlv.value.as_bytes().to_owned(),
        });
    }
    let extratlvs = if extratlvs.entries.is_empty() {
        None
    } else {
        Some(extratlvs)
    };

    let o = pay_rpc
        .call_typed(&KeysendRequest {
            exemptfee: None,
            extratlvs,
            label: None,
            maxdelay: None,
            maxfee: None,
            maxfeepercent: None,
            retry_for: None,
            routehints: None,
            amount_msat: Amount::from_msat(params.amount),
            destination: pubkey,
        })
        .await?;

    Ok((
        o.amount_sent_msat.msat(),
        o.amount_msat.msat(),
        o.payment_preimage,
    ))
}

fn map_keysend_error(e: &RpcError, is_xkeysend: bool) -> nip47::NIP47Error {
    let Some(c) = e.code else {
        return nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        };
    };

    let failed_codes = if is_xkeysend {
        vec![203, 205, 207, 219]
    } else {
        vec![203, 205, 210]
    };

    if failed_codes.contains(&c) {
        nip47::NIP47Error {
            code: nip47::ErrorCode::PaymentFailed,
            message: e.to_string(),
        }
    } else if is_xkeysend && c == 209 {
        nip47::NIP47Error {
            code: nip47::ErrorCode::Other,
            message: e.to_string(),
        }
    } else if !is_xkeysend && c == 206 {
        nip47::NIP47Error {
            code: nip47::ErrorCode::InsufficientBalance,
            message: e.to_string(),
        }
    } else {
        nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        }
    }
}

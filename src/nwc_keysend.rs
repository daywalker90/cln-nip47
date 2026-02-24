use std::str::FromStr;

use cln_plugin::Plugin;
use cln_rpc::{
    model::requests::KeysendRequest,
    primitives::{Amount, PublicKey, TlvEntry, TlvStream},
};
use nostr::nips::nip47;

use crate::{
    structs::PluginState,
    util::{budget_amount_check, load_nwc_store, update_nwc_store},
};

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
    let mut rpc = plugin.state().rpc_lock.lock().await;

    if params.preimage.is_some() {
        return Err(nip47::NIP47Error {
            code: nip47::ErrorCode::Other,
            message: "CLN generates the preimage itself!".to_owned(),
        });
    }

    let mut nwc_store = load_nwc_store(&mut rpc, label)
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?;

    budget_amount_check(Some(params.amount), None, nwc_store.budget_msat).map_err(|e| {
        nip47::NIP47Error {
            code: nip47::ErrorCode::QuotaExceeded,
            message: e.to_string(),
        }
    })?;

    let pubkey = PublicKey::from_str(&params.pubkey).map_err(|e| nip47::NIP47Error {
        code: nip47::ErrorCode::Other,
        message: e.to_string(),
    })?;

    let mut extratlvs = TlvStream {
        entries: Vec::new(),
    };
    for tlv in params.tlv_records {
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

    match rpc
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
        .await
    {
        Ok(o) => {
            if let Some(ref mut bdg) = nwc_store.budget_msat {
                *bdg = bdg.saturating_sub(o.amount_sent_msat.msat());
                update_nwc_store(&mut rpc, label, nwc_store)
                    .await
                    .map_err(|e| nip47::NIP47Error {
                        code: nip47::ErrorCode::Internal,
                        message: e.to_string(),
                    })?;
            }

            let preimage = hex::encode(o.payment_preimage.to_vec());

            let fees_paid = o.amount_sent_msat.msat() - o.amount_msat.msat();

            Ok(nip47::PayKeysendResponse {
                preimage,
                fees_paid: Some(fees_paid),
            })
        }
        Err(e) => match e.code {
            Some(c) => match c {
                203 | 205 | 210 => Err(nip47::NIP47Error {
                    code: nip47::ErrorCode::PaymentFailed,
                    message: e.to_string(),
                }),
                206 => Err(nip47::NIP47Error {
                    code: nip47::ErrorCode::InsufficientBalance,
                    message: e.to_string(),
                }),
                _ => Err(nip47::NIP47Error {
                    code: nip47::ErrorCode::Internal,
                    message: e.to_string(),
                }),
            },
            None => Err(nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            }),
        },
    }
}

use std::str::FromStr;

use cln_plugin::Plugin;
use cln_rpc::{primitives::Sha256, RpcError};
use nostr_sdk::nips::*;
use nostr_sdk::nostr::types::time::Timestamp;
use serde_json::json;

use crate::{
    structs::{HoldInvoiceRequest, HoldInvoiceResponse, PluginState},
    tasks::hold_invoice_watcher,
};

pub async fn make_hold_invoice(
    plugin: Plugin<PluginState>,
    params: nip47::MakeHoldInvoiceRequest,
) -> Result<nip47::MakeHoldInvoiceResponse, nip47::NIP47Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let mut deschashonly = None;

    if let Some(d_hash) = params.description_hash {
        if params.description.is_none() {
            return Err(nip47::NIP47Error {
                code: nip47::ErrorCode::Other,
                message: "Must have description when using description_hash".to_owned(),
            });
        }
        let description = params.description.as_ref().unwrap();
        let my_description_hash = Sha256::const_hash(description.as_bytes());
        let description_hash = Sha256::from_str(&d_hash).map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?;
        if my_description_hash != description_hash {
            return Err(nip47::NIP47Error {
                code: nip47::ErrorCode::Other,
                message: "description_hash not matching description".to_owned(),
            });
        }
        deschashonly = Some(true)
    }

    let holdinvoice_request = HoldInvoiceRequest {
        amount_msat: params.amount,
        description: params
            .description
            .unwrap_or("NWC make_hold_invoice".to_owned()),
        expiry: params.expiry,
        exposeprivatechannels: None,
        preimage: None,
        payment_hash: Some(params.payment_hash),
        cltv: params.cltv_expiry_delta,
        deschashonly,
    };

    let holdinvoice: Result<HoldInvoiceResponse, RpcError> = rpc
        .call_raw("holdinvoice", &json!(holdinvoice_request))
        .await;
    match holdinvoice {
        Ok(o) => {
            let response = nip47::MakeHoldInvoiceResponse {
                invoice: Some(o.bolt11),
                transaction_type: nip47::TransactionType::Incoming,
                description: o.description,
                description_hash: o.description_hash.map(|d| d.to_string()),
                amount: params.amount,
                created_at: Timestamp::now(),
                expires_at: Timestamp::from_secs(o.expires_at),
                metadata: None,
                payment_hash: o.payment_hash.to_string(),
            };
            tokio::spawn(hold_invoice_watcher(plugin.clone(), response.clone()));
            Ok(response)
        }
        Err(e) => Err(nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        }),
    }
}

pub async fn cancel_hold_invoice(
    plugin: Plugin<PluginState>,
    params: nip47::CancelHoldInvoiceRequest,
) -> Result<nip47::CancelHoldInvoiceResponse, nip47::NIP47Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let holdinvoice: Result<serde_json::Value, RpcError> = rpc
        .call_raw(
            "holdinvoicecancel",
            &json!({"payment_hash": params.payment_hash}),
        )
        .await;
    match holdinvoice {
        Ok(_o) => Ok(nip47::CancelHoldInvoiceResponse {}),
        Err(e) => Err(nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        }),
    }
}

pub async fn settle_hold_invoice(
    plugin: Plugin<PluginState>,
    params: nip47::SettleHoldInvoiceRequest,
) -> Result<nip47::SettleHoldInvoiceResponse, nip47::NIP47Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let holdinvoice: Result<serde_json::Value, RpcError> = rpc
        .call_raw("holdinvoicesettle", &json!({"preimage": params.preimage}))
        .await;
    match holdinvoice {
        Ok(_o) => Ok(nip47::SettleHoldInvoiceResponse {}),
        Err(e) => Err(nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        }),
    }
}

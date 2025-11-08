use std::str::FromStr;

use cln_plugin::Plugin;
use cln_rpc::primitives::Sha256;
use nostr_sdk::{nips::nip47, Timestamp};

use crate::{
    hold::{invoice_request::Description, CancelRequest, InvoiceRequest, SettleRequest},
    nwc_notifications::holdinvoice_accepted_handler,
    structs::PluginState,
};

pub async fn make_hold_invoice_response(
    plugin: Plugin<PluginState>,
    params: nip47::MakeHoldInvoiceRequest,
) -> Vec<(nip47::Response, Option<String>)> {
    vec![match make_hold_invoice(plugin, params).await {
        Ok(o) => (
            nip47::Response {
                result_type: nip47::Method::MakeHoldInvoice,
                error: None,
                result: Some(nip47::ResponseResult::MakeHoldInvoice(o)),
            },
            None,
        ),
        Err(e) => (
            nip47::Response {
                result_type: nip47::Method::MakeHoldInvoice,
                error: Some(e),
                result: None,
            },
            None,
        ),
    }]
}

async fn make_hold_invoice(
    plugin: Plugin<PluginState>,
    params: nip47::MakeHoldInvoiceRequest,
) -> Result<nip47::MakeHoldInvoiceResponse, nip47::NIP47Error> {
    let Some(mut hold_client) = plugin.state().hold_client.lock().clone() else {
        return Err(nip47::NIP47Error {
            code: nip47::ErrorCode::NotImplemented,
            message: "No hold plugin found".to_owned(),
        });
    };

    let description: Option<Description> = if let Some(d_hash) = &params.description_hash {
        if let Some(description) = &params.description {
            let my_description_hash = Sha256::const_hash(description.as_bytes());
            let description_hash = Sha256::from_str(d_hash).map_err(|e| nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            })?;
            if my_description_hash != description_hash {
                return Err(nip47::NIP47Error {
                    code: nip47::ErrorCode::Other,
                    message: "description_hash not matching description".to_owned(),
                });
            }
        }

        let desc_hash_bytes = match hex::decode(d_hash) {
            Ok(p) => p,
            Err(_e) => {
                return Err(nip47::NIP47Error {
                    code: nip47::ErrorCode::Other,
                    message: "Could not convert description hash to bytes".to_owned(),
                })
            }
        };
        Some(Description::Hash(desc_hash_bytes))
    } else {
        params
            .description
            .as_ref()
            .map(|desc| Description::Memo(desc.clone()))
    };

    let payment_hash = match hex::decode(&params.payment_hash) {
        Ok(p) => p,
        Err(_e) => {
            return Err(nip47::NIP47Error {
                code: nip47::ErrorCode::Other,
                message: "Invalid payment hash".to_owned(),
            })
        }
    };

    let expiry = params.expiry.unwrap_or(60 * 60);

    let holdinvoice_request = InvoiceRequest {
        payment_hash: payment_hash.clone(),
        amount_msat: params.amount,
        expiry: Some(expiry),
        min_final_cltv_expiry: params.cltv_expiry_delta.map(u64::from),
        routing_hints: Vec::new(),
        description,
    };

    let holdinvoice = hold_client
        .invoice(holdinvoice_request)
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Other,
            message: format!("Error creating hold invoice: {e}"),
        })?
        .into_inner();

    let response = nip47::MakeHoldInvoiceResponse {
        invoice: Some(holdinvoice.bolt11),
        transaction_type: nip47::TransactionType::Incoming,
        description: params.description,
        description_hash: params.description_hash,
        amount: params.amount,
        created_at: Timestamp::now(),
        expires_at: Timestamp::now() + expiry,
        metadata: None,
        payment_hash: params.payment_hash,
    };

    tokio::spawn(holdinvoice_accepted_handler(plugin, payment_hash));
    Ok(response)
}

pub async fn cancel_hold_invoice_response(
    plugin: Plugin<PluginState>,
    params: nip47::CancelHoldInvoiceRequest,
) -> Vec<(nip47::Response, Option<String>)> {
    vec![match cancel_hold_invoice(plugin, params).await {
        Ok(o) => (
            nip47::Response {
                result_type: nip47::Method::CancelHoldInvoice,
                error: None,
                result: Some(nip47::ResponseResult::CancelHoldInvoice(o)),
            },
            None,
        ),
        Err(e) => (
            nip47::Response {
                result_type: nip47::Method::CancelHoldInvoice,
                error: Some(e),
                result: None,
            },
            None,
        ),
    }]
}

async fn cancel_hold_invoice(
    plugin: Plugin<PluginState>,
    params: nip47::CancelHoldInvoiceRequest,
) -> Result<nip47::CancelHoldInvoiceResponse, nip47::NIP47Error> {
    let Some(mut hold_client) = plugin.state().hold_client.lock().clone() else {
        return Err(nip47::NIP47Error {
            code: nip47::ErrorCode::NotImplemented,
            message: "No hold plugin found".to_owned(),
        });
    };

    let payment_hash = match hex::decode(&params.payment_hash) {
        Ok(p) => p,
        Err(_e) => {
            return Err(nip47::NIP47Error {
                code: nip47::ErrorCode::Other,
                message: "Invalid payment hash".to_owned(),
            })
        }
    };

    let hold_cancel_request = CancelRequest { payment_hash };

    let hold_cancel_response = hold_client.cancel(hold_cancel_request).await;

    match hold_cancel_response {
        Ok(_o) => Ok(nip47::CancelHoldInvoiceResponse {}),
        Err(e) => Err(nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        }),
    }
}

pub async fn settle_hold_invoice_response(
    plugin: Plugin<PluginState>,
    params: nip47::SettleHoldInvoiceRequest,
) -> Vec<(nip47::Response, Option<String>)> {
    vec![match settle_hold_invoice(plugin, params).await {
        Ok(o) => (
            nip47::Response {
                result_type: nip47::Method::SettleHoldInvoice,
                error: None,
                result: Some(nip47::ResponseResult::SettleHoldInvoice(o)),
            },
            None,
        ),
        Err(e) => (
            nip47::Response {
                result_type: nip47::Method::SettleHoldInvoice,
                error: Some(e),
                result: None,
            },
            None,
        ),
    }]
}

async fn settle_hold_invoice(
    plugin: Plugin<PluginState>,
    params: nip47::SettleHoldInvoiceRequest,
) -> Result<nip47::SettleHoldInvoiceResponse, nip47::NIP47Error> {
    let Some(mut hold_client) = plugin.state().hold_client.lock().clone() else {
        return Err(nip47::NIP47Error {
            code: nip47::ErrorCode::NotImplemented,
            message: "No hold plugin found".to_owned(),
        });
    };

    let preimage = match hex::decode(&params.preimage) {
        Ok(p) => p,
        Err(_e) => {
            return Err(nip47::NIP47Error {
                code: nip47::ErrorCode::Other,
                message: "Invalid preimage".to_owned(),
            })
        }
    };

    let hold_settle_request = SettleRequest {
        payment_preimage: preimage,
    };

    let hold_settle_response = hold_client.settle(hold_settle_request).await;

    match hold_settle_response {
        Ok(_o) => Ok(nip47::SettleHoldInvoiceResponse {}),
        Err(e) => Err(nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        }),
    }
}

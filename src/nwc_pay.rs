use std::time::Duration;

use cln_plugin::Plugin;
use cln_rpc::{
    model::requests::{DecodeRequest, PayRequest, XpayRequest},
    primitives::Amount,
};
use nostr_sdk::nips::*;
use tokio::time;

use crate::{
    structs::PluginState,
    util::{at_or_above_version, budget_amount_check, load_nwc_store, update_nwc_store},
};

pub async fn pay_invoice(
    plugin: Plugin<PluginState>,
    params: nip47::PayInvoiceRequest,
    label: &String,
) -> Result<(nip47::PayInvoiceResponse, String), (nip47::NIP47Error, String)> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let id = params.id.clone().unwrap_or_default();

    let invoice_decoded = rpc
        .call_typed(&DecodeRequest {
            string: params.invoice.clone(),
        })
        .await
        .map_err(|e| {
            (
                nip47::NIP47Error {
                    code: nip47::ErrorCode::Internal,
                    message: e.to_string(),
                },
                id.clone(),
            )
        })?;

    let not_invoice_error = Err((
        nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: "Not an invoice or invalid invoice".to_owned(),
        },
        id.clone(),
    ));

    if !invoice_decoded.valid {
        return not_invoice_error;
    }

    let id = if let Some(i) = params.id {
        i
    } else {
        match invoice_decoded.item_type {
            cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => {
                invoice_decoded.invoice_payment_hash.unwrap().to_string()
            }
            cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
                invoice_decoded.payment_hash.unwrap().to_string()
            }
            _ => return not_invoice_error,
        }
    };

    let invoice_amt_msat = match invoice_decoded.item_type {
        cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => {
            invoice_decoded.invoice_amount_msat.unwrap().msat()
        }
        cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
            invoice_decoded.amount_msat.unwrap().msat()
        }
        _ => return not_invoice_error,
    };

    let mut nwc_store = load_nwc_store(&mut rpc, label).await.map_err(|e| {
        (
            nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            },
            id.clone(),
        )
    })?;

    budget_amount_check(params.amount, Some(invoice_amt_msat), nwc_store.budget_msat).map_err(
        |e| {
            (
                nip47::NIP47Error {
                    code: nip47::ErrorCode::QuotaExceeded,
                    message: e.to_string(),
                },
                id.clone(),
            )
        },
    )?;

    let my_version = plugin.state().config.lock().clone().my_cln_version;

    if at_or_above_version(&my_version, "24.11").map_err(|e| {
        (
            nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            },
            id.clone(),
        )
    })? {
        match rpc
            .call_typed(&XpayRequest {
                amount_msat: params.amount.map(Amount::from_msat),
                maxdelay: None,
                maxfee: None,
                partial_msat: None,
                retry_for: None,
                layers: None,
                invstring: params.invoice,
            })
            .await
        {
            Ok(o) => {
                if let Some(ref mut bdg) = nwc_store.budget_msat {
                    *bdg = bdg.saturating_sub(o.amount_sent_msat.msat());
                    update_nwc_store(&mut rpc, label, nwc_store)
                        .await
                        .map_err(|e| {
                            (
                                nip47::NIP47Error {
                                    code: nip47::ErrorCode::Internal,
                                    message: e.to_string(),
                                },
                                id.clone(),
                            )
                        })?;
                }

                let preimage = hex::encode(o.payment_preimage.to_vec());
                let fees_paid = o.amount_sent_msat.msat() - o.amount_msat.msat();
                Ok((
                    nip47::PayInvoiceResponse {
                        preimage,
                        fees_paid: Some(fees_paid),
                    },
                    id,
                ))
            }
            Err(e) => match e.code {
                Some(c) => match c {
                    207 | 219 => Err((
                        nip47::NIP47Error {
                            code: nip47::ErrorCode::Other,
                            message: e.to_string(),
                        },
                        id,
                    )),
                    203 | 205 | 209 => Err((
                        nip47::NIP47Error {
                            code: nip47::ErrorCode::PaymentFailed,
                            message: e.to_string(),
                        },
                        id,
                    )),
                    _ => Err((
                        nip47::NIP47Error {
                            code: nip47::ErrorCode::Internal,
                            message: e.to_string(),
                        },
                        id,
                    )),
                },
                None => Err((
                    nip47::NIP47Error {
                        code: nip47::ErrorCode::Internal,
                        message: e.to_string(),
                    },
                    id,
                )),
            },
        }
    } else {
        match rpc
            .call_typed(&PayRequest {
                amount_msat: params.amount.map(Amount::from_msat),
                description: None,
                exemptfee: None,
                label: None,
                localinvreqid: None,
                maxdelay: None,
                maxfee: None,
                maxfeepercent: None,
                partial_msat: None,
                retry_for: None,
                riskfactor: None,
                exclude: None,
                bolt11: params.invoice,
            })
            .await
        {
            Ok(o) => {
                if let Some(ref mut bdg) = nwc_store.budget_msat {
                    *bdg = bdg.saturating_sub(o.amount_sent_msat.msat());
                    update_nwc_store(&mut rpc, label, nwc_store)
                        .await
                        .map_err(|e| {
                            (
                                nip47::NIP47Error {
                                    code: nip47::ErrorCode::Internal,
                                    message: e.to_string(),
                                },
                                id.clone(),
                            )
                        })?;
                }

                let preimage = hex::encode(o.payment_preimage.to_vec());
                let fees_paid = o.amount_sent_msat.msat() - o.amount_msat.msat();
                Ok((
                    nip47::PayInvoiceResponse {
                        preimage,
                        fees_paid: Some(fees_paid),
                    },
                    id,
                ))
            }
            Err(e) => match e.code {
                Some(c) => match c {
                    201 | 207 | 219 => Err((
                        nip47::NIP47Error {
                            code: nip47::ErrorCode::Other,
                            message: e.to_string(),
                        },
                        id,
                    )),
                    203 | 205 | 209 | 210 => Err((
                        nip47::NIP47Error {
                            code: nip47::ErrorCode::PaymentFailed,
                            message: e.to_string(),
                        },
                        id,
                    )),
                    206 => Err((
                        nip47::NIP47Error {
                            code: nip47::ErrorCode::InsufficientBalance,
                            message: e.to_string(),
                        },
                        id,
                    )),
                    _ => Err((
                        nip47::NIP47Error {
                            code: nip47::ErrorCode::Internal,
                            message: e.to_string(),
                        },
                        id,
                    )),
                },
                None => Err((
                    nip47::NIP47Error {
                        code: nip47::ErrorCode::Internal,
                        message: e.to_string(),
                    },
                    id,
                )),
            },
        }
    }
}

pub async fn multi_pay_invoice(
    plugin: Plugin<PluginState>,
    params: nip47::MultiPayInvoiceRequest,
    label: &String,
) -> Vec<(nip47::Response, String)> {
    let mut responses = Vec::new();
    for pay in params.invoices {
        let result = pay_invoice(plugin.clone(), pay, label).await;
        let response_res = match result {
            Ok((resp, id)) => (
                nip47::Response {
                    result_type: nip47::Method::MultiPayInvoice,
                    error: None,
                    result: Some(nip47::ResponseResult::MultiPayInvoice(resp)),
                },
                id,
            ),
            Err((e, id)) => (
                nip47::Response {
                    result_type: nip47::Method::MultiPayInvoice,
                    error: Some(e),
                    result: None,
                },
                id,
            ),
        };
        responses.push(response_res);
        time::sleep(Duration::from_millis(100)).await;
    }
    responses
}

use cln_plugin::Plugin;
use cln_rpc::{
    ClnRpc,
    RpcError,
    model::{
        requests::{DecodeRequest, PayRequest, XpayRequest},
        responses::DecodeResponse,
    },
    primitives::{Amount, Secret},
};
use nostr::nips::nip47;

use crate::{
    structs::{NOT_INV_ERR, NwcStore, PluginState},
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

pub const XPAY_COMMAND: &str = "xpay";

pub async fn pay_invoice_response(
    plugin: Plugin<PluginState>,
    params: nip47::PayInvoiceRequest,
    label: &str,
) -> Vec<(nip47::Response, Option<String>)> {
    vec![match pay_invoice(plugin, params, label).await {
        Ok((o, id)) => (
            nip47::Response {
                result_type: nip47::Method::PayInvoice,
                error: None,
                result: Some(nip47::ResponseResult::PayInvoice(o)),
            },
            id,
        ),
        Err((e, id)) => (
            nip47::Response {
                result_type: nip47::Method::PayInvoice,
                error: Some(e),
                result: None,
            },
            id,
        ),
    }]
}

async fn pay_invoice(
    plugin: Plugin<PluginState>,
    params: nip47::PayInvoiceRequest,
    label: &str,
) -> Result<(nip47::PayInvoiceResponse, Option<String>), (nip47::NIP47Error, Option<String>)> {
    let (id, reservation) = {
        let mut rpc = plugin.state().rpc_lock.lock().await;

        let decoded_invoice = decode_and_validate_invoice(&mut rpc, &params).await?;

        let id = get_payment_id(&params, &decoded_invoice)?;

        let invoice_amt_msat = get_invoice_amount_msat(&decoded_invoice);

        let nwc_store =
            load_nwc_and_check_budget(&mut rpc, label, &params, invoice_amt_msat, &id).await?;

        let amt_msat = match (params.amount, invoice_amt_msat) {
            (None, None) => {
                return Err((
                    nip47::NIP47Error {
                        code: nip47::ErrorCode::Internal,
                        message: "No amount found in request or invoice".to_owned(),
                    },
                    Some(id.clone()),
                ));
            }
            (None, Some(b)) => b,
            (Some(a), None) => a,
            (Some(a), Some(b)) => {
                if a != b {
                    return Err((
                        nip47::NIP47Error {
                            code: nip47::ErrorCode::Internal,
                            message: "request amount does not match invoice amount".to_owned(),
                        },
                        Some(id.clone()),
                    ));
                }
                a
            }
        };

        // Reserve the invoice amount plus the worst case fee so that no
        // combination of concurrent payments can exceed the budget and so that
        // balance queries during the payment reflect the reserved amount.
        if get_budget_msat(&nwc_store).unwrap_or(u64::MAX)
            < amt_msat.saturating_add(payment_fee_reserve_msat(amt_msat))
        {
            return Err((
                nip47::NIP47Error {
                    code: nip47::ErrorCode::QuotaExceeded,
                    message: "Payment and estimated fees exceed the available budget".to_owned(),
                },
                Some(id),
            ));
        }

        let reservation = reserve_budget(&mut rpc, label, &nwc_store, amt_msat)
            .await
            .map_err(|e| {
                (
                    nip47::NIP47Error {
                        code: nip47::ErrorCode::Internal,
                        message: e.to_string(),
                    },
                    Some(id.clone()),
                )
            })?;

        (id, reservation)
    };

    let has_xpay = plugin.state().config.lock().has_xpay;

    let mut pay_rpc = ClnRpc::new(rpc_socket_path(&plugin)).await.map_err(|e| {
        (
            nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: format!("Could not connect to lightningd: {e}"),
            },
            Some(id.clone()),
        )
    })?;

    let pay_result = if has_xpay {
        pay_with_xpay(&mut pay_rpc, &params).await
    } else {
        pay_with_legacy(&mut pay_rpc, &params).await
    };

    match pay_result {
        Ok((amount_sent_msat, amount_msat, preimage)) => {
            let mut rpc = plugin.state().rpc_lock.lock().await;
            if let Err(e) = settle_budget(&mut rpc, label, reservation, amount_sent_msat).await {
                log::error!("Error updating budget after successful payment: {e}");
            }

            let preimage_str = hex::encode(preimage.to_vec());
            let fees_paid = amount_sent_msat.saturating_sub(amount_msat);
            Ok((
                nip47::PayInvoiceResponse {
                    preimage: preimage_str,
                    fees_paid: Some(fees_paid),
                },
                Some(id),
            ))
        }
        Err(e) => {
            let mut rpc = plugin.state().rpc_lock.lock().await;
            if let Err(refund_err) = refund_budget(&mut rpc, label, reservation).await {
                log::error!(
                    "Error refunding budget reservation after failed payment: {refund_err}"
                );
            }
            Err(map_cln_error_to_nip47(&e, &id, has_xpay))
        }
    }
}

async fn decode_and_validate_invoice(
    rpc: &mut ClnRpc,
    params: &nip47::PayInvoiceRequest,
) -> Result<DecodeResponse, (nip47::NIP47Error, Option<String>)> {
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
                params.id.clone(),
            )
        })?;

    if !invoice_decoded.valid {
        return Err((
            nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: NOT_INV_ERR.to_owned(),
            },
            params.id.clone(),
        ));
    }

    if !matches!(
        invoice_decoded.item_type,
        cln_rpc::model::responses::DecodeType::BOLT11_INVOICE
    ) {
        return Err((
            nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: NOT_INV_ERR.to_owned(),
            },
            params.id.clone(),
        ));
    }

    Ok(invoice_decoded)
}

fn get_payment_id(
    params: &nip47::PayInvoiceRequest,
    decoded_invoice: &DecodeResponse,
) -> Result<String, (nip47::NIP47Error, Option<String>)> {
    let id = if let Some(i) = &params.id {
        i.clone()
    } else {
        decoded_invoice
            .payment_hash
            .as_ref()
            .ok_or_else(|| {
                (
                    nip47::NIP47Error {
                        code: nip47::ErrorCode::Internal,
                        message: "payment_hash missing in decoded invoice".to_owned(),
                    },
                    None,
                )
            })?
            .to_string()
    };

    Ok(id)
}

fn get_invoice_amount_msat(decoded_invoice: &DecodeResponse) -> Option<u64> {
    decoded_invoice.amount_msat.as_ref().map(Amount::msat)
}

async fn load_nwc_and_check_budget(
    rpc: &mut ClnRpc,
    label: &str,
    params: &nip47::PayInvoiceRequest,
    invoice_amt_msat: Option<u64>,
    id: &str,
) -> Result<NwcStore, (nip47::NIP47Error, Option<String>)> {
    let nwc_store = load_nwc_store(rpc, label).await.map_err(|e| {
        (
            nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            },
            Some(id.to_owned()),
        )
    })?;

    budget_amount_check(params.amount, invoice_amt_msat, get_budget_msat(&nwc_store)).map_err(
        |e| {
            (
                nip47::NIP47Error {
                    code: nip47::ErrorCode::QuotaExceeded,
                    message: e.to_string(),
                },
                Some(id.to_owned()),
            )
        },
    )?;

    Ok(nwc_store)
}

fn map_cln_error_to_nip47(
    e: &RpcError,
    id: &str,
    is_xpay: bool,
) -> (nip47::NIP47Error, Option<String>) {
    match e.code {
        Some(c) => {
            let other_codes = if is_xpay {
                vec![207, 219]
            } else {
                vec![201, 207, 219]
            };
            let failed_codes = if is_xpay {
                vec![203, 205, 209]
            } else {
                vec![203, 205, 209, 210]
            };

            if other_codes.contains(&c) {
                (
                    nip47::NIP47Error {
                        code: nip47::ErrorCode::Other,
                        message: e.to_string(),
                    },
                    Some(id.to_owned()),
                )
            } else if failed_codes.contains(&c) {
                (
                    nip47::NIP47Error {
                        code: nip47::ErrorCode::PaymentFailed,
                        message: e.to_string(),
                    },
                    Some(id.to_owned()),
                )
            } else if !is_xpay && c == 206 {
                (
                    nip47::NIP47Error {
                        code: nip47::ErrorCode::PaymentFailed,
                        message: format!("Route too expensive: {e}"),
                    },
                    Some(id.to_owned()),
                )
            } else {
                (
                    nip47::NIP47Error {
                        code: nip47::ErrorCode::Internal,
                        message: e.to_string(),
                    },
                    Some(id.to_owned()),
                )
            }
        }
        None => (
            nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            },
            Some(id.to_owned()),
        ),
    }
}

async fn pay_with_xpay(
    pay_rpc: &mut ClnRpc,
    params: &nip47::PayInvoiceRequest,
) -> Result<(u64, u64, Secret), RpcError> {
    let payment_result = pay_rpc
        .call_typed(&XpayRequest {
            amount_msat: params.amount.map(Amount::from_msat),
            maxdelay: None,
            maxfee: None,
            partial_msat: None,
            retry_for: None,
            layers: None,
            invstring: params.invoice.clone(),
            payer_note: None,
            dev_use_shadow: None,
            label: None,
            localinvreqid: None,
        })
        .await?;

    Ok((
        payment_result.amount_sent_msat.msat(),
        payment_result.amount_msat.msat(),
        payment_result.payment_preimage,
    ))
}

async fn pay_with_legacy(
    pay_rpc: &mut ClnRpc,
    params: &nip47::PayInvoiceRequest,
) -> Result<(u64, u64, Secret), RpcError> {
    let payment_result = pay_rpc
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
            bolt11: params.invoice.clone(),
        })
        .await?;

    Ok((
        payment_result.amount_sent_msat.msat(),
        payment_result.amount_msat.msat(),
        payment_result.payment_preimage,
    ))
}

use std::time::Duration;

use cln_plugin::Plugin;
use cln_rpc::{
    model::{
        requests::{DecodeRequest, PayRequest, XpayRequest},
        responses::DecodeResponse,
    },
    primitives::{Amount, Secret},
    ClnRpc, RpcError,
};
use nostr_sdk::nips::nip47;
use tokio::time;

use crate::{
    structs::{NwcStore, PluginState, NOT_INV_ERR},
    util::{at_or_above_version, budget_amount_check, load_nwc_store, update_nwc_store},
};

pub async fn pay_invoice(
    plugin: Plugin<PluginState>,
    params: nip47::PayInvoiceRequest,
    label: &str,
) -> Result<(nip47::PayInvoiceResponse, Option<String>), (nip47::NIP47Error, Option<String>)> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let decoded_invoice = decode_and_validate_invoice(&mut rpc, &params).await?;

    let id = get_payment_id(&params, &decoded_invoice)?;

    let invoice_amt_msat = get_invoice_amount_msat(&decoded_invoice, &id)?;

    let nwc_store =
        load_nwc_and_check_budget(&mut rpc, label, &params, invoice_amt_msat, &id).await?;

    let my_version = plugin.state().config.lock().clone().my_cln_version;
    let use_xpay = check_cln_version(&my_version, &id)?;

    if use_xpay {
        pay_with_xpay_full(&mut rpc, params, label, nwc_store, &id).await
    } else {
        pay_with_legacy_full(&mut rpc, params, label, nwc_store, &id).await
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

fn get_invoice_amount_msat(
    decoded_invoice: &DecodeResponse,
    id: &str,
) -> Result<u64, (nip47::NIP47Error, Option<String>)> {
    decoded_invoice
        .amount_msat
        .as_ref()
        .ok_or_else(|| {
            (
                nip47::NIP47Error {
                    code: nip47::ErrorCode::Internal,
                    message: "Missing amount_msat in decoded invoice".to_owned(),
                },
                Some(id.to_owned()),
            )
        })
        .map(Amount::msat)
}

async fn load_nwc_and_check_budget(
    rpc: &mut ClnRpc,
    label: &str,
    params: &nip47::PayInvoiceRequest,
    invoice_amt_msat: u64,
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

    budget_amount_check(params.amount, Some(invoice_amt_msat), nwc_store.budget_msat).map_err(
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

fn check_cln_version(
    my_version: &str,
    id: &str,
) -> Result<bool, (nip47::NIP47Error, Option<String>)> {
    at_or_above_version(my_version, "24.11").map_err(|e| {
        (
            nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            },
            Some(id.to_owned()),
        )
    })
}

async fn update_budget_and_create_response(
    rpc: &mut ClnRpc,
    label: &str,
    nwc_store: &mut NwcStore,
    amount_sent_msat: u64,
    amount_msat: u64,
    preimage: Secret,
    id: &str,
) -> Result<(nip47::PayInvoiceResponse, Option<String>), (nip47::NIP47Error, Option<String>)> {
    if let Some(ref mut bdg) = nwc_store.budget_msat {
        *bdg = bdg.saturating_sub(amount_sent_msat);
        update_nwc_store(rpc, label, nwc_store.clone())
            .await
            .map_err(|e| {
                (
                    nip47::NIP47Error {
                        code: nip47::ErrorCode::Internal,
                        message: e.to_string(),
                    },
                    Some(id.to_owned()),
                )
            })?;
    }

    let preimage_str = hex::encode(preimage.to_vec());
    let fees_paid = amount_sent_msat - amount_msat;
    Ok((
        nip47::PayInvoiceResponse {
            preimage: preimage_str,
            fees_paid: Some(fees_paid),
        },
        Some(id.to_owned()),
    ))
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
                        code: nip47::ErrorCode::InsufficientBalance,
                        message: e.to_string(),
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

async fn pay_with_xpay_full(
    rpc: &mut ClnRpc,
    params: nip47::PayInvoiceRequest,
    label: &str,
    mut nwc_store: NwcStore,
    id: &str,
) -> Result<(nip47::PayInvoiceResponse, Option<String>), (nip47::NIP47Error, Option<String>)> {
    let payment_result = rpc
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
        .map_err(|e| map_cln_error_to_nip47(&e, id, true))?;

    let amount_sent_msat = payment_result.amount_sent_msat.msat();
    let amount_msat = payment_result.amount_msat.msat();
    let preimage = payment_result.payment_preimage;

    update_budget_and_create_response(
        rpc,
        label,
        &mut nwc_store,
        amount_sent_msat,
        amount_msat,
        preimage,
        id,
    )
    .await
}

async fn pay_with_legacy_full(
    rpc: &mut ClnRpc,
    params: nip47::PayInvoiceRequest,
    label: &str,
    mut nwc_store: NwcStore,
    id: &str,
) -> Result<(nip47::PayInvoiceResponse, Option<String>), (nip47::NIP47Error, Option<String>)> {
    let payment_result = rpc
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
        .map_err(|e| map_cln_error_to_nip47(&e, id, false))?;

    let amount_sent_msat = payment_result.amount_sent_msat.msat();
    let amount_msat = payment_result.amount_msat.msat();
    let preimage = payment_result.payment_preimage;

    update_budget_and_create_response(
        rpc,
        label,
        &mut nwc_store,
        amount_sent_msat,
        amount_msat,
        preimage,
        id,
    )
    .await
}

pub async fn multi_pay_invoice(
    plugin: Plugin<PluginState>,
    params: nip47::MultiPayInvoiceRequest,
    label: &str,
) -> Vec<(nip47::Response, Option<String>)> {
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

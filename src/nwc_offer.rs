use std::time::Duration;

use cln_plugin::Plugin;
use cln_rpc::{
    model::{
        requests::{DecodeRequest, FetchinvoiceRequest, OfferRequest, PayRequest, XpayRequest},
        responses::{DecodeResponse, FetchinvoiceResponse},
    },
    primitives::{Amount, Secret},
    ClnRpc,
    RpcError,
};
use nostr_sdk::{nips::nip47, Timestamp};
use tokio::time;
use uuid::Uuid;

use crate::{
    structs::{NwcStore, PluginState},
    util::{at_or_above_version, budget_amount_check, load_nwc_store, update_nwc_store},
};

pub async fn get_offer_info_response(
    plugin: Plugin<PluginState>,
    params: nip47::GetOfferInfoRequest,
) -> Vec<(nip47::Response, Option<String>)> {
    vec![match get_offer_info(plugin, params).await {
        Ok(o) => (
            nip47::Response {
                result_type: nip47::Method::GetOfferInfo,
                error: None,
                result: Some(nip47::ResponseResult::GetOfferInfo(o)),
            },
            None,
        ),
        Err(e) => (
            nip47::Response {
                result_type: nip47::Method::GetOfferInfo,
                error: Some(e),
                result: None,
            },
            None,
        ),
    }]
}

async fn get_offer_info(
    plugin: Plugin<PluginState>,
    params: nip47::GetOfferInfoRequest,
) -> Result<nip47::GetOfferInfoResponse, nip47::NIP47Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let not_offer_err = Err(nip47::NIP47Error {
        code: nip47::ErrorCode::Other,
        message: "Not an offer or invalid offer".to_owned(),
    });

    let decoded_offer = rpc
        .call_typed(&DecodeRequest {
            string: params.offer.clone(),
        })
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?;

    if !decoded_offer.valid {
        return not_offer_err;
    }

    match decoded_offer.item_type {
        cln_rpc::model::responses::DecodeType::BOLT12_OFFER => (),
        _ => return not_offer_err,
    }

    let description = decoded_offer.offer_description;

    let amount = decoded_offer.offer_amount_msat.map(|a| a.msat());

    let expires_at = decoded_offer
        .offer_absolute_expiry
        .map(Timestamp::from_secs);

    Ok(nip47::GetOfferInfoResponse {
        offer: params.offer,
        description,
        amount,
        issuer: decoded_offer.offer_issuer,
        expires_at,
        currency: None,
        currency_minor_unit: None,
    })
}

pub async fn make_offer_response(
    plugin: Plugin<PluginState>,
    params: nip47::MakeOfferRequest,
) -> Vec<(nip47::Response, Option<String>)> {
    vec![match make_offer(plugin, params).await {
        Ok(o) => (
            nip47::Response {
                result_type: nip47::Method::MakeOffer,
                error: None,
                result: Some(nip47::ResponseResult::MakeOffer(o)),
            },
            None,
        ),
        Err(e) => (
            nip47::Response {
                result_type: nip47::Method::MakeOffer,
                error: Some(e),
                result: None,
            },
            None,
        ),
    }]
}

async fn make_offer(
    plugin: Plugin<PluginState>,
    params: nip47::MakeOfferRequest,
) -> Result<nip47::MakeOfferResponse, nip47::NIP47Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let absolute_expiry = params.absolute_expiry.map(Timestamp::from_secs);

    let single_use = if let Some(su) = params.single_use {
        Some(su)
    } else {
        Some(false)
    };

    let offer = rpc
        .call_typed(&OfferRequest {
            absolute_expiry: absolute_expiry.map(|e| e.as_secs()),
            description: params.description.clone(),
            issuer: params.issuer.clone(),
            label: Some("NWC offer -".to_owned() + Uuid::new_v4().to_string().as_str()),
            quantity_max: None,
            recurrence: None,
            recurrence_base: None,
            recurrence_limit: None,
            recurrence_paywindow: None,
            single_use,
            amount: params.amount.map_or("any".to_owned(), |a| a.to_string()),
            optional_recurrence: None,
            proportional_amount: None,
        })
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?;

    Ok(nip47::MakeOfferResponse {
        offer: offer.bolt12,
        description: params.description,
        amount: params.amount,
        issuer: params.issuer,
        expires_at: absolute_expiry,
        currency: None,
        currency_minor_unit: None,
        single_use: params.single_use,
    })
}

pub async fn pay_offer_response(
    plugin: Plugin<PluginState>,
    params: nip47::PayOfferRequest,
    label: &str,
) -> Vec<(nip47::Response, Option<String>)> {
    vec![match pay_offer(plugin, params, label).await {
        Ok((o, id)) => (
            nip47::Response {
                result_type: nip47::Method::PayOffer,
                error: None,
                result: Some(nip47::ResponseResult::PayOffer(o)),
            },
            id,
        ),
        Err((e, id)) => (
            nip47::Response {
                result_type: nip47::Method::PayOffer,
                error: Some(e),
                result: None,
            },
            id,
        ),
    }]
}

async fn pay_offer(
    plugin: Plugin<PluginState>,
    params: nip47::PayOfferRequest,
    label: &str,
) -> Result<(nip47::PayOfferResponse, Option<String>), (nip47::NIP47Error, Option<String>)> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let fetch_invoice_response = fetch_invoice(&mut rpc, &params).await?;

    let decoded_bolt12 = decode_bolt12_invoice(&mut rpc, &params, &fetch_invoice_response).await?;

    let id = get_payment_id(&params, &decoded_bolt12)?;

    let invoice_amt_msat = get_invoice_amount_msat(&decoded_bolt12, &params, &id)?;

    let nwc_store =
        load_nwc_and_check_budget(&mut rpc, label, &params, invoice_amt_msat, &id).await?;

    let my_version = plugin.state().config.lock().clone().my_cln_version;
    let use_xpay = check_cln_version(&my_version, &id)?;

    if use_xpay {
        pay_with_xpay_full(
            &mut rpc,
            fetch_invoice_response.invoice,
            label,
            nwc_store,
            &id,
        )
        .await
    } else {
        pay_with_legacy_full(
            &mut rpc,
            fetch_invoice_response.invoice,
            label,
            nwc_store,
            &id,
        )
        .await
    }
}

async fn fetch_invoice(
    rpc: &mut ClnRpc,
    params: &nip47::PayOfferRequest,
) -> Result<FetchinvoiceResponse, (nip47::NIP47Error, Option<String>)> {
    let bolt12_invoice = rpc
        .call_typed(&FetchinvoiceRequest {
            amount_msat: params.amount.map(Amount::from_msat),
            bip353: None,
            payer_metadata: None,
            payer_note: params.payer_note.clone(),
            quantity: None,
            recurrence_counter: None,
            recurrence_label: None,
            recurrence_start: None,
            timeout: None,
            offer: params.offer.clone(),
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
    Ok(bolt12_invoice)
}

async fn decode_bolt12_invoice(
    rpc: &mut ClnRpc,
    params: &nip47::PayOfferRequest,
    fetch_invoice_resp: &FetchinvoiceResponse,
) -> Result<DecodeResponse, (nip47::NIP47Error, Option<String>)> {
    let invoice_decoded = rpc
        .call_typed(&DecodeRequest {
            string: fetch_invoice_resp.invoice.clone(),
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
    Ok(invoice_decoded)
}

fn get_payment_id(
    params: &nip47::PayOfferRequest,
    decoded_offer: &DecodeResponse,
) -> Result<String, (nip47::NIP47Error, Option<String>)> {
    let id = if let Some(i) = &params.id {
        i.clone()
    } else {
        decoded_offer
            .invoice_payment_hash
            .as_ref()
            .ok_or_else(|| {
                (
                    nip47::NIP47Error {
                        code: nip47::ErrorCode::Internal,
                        message: "payment_hash missing in decoded bolt12 invoice".to_owned(),
                    },
                    None,
                )
            })?
            .clone()
    };

    Ok(id)
}

fn get_invoice_amount_msat(
    invoice_decoded: &DecodeResponse,
    params: &nip47::PayOfferRequest,
    id: &str,
) -> Result<u64, (nip47::NIP47Error, Option<String>)> {
    let amt = invoice_decoded
        .invoice_amount_msat
        .as_ref()
        .ok_or_else(|| {
            (
                nip47::NIP47Error {
                    code: nip47::ErrorCode::Internal,
                    message: "Missing amount_msat in decoded bolt12 invoice".to_owned(),
                },
                Some(id.to_owned()),
            )
        })
        .map(Amount::msat)?;
    if let Some(a) = params.amount {
        if amt != a {
            return Err((
                nip47::NIP47Error {
                    code: nip47::ErrorCode::Internal,
                    message: "amount in decoded bolt12 invoice does not match amount in request"
                        .to_owned(),
                },
                Some(id.to_owned()),
            ));
        }
    }
    Ok(amt)
}

async fn load_nwc_and_check_budget(
    rpc: &mut ClnRpc,
    label: &str,
    params: &nip47::PayOfferRequest,
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
) -> Result<(nip47::PayOfferResponse, Option<String>), (nip47::NIP47Error, Option<String>)> {
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
        nip47::PayOfferResponse {
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
    bolt12_invoice: String,
    label: &str,
    mut nwc_store: NwcStore,
    id: &str,
) -> Result<(nip47::PayOfferResponse, Option<String>), (nip47::NIP47Error, Option<String>)> {
    let payment_result = rpc
        .call_typed(&XpayRequest {
            amount_msat: None,
            maxdelay: None,
            maxfee: None,
            partial_msat: None,
            retry_for: None,
            layers: None,
            invstring: bolt12_invoice,
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
    bolt12_invoice: String,
    label: &str,
    mut nwc_store: NwcStore,
    id: &str,
) -> Result<(nip47::PayOfferResponse, Option<String>), (nip47::NIP47Error, Option<String>)> {
    let payment_result = rpc
        .call_typed(&PayRequest {
            amount_msat: None,
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
            bolt11: bolt12_invoice,
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

pub async fn multi_pay_offer(
    plugin: Plugin<PluginState>,
    params: nip47::MultiPayOfferRequest,
    label: &str,
) -> Vec<(nip47::Response, Option<String>)> {
    let mut responses = Vec::new();
    for pay in params.offers {
        let result = pay_offer(plugin.clone(), pay, label).await;
        let response_res = match result {
            Ok((resp, id)) => (
                nip47::Response {
                    result_type: nip47::Method::MultiPayOffer,
                    error: None,
                    result: Some(nip47::ResponseResult::MultiPayOffer(resp)),
                },
                id,
            ),
            Err((e, id)) => (
                nip47::Response {
                    result_type: nip47::Method::MultiPayOffer,
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

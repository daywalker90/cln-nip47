use std::{cmp::Reverse, str::FromStr};

use cln_plugin::Plugin;
use cln_rpc::{
    model::{
        requests::{DecodeRequest, ListinvoicesRequest, ListpaysRequest},
        responses::{
            ListinvoicesInvoices, ListinvoicesInvoicesStatus, ListpaysPays, ListpaysPaysStatus,
        },
    },
    primitives::Sha256,
    ClnRpc,
};
use nostr_sdk::nips::nip47;
use nostr_sdk::Timestamp;

use crate::structs::{PluginState, NOT_INV_ERR};

pub async fn lookup_invoice_response(
    plugin: Plugin<PluginState>,
    params: nip47::LookupInvoiceRequest,
) -> Vec<(nip47::Response, Option<String>)> {
    vec![match lookup_invoice(plugin, params).await {
        Ok(o) => (
            nip47::Response {
                result_type: nip47::Method::LookupInvoice,
                error: None,
                result: Some(nip47::ResponseResult::LookupInvoice(o)),
            },
            None,
        ),
        Err(e) => (
            nip47::Response {
                result_type: nip47::Method::LookupInvoice,
                error: Some(e),
                result: None,
            },
            None,
        ),
    }]
}

async fn lookup_invoice(
    plugin: Plugin<PluginState>,
    params: nip47::LookupInvoiceRequest,
) -> Result<nip47::LookupInvoiceResponse, nip47::NIP47Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    if params.payment_hash.is_none() && params.invoice.is_none() {
        return Err(nip47::NIP47Error {
            code: nip47::ErrorCode::Other,
            message: "Neither invoice nor payment_hash given".to_owned(),
        });
    }

    let invoice = if params.payment_hash.is_some() && params.invoice.is_some() {
        None
    } else {
        params.invoice
    };

    let invoices = rpc
        .call_typed(&ListinvoicesRequest {
            index: None,
            invstring: invoice.clone(),
            label: None,
            limit: None,
            offer_id: None,
            payment_hash: params.payment_hash.clone(),
            start: None,
        })
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?
        .invoices;

    if invoices.len() == 1 {
        let invoice_response = invoices.into_iter().next().unwrap();

        make_lookup_response_from_listinvoices(&mut rpc, invoice_response).await
    } else {
        let payment_hash_hash = if let Some(hash) = params.payment_hash {
            if let Ok(res) = Sha256::from_str(&hash) {
                Some(res)
            } else {
                return Err(nip47::NIP47Error {
                    code: nip47::ErrorCode::Internal,
                    message: "Could not convert payment hash".to_owned(),
                });
            }
        } else {
            None
        };

        let pays = rpc
            .call_typed(&ListpaysRequest {
                bolt11: invoice,
                index: None,
                limit: None,
                payment_hash: payment_hash_hash,
                start: None,
                status: None,
            })
            .await
            .map_err(|e| nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            })?
            .pays;

        if pays.len() != 1 {
            return Err(nip47::NIP47Error {
                code: nip47::ErrorCode::NotFound,
                message: "Transaction not found".to_owned(),
            });
        }
        let list_pay = pays.into_iter().next().unwrap();

        make_lookup_response_from_listpays(&mut rpc, list_pay).await
    }
}

pub async fn list_transactions_response(
    plugin: Plugin<PluginState>,
    params: nip47::ListTransactionsRequest,
) -> Vec<(nip47::Response, Option<String>)> {
    vec![match list_transactions(plugin, params).await {
        Ok(o) => (
            nip47::Response {
                result_type: nip47::Method::ListTransactions,
                error: None,
                result: Some(nip47::ResponseResult::ListTransactions(o)),
            },
            None,
        ),
        Err(e) => (
            nip47::Response {
                result_type: nip47::Method::ListTransactions,
                error: Some(e),
                result: None,
            },
            None,
        ),
    }]
}

async fn list_transactions(
    plugin: Plugin<PluginState>,
    params: nip47::ListTransactionsRequest,
) -> Result<Vec<nip47::LookupInvoiceResponse>, nip47::NIP47Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let (query_invoices, query_payments) = match params.transaction_type {
        Some(t) => match t {
            nip47::TransactionType::Incoming => (true, false),
            nip47::TransactionType::Outgoing => (false, true),
        },
        None => (true, true),
    };

    let unpaid = params.unpaid.unwrap_or(false);

    let mut transactions: Vec<nip47::LookupInvoiceResponse> = Vec::new();
    if query_invoices {
        let list_invoices = rpc
            .call_typed(&ListinvoicesRequest {
                index: None,
                invstring: None,
                label: None,
                limit: None,
                offer_id: None,
                payment_hash: None,
                start: None,
            })
            .await
            .map_err(|e| nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            })?
            .invoices;

        for list_invoice in list_invoices {
            if !unpaid && list_invoice.status == ListinvoicesInvoicesStatus::UNPAID {
                continue;
            }

            match make_lookup_response_from_listinvoices(&mut rpc, list_invoice).await {
                Ok(t) => transactions.push(t),
                Err(_e) => (),
            }
        }
    }

    if query_payments {
        let list_pays = rpc
            .call_typed(&ListpaysRequest {
                bolt11: None,
                index: None,
                limit: None,
                payment_hash: None,
                start: None,
                status: None,
            })
            .await
            .map_err(|e| nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            })?
            .pays;

        for list_pay in list_pays {
            match make_lookup_response_from_listpays(&mut rpc, list_pay).await {
                Ok(t) => transactions.push(t),
                Err(_e) => (),
            }
        }
    }

    transactions.sort_by_key(|t| Reverse(t.created_at));

    if let Some(offset) = params.offset {
        let len = transactions.len() as u64;
        if offset >= len {
            transactions.clear();
        } else {
            let off = usize::try_from(offset).unwrap();
            transactions.drain(0..off);
        }
    }

    if let Some(limit) = params.limit {
        let len = transactions.len() as u64;
        if limit < len {
            let l = usize::try_from(limit).unwrap();
            transactions = transactions.drain(0..l).collect();
        }
    }

    transactions = trim_to_size(transactions, 127 * 1024);

    Ok(transactions)
}

async fn make_lookup_response_from_listinvoices(
    rpc: &mut ClnRpc,
    list_invoice: ListinvoicesInvoices,
) -> Result<nip47::LookupInvoiceResponse, nip47::NIP47Error> {
    let not_invoice_err = Err(nip47::NIP47Error {
        code: nip47::ErrorCode::Other,
        message: NOT_INV_ERR.to_owned(),
    });

    let invstring = if list_invoice.bolt11.is_some() {
        list_invoice.bolt11.as_ref().unwrap()
    } else {
        list_invoice.bolt12.as_ref().unwrap()
    };
    let invoice_decoded = rpc
        .call_typed(&DecodeRequest {
            string: invstring.clone(),
        })
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?;

    if !invoice_decoded.valid {
        return not_invoice_err;
    }

    let description = match invoice_decoded.item_type {
        cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => invoice_decoded.offer_description,
        cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => invoice_decoded.description,
        _ => return not_invoice_err,
    };
    let description_hash = match invoice_decoded.item_type {
        cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => None,
        cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
            invoice_decoded.description_hash.map(|h| h.to_string())
        }
        _ => return not_invoice_err,
    };

    let amount = match invoice_decoded.item_type {
        cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => {
            invoice_decoded.invoice_amount_msat.unwrap().msat()
        }
        cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
            if let Some(amt) = invoice_decoded.amount_msat {
                amt.msat()
            } else if let Some(a) = list_invoice.amount_msat {
                a.msat()
            } else {
                // amount: `any` but have to put a value...
                0
            }
        }
        _ => return not_invoice_err,
    };

    let created_at = match invoice_decoded.item_type {
        cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => {
            Timestamp::from_secs(invoice_decoded.invoice_created_at.unwrap())
        }
        cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
            Timestamp::from_secs(invoice_decoded.created_at.unwrap())
        }
        _ => return not_invoice_err,
    };

    let preimage = list_invoice
        .payment_preimage
        .map(|p| hex::encode(p.to_vec()));

    let state = match list_invoice.status {
        ListinvoicesInvoicesStatus::UNPAID => nip47::TransactionState::Pending,
        ListinvoicesInvoicesStatus::PAID => nip47::TransactionState::Settled,
        ListinvoicesInvoicesStatus::EXPIRED => nip47::TransactionState::Expired,
    };

    Ok(nip47::LookupInvoiceResponse {
        transaction_type: Some(nip47::TransactionType::Incoming),
        invoice: Some(invstring.to_owned()),
        description,
        description_hash,
        preimage,
        payment_hash: list_invoice.payment_hash.to_string(),
        amount,
        fees_paid: 0,
        created_at,
        expires_at: Some(Timestamp::from_secs(list_invoice.expires_at)),
        settled_at: list_invoice.paid_at.map(Timestamp::from_secs),
        metadata: None,
        state: Some(state),
    })
}

async fn make_lookup_response_from_listpays(
    rpc: &mut ClnRpc,
    list_pay: ListpaysPays,
) -> Result<nip47::LookupInvoiceResponse, nip47::NIP47Error> {
    let not_invoice_err = Err(nip47::NIP47Error {
        code: nip47::ErrorCode::Other,
        message: NOT_INV_ERR.to_owned(),
    });

    let invstring = if list_pay.bolt11.is_some() {
        list_pay.bolt11
    } else {
        list_pay.bolt12
    };

    let invoice_decoded = if let Some(invstr) = &invstring {
        Some(
            rpc.call_typed(&DecodeRequest {
                string: invstr.clone(),
            })
            .await
            .map_err(|e| nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            })?,
        )
    } else {
        None
    };

    if invoice_decoded.is_some() && !invoice_decoded.as_ref().unwrap().valid {
        return not_invoice_err;
    }

    let description_hash = if let Some(inv_dec) = &invoice_decoded {
        match inv_dec.item_type {
            cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => None,
            cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
                inv_dec.description_hash.map(|h| h.to_string())
            }
            _ => return not_invoice_err,
        }
    } else {
        None
    };
    let amount = if let Some(amt) = list_pay.amount_msat {
        amt.msat()
    } else if let Some(inv_dec) = &invoice_decoded {
        match inv_dec.item_type {
            cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => {
                inv_dec.invoice_amount_msat.unwrap().msat()
            }
            cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
                if let Some(amt) = inv_dec.amount_msat {
                    amt.msat()
                } else {
                    // amount: `any` but have to put a value...
                    0
                }
            }
            _ => return not_invoice_err,
        }
    } else {
        return not_invoice_err;
    };

    let description = if let Some(inv_dec) = invoice_decoded {
        match inv_dec.item_type {
            cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => inv_dec.offer_description,
            cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => inv_dec.description,
            _ => return not_invoice_err,
        }
    } else {
        list_pay.description
    };

    let fees_paid = if let Some(amt_sent) = list_pay.amount_sent_msat {
        amt_sent.msat() - amount
    } else {
        0
    };
    let preimage = list_pay.preimage.map(|p| hex::encode(p.to_vec()));

    let state = match list_pay.status {
        ListpaysPaysStatus::PENDING => nip47::TransactionState::Pending,
        ListpaysPaysStatus::FAILED => nip47::TransactionState::Failed,
        ListpaysPaysStatus::COMPLETE => nip47::TransactionState::Settled,
    };

    Ok(nip47::LookupInvoiceResponse {
        transaction_type: Some(nip47::TransactionType::Outgoing),
        invoice: invstring,
        description,
        description_hash,
        preimage,
        payment_hash: list_pay.payment_hash.to_string(),
        amount,
        fees_paid,
        created_at: Timestamp::from_secs(list_pay.created_at),
        expires_at: None,
        settled_at: list_pay.completed_at.map(Timestamp::from_secs),
        metadata: None,
        state: Some(state),
    })
}

fn trim_to_size(
    mut transactions: Vec<nip47::LookupInvoiceResponse>,
    max_size: usize,
) -> Vec<nip47::LookupInvoiceResponse> {
    let length_before = transactions.len();
    while !transactions.is_empty() {
        match serde_json::to_vec(&transactions) {
            Ok(serialized) => {
                if serialized.len() <= max_size {
                    log::info!(
                        "Trimmed {} transactions to stay under {}bytes",
                        length_before - transactions.len(),
                        max_size
                    );
                    return transactions;
                }
                transactions.pop();
            }
            Err(e) => {
                log::warn!("Failed to serialize transactions: {e}");
                return transactions;
            }
        }
    }

    Vec::new()
}

use std::{cmp::Reverse, str::FromStr};

use cln_plugin::Plugin;
use cln_rpc::{
    model::{
        requests::{DecodeRequest, ListinvoicesRequest, ListpaysRequest},
        responses::{ListinvoicesInvoicesStatus, ListpaysPaysStatus},
    },
    primitives::Sha256,
};
use nostr_sdk::nips::*;
use nostr_sdk::*;

use crate::structs::PluginState;

pub async fn lookup_invoice(
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

    let not_invoice_err = Err(nip47::NIP47Error {
        code: nip47::ErrorCode::Other,
        message: "Not an invoice or invalid invoice".to_owned(),
    });

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
        let invoice_response = invoices.first().cloned().unwrap();
        let invstring = if invoice_response.bolt11.is_some() {
            invoice_response.bolt11.unwrap()
        } else {
            invoice_response.bolt12.unwrap()
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
            cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => {
                invoice_decoded.offer_description
            }
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
                } else if let Some(a) = invoice_response.amount_msat {
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

        let preimage = invoice_response
            .payment_preimage
            .map(|p| hex::encode(p.to_vec()));

        Ok(nip47::LookupInvoiceResponse {
            transaction_type: Some(nip47::TransactionType::Incoming),
            invoice: Some(invstring),
            description,
            description_hash,
            preimage,
            payment_hash: invoice_response.payment_hash.to_string(),
            amount,
            fees_paid: 0,
            created_at,
            expires_at: Some(Timestamp::from_secs(invoice_response.expires_at)),
            settled_at: invoice_response.paid_at.map(Timestamp::from_secs),
            metadata: None,
        })
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
        let list_pay = pays.first().unwrap().clone();
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
                        return not_invoice_err;
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

        let fees_paid = list_pay.amount_sent_msat.unwrap().msat() - amount;
        let preimage = list_pay.preimage.map(|p| hex::encode(p.to_vec()));

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
        })
    }
}

pub async fn list_transactions(
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

    let from = params.from.map(|f| f.as_u64());
    let until = params.until.map(|f| f.as_u64());
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

        for list_invoice in list_invoices.into_iter() {
            if list_invoice.status == ListinvoicesInvoicesStatus::EXPIRED {
                continue;
            }
            if !unpaid && list_invoice.status == ListinvoicesInvoicesStatus::UNPAID {
                continue;
            }
            let invstring = if list_invoice.bolt11.is_some() {
                list_invoice.bolt11.unwrap()
            } else {
                list_invoice.bolt12.unwrap()
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
                continue;
            }
            let created_at = match invoice_decoded.item_type {
                cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => {
                    Timestamp::from_secs(invoice_decoded.invoice_created_at.unwrap())
                }
                cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
                    Timestamp::from_secs(invoice_decoded.created_at.unwrap())
                }
                _ => continue,
            };
            if let Some(f) = from {
                if created_at.as_u64() < f {
                    continue;
                }
            }
            if let Some(u) = until {
                if created_at.as_u64() > u {
                    continue;
                }
            }
            let description = match invoice_decoded.item_type {
                cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => {
                    invoice_decoded.offer_description
                }
                cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
                    invoice_decoded.description
                }
                _ => continue,
            };
            let description_hash = match invoice_decoded.item_type {
                cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => None,
                cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
                    invoice_decoded.description_hash.map(|h| h.to_string())
                }
                _ => continue,
            };
            let amount = match invoice_decoded.item_type {
                cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => {
                    invoice_decoded.invoice_amount_msat.unwrap().msat()
                }
                cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
                    if let Some(amt) = invoice_decoded.amount_msat {
                        amt.msat()
                    } else {
                        // amount: `any` but have to put a value...
                        0
                    }
                }
                _ => continue,
            };
            let expires_at = match invoice_decoded.item_type {
                cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => {
                    invoice_decoded.invoice_relative_expiry.map(|e_at| {
                        Timestamp::from_secs(
                            invoice_decoded.invoice_created_at.unwrap() + (e_at as u64),
                        )
                    })
                }
                cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => invoice_decoded
                    .expiry
                    .map(|e_at| Timestamp::from_secs(invoice_decoded.created_at.unwrap() + e_at)),
                _ => continue,
            };
            let preimage = list_invoice
                .payment_preimage
                .map(|p| hex::encode(p.to_vec()));

            transactions.push(nip47::LookupInvoiceResponse {
                transaction_type: Some(nip47::TransactionType::Incoming),
                invoice: Some(invstring),
                description,
                description_hash,
                preimage,
                payment_hash: list_invoice.payment_hash.to_string(),
                amount,
                fees_paid: 0,
                created_at,
                expires_at,
                settled_at: list_invoice.paid_at.map(Timestamp::from_secs),
                metadata: None,
            });
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

        for list_pay in list_pays.into_iter() {
            if list_pay.status != ListpaysPaysStatus::COMPLETE {
                continue;
            }
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
                continue;
            }

            let description_hash = if let Some(inv_dec) = &invoice_decoded {
                match inv_dec.item_type {
                    cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => None,
                    cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
                        inv_dec.description_hash.map(|h| h.to_string())
                    }
                    _ => continue,
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
                            continue;
                        }
                    }
                    _ => continue,
                }
            } else {
                continue;
            };

            let description = if let Some(inv_dec) = invoice_decoded {
                match inv_dec.item_type {
                    cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => {
                        inv_dec.offer_description
                    }
                    cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => inv_dec.description,
                    _ => continue,
                }
            } else {
                list_pay.description
            };

            let fees_paid = list_pay.amount_sent_msat.unwrap().msat() - amount;
            let preimage = list_pay.preimage.map(|p| hex::encode(p.to_vec()));

            transactions.push(nip47::LookupInvoiceResponse {
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
            });
        }
    }

    transactions.sort_by_key(|t| Reverse(t.created_at));

    if let Some(l) = params.limit {
        if transactions.len() > (l as usize) {
            transactions = transactions.drain(0..(l as usize)).collect()
        }
    }
    transactions = trim_to_size(transactions, 127 * 1024);

    Ok(transactions)
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
                } else {
                    transactions.pop();
                }
            }
            Err(e) => {
                log::warn!("Failed to serialize transactions: {}", e);
                return transactions;
            }
        }
    }

    Vec::new()
}

use std::{cmp::Reverse, str::FromStr};

use cln_plugin::Plugin;
use cln_rpc::{
    ClnRpc,
    model::{
        requests::{
            DecodeRequest,
            ListinvoicesIndex,
            ListinvoicesRequest,
            ListpaysRequest,
            WaitIndexname,
            WaitRequest,
            WaitSubsystem,
        },
        responses::{
            DecodeResponse,
            DecodeType,
            ListinvoicesInvoices,
            ListinvoicesInvoicesStatus,
            ListpaysPays,
            ListpaysPaysStatus,
        },
    },
    primitives::Sha256,
};
use nostr::{nips::nip47, types::Timestamp};
use tonic::transport::Channel;

use crate::{
    hold::{self, ListRequest, hold_client::HoldClient, list_request::Constraint},
    structs::{NOT_INV_ERR, PluginState},
    util::rpc_socket_path,
};

#[allow(clippy::doc_markdown)]
/// Hard bound on the number of transactions a single `list_transactions`
/// request will fetch, decode and return (DoS protection).
const MAX_TRANSACTIONS: usize = 500;
/// Bound on the size of a `list_transactions` response in bytes.
const RESPONSE_LIMIT_BYTES: usize = 127 * 1024;

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
        params.invoice.clone()
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

        return make_lookup_response_from_listinvoices(&mut rpc, invoice_response).await;
    }

    let payment_hash_hash = if let Some(hash) = &params.payment_hash {
        if let Ok(res) = Sha256::from_str(hash) {
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

    if pays.len() == 1 {
        let list_pay = pays.into_iter().next().unwrap();

        return make_lookup_response_from_listpays(&mut rpc, list_pay).await;
    }

    let holdinvoice_support = plugin.state().hold_client.lock().is_some();

    if holdinvoice_support {
        let mut hold_client = plugin.state().hold_client.lock().clone().unwrap();
        return lookup_holdinvoice(&mut hold_client, &mut rpc, params).await;
    }

    Err(nip47::NIP47Error {
        code: nip47::ErrorCode::NotFound,
        message: "Transaction not found".to_owned(),
    })
}

async fn lookup_holdinvoice(
    hold_client: &mut HoldClient<Channel>,
    rpc: &mut ClnRpc,
    params: nip47::LookupInvoiceRequest,
) -> Result<nip47::LookupInvoiceResponse, nip47::NIP47Error> {
    log::debug!("Looking up hold invoice for params {params:#?}");
    let (hold_invoice, invoice_decoded) =
        get_and_decode_holdinvoice(rpc, hold_client, params).await?;
    let not_invoice_err = Err(nip47::NIP47Error {
        code: nip47::ErrorCode::Other,
        message: NOT_INV_ERR.to_owned(),
    });

    let description = match invoice_decoded.item_type {
        DecodeType::BOLT12_INVOICE => invoice_decoded.offer_description,
        DecodeType::BOLT11_INVOICE => invoice_decoded.description,
        _ => return not_invoice_err,
    };
    let description_hash = match invoice_decoded.item_type {
        DecodeType::BOLT12_INVOICE => None,
        DecodeType::BOLT11_INVOICE => invoice_decoded.description_hash.map(|h| h.to_string()),
        _ => return not_invoice_err,
    };

    let created_at = match invoice_decoded.item_type {
        DecodeType::BOLT12_INVOICE => {
            Timestamp::from_secs(invoice_decoded.invoice_created_at.unwrap())
        }
        DecodeType::BOLT11_INVOICE => Timestamp::from_secs(invoice_decoded.created_at.unwrap()),
        _ => return not_invoice_err,
    };

    let amount = match invoice_decoded.item_type {
        DecodeType::BOLT12_INVOICE => invoice_decoded.invoice_amount_msat.unwrap().msat(),
        DecodeType::BOLT11_INVOICE => {
            if let Some(amt) = invoice_decoded.amount_msat {
                amt.msat()
            } else {
                // amount: `any` but have to put a value...
                0
            }
        }
        _ => return not_invoice_err,
    };

    let expires_at = match invoice_decoded.item_type {
        DecodeType::BOLT12_INVOICE => invoice_decoded
            .invoice_relative_expiry
            .map(|e_at| created_at + Timestamp::from_secs(u64::from(e_at))),
        DecodeType::BOLT11_INVOICE => invoice_decoded
            .expiry
            .map(|e_at| created_at + Timestamp::from_secs(e_at)),
        _ => return not_invoice_err,
    };

    let state = match hold_invoice.state() {
        hold::InvoiceState::Unpaid => nip47::TransactionState::Pending,
        hold::InvoiceState::Accepted => nip47::TransactionState::Accepted,
        hold::InvoiceState::Paid => nip47::TransactionState::Settled,
        hold::InvoiceState::Cancelled => nip47::TransactionState::Expired,
    };

    let settled_at = if hold_invoice.settled_at() != 0 {
        Some(Timestamp::from_secs(hold_invoice.settled_at()))
    } else {
        None
    };

    let preimage = if hold_invoice.state() == hold::InvoiceState::Paid {
        Some(hex::encode(hold_invoice.preimage()))
    } else {
        None
    };

    Ok(nip47::LookupInvoiceResponse {
        transaction_type: Some(nip47::TransactionType::Incoming),
        invoice: Some(hold_invoice.invoice.clone()),
        description,
        description_hash,
        preimage,
        payment_hash: hex::encode(hold_invoice.payment_hash),
        amount,
        fees_paid: 0,
        created_at,
        expires_at,
        settled_at,
        metadata: None,
        state: Some(state),
    })
}

async fn get_and_decode_holdinvoice(
    rpc: &mut ClnRpc,
    hold_client: &mut HoldClient<Channel>,
    params: nip47::LookupInvoiceRequest,
) -> Result<(hold::Invoice, DecodeResponse), nip47::NIP47Error> {
    if let Some(ph) = &params.payment_hash {
        let payment_hash_hash = match hex::decode(ph) {
            Ok(p) => p,
            Err(_e) => {
                return Err(nip47::NIP47Error {
                    code: nip47::ErrorCode::Other,
                    message: "Invalid payment hash".to_owned(),
                });
            }
        };
        let list_request = ListRequest {
            constraint: Some(Constraint::PaymentHash(payment_hash_hash)),
        };
        let hold_lookup = hold_client
            .list(list_request)
            .await
            .map_err(|e| nip47::NIP47Error {
                code: nip47::ErrorCode::Other,
                message: format!("Could not fetch hold invoice: {e}"),
            })?
            .into_inner();

        if hold_lookup.invoices.len() != 1 {
            return Err(nip47::NIP47Error {
                code: nip47::ErrorCode::Other,
                message: "Transaction not found".to_owned(),
            });
        }

        let hold_invoice = hold_lookup.invoices.into_iter().next().unwrap();
        let invoice_decoded = rpc
            .call_typed(&DecodeRequest {
                string: hold_invoice.invoice.clone(),
            })
            .await
            .map_err(|e| nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            })?;

        Ok((hold_invoice, invoice_decoded))
    } else {
        let invoice_decoded = rpc
            .call_typed(&DecodeRequest {
                string: params.invoice.unwrap(),
            })
            .await
            .map_err(|e| nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            })?;
        let ph = match invoice_decoded.item_type {
            DecodeType::BOLT12_INVOICE => invoice_decoded.invoice_payment_hash.unwrap(),
            DecodeType::BOLT11_INVOICE => invoice_decoded.payment_hash.unwrap().to_string(),
            _ => {
                return Err(nip47::NIP47Error {
                    code: nip47::ErrorCode::Other,
                    message: "Not a supported invoice type".to_owned(),
                });
            }
        };
        let payment_hash_hash = match hex::decode(&ph) {
            Ok(p) => p,
            Err(_e) => {
                return Err(nip47::NIP47Error {
                    code: nip47::ErrorCode::Other,
                    message: "Invalid payment hash in invoice".to_owned(),
                });
            }
        };

        let list_request = ListRequest {
            constraint: Some(Constraint::PaymentHash(payment_hash_hash)),
        };

        let hold_lookup = hold_client
            .list(list_request)
            .await
            .map_err(|e| nip47::NIP47Error {
                code: nip47::ErrorCode::Other,
                message: format!("Could not fetch hold invoice: {e}"),
            })?
            .into_inner();

        if hold_lookup.invoices.len() != 1 {
            return Err(nip47::NIP47Error {
                code: nip47::ErrorCode::Other,
                message: "Transaction not found".to_owned(),
            });
        }

        let hold_invoice = hold_lookup.invoices.into_iter().next().unwrap();
        let invoice_decoded = rpc
            .call_typed(&DecodeRequest {
                string: hold_invoice.invoice.clone(),
            })
            .await
            .map_err(|e| nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            })?;
        Ok((hold_invoice, invoice_decoded))
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
    if params.limit == Some(0) {
        return Ok(Vec::new());
    }

    let mut rpc = ClnRpc::new(rpc_socket_path(&plugin))
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: format!("Could not connect to lightningd: {e}"),
        })?;

    let limit = usize::try_from(params.limit.unwrap_or(MAX_TRANSACTIONS as u64)).map_err(|e| {
        nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: format!("32-bit usize limit exceeded: {e}"),
        }
    })?;
    let offset = usize::try_from(params.offset.unwrap_or(0)).map_err(|e| nip47::NIP47Error {
        code: nip47::ErrorCode::Internal,
        message: format!("32-bit usize limit exceeded: {e}"),
    })?;
    let want = limit.saturating_add(offset);
    // Enough candidates from each source to satisfy the requested window even
    // if some get filtered out again by `from`/`until`/`unpaid`.
    let per_source = want
        .saturating_mul(2)
        .saturating_add(16)
        .min(MAX_TRANSACTIONS);
    let from = params.from.map(|t| t.as_secs());
    let until = params.until.map(|t| t.as_secs());
    let unpaid = params.unpaid.unwrap_or(false);

    let (query_invoices, query_payments) = match params.transaction_type {
        Some(t) => match t {
            nip47::TransactionType::Incoming => (true, false),
            nip47::TransactionType::Outgoing => (false, true),
        },
        None => (true, true),
    };

    let mut transactions: Vec<nip47::LookupInvoiceResponse> = Vec::new();

    if query_invoices {
        let invoices = collect_invoices(&mut rpc, per_source, from, until, unpaid).await?;
        transactions.extend(invoices);

        let hold_client = plugin.state().hold_client.lock().clone();
        if let Some(mut hold_client) = hold_client {
            list_holdinvoices_to_transactions(
                &mut hold_client,
                &mut rpc,
                per_source,
                from,
                until,
                &mut transactions,
            )
            .await?;
        }
    }

    if query_payments {
        let pays = collect_pays(&mut rpc, per_source, from, until).await?;
        transactions.extend(pays);
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

    transactions = trim_to_size(transactions, RESPONSE_LIMIT_BYTES);

    Ok(transactions)
}

async fn max_invoice_created_index(rpc: &mut ClnRpc) -> Result<Option<u64>, cln_rpc::RpcError> {
    let current_index = rpc
        .call_typed(&WaitRequest {
            indexname: WaitIndexname::CREATED,
            subsystem: WaitSubsystem::INVOICES,
            nextvalue: 0,
        })
        .await?
        .created
        .ok_or_else(|| cln_rpc::RpcError {
            code: Some(-32700),
            message: "Missing created field in wait response".to_owned(),
            data: None,
        })?;

    if current_index == 0 {
        return Ok(None);
    }
    Ok(Some(current_index))
}

async fn first_invoice_created_index(rpc: &mut ClnRpc) -> Result<u64, cln_rpc::RpcError> {
    let first_index = rpc
        .call_typed(&ListinvoicesRequest {
            label: None,
            invstring: None,
            payment_hash: None,
            offer_id: None,
            index: Some(ListinvoicesIndex::CREATED),
            start: Some(0),
            limit: Some(1),
        })
        .await?
        .invoices
        .first()
        .map_or(0, |i| i.created_index);
    Ok(first_index)
}

/// Collect up to `per_source` most recent paid/public invoices within
/// `[from, until]` by paging backwards through `created_index`, decoding only
/// what is needed. Bounded by `MAX_TRANSACTIONS` regardless of node history.
async fn collect_invoices(
    rpc: &mut ClnRpc,
    per_source: usize,
    from: Option<u64>,
    until: Option<u64>,
    unpaid: bool,
) -> Result<Vec<nip47::LookupInvoiceResponse>, nip47::NIP47Error> {
    const PAGE: u32 = 200;
    const MAX_RAW_ROWS: usize = MAX_TRANSACTIONS * 8;

    let Some(max_idx) = max_invoice_created_index(rpc)
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?
    else {
        return Ok(Vec::new());
    };

    let first_index = first_invoice_created_index(rpc)
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?;

    let mut out = Vec::new();
    let mut raw_seen = 0usize;
    let mut start_excl = max_idx.saturating_add(1);

    loop {
        let page_start = start_excl.saturating_sub(u64::from(PAGE));
        let rows = rpc
            .call_typed(&ListinvoicesRequest {
                index: Some(ListinvoicesIndex::CREATED),
                invstring: None,
                label: None,
                limit: Some(PAGE),
                offer_id: None,
                payment_hash: None,
                start: Some(page_start),
            })
            .await
            .map_err(|e| nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            })?
            .invoices;

        // Process newest first within the page.
        for row in rows.into_iter().rev() {
            raw_seen += 1;
            if raw_seen > MAX_RAW_ROWS {
                return Ok(out);
            }

            if row.created_index >= start_excl {
                continue;
            }

            if !unpaid && row.status == ListinvoicesInvoicesStatus::UNPAID {
                continue;
            }

            match make_lookup_response_from_listinvoices(rpc, row).await {
                Ok(tx) => {
                    let created = tx.created_at.as_secs();
                    if let Some(f) = from {
                        // Newest first, so everything older is also out of range.
                        if created < f {
                            return Ok(out);
                        }
                    }
                    if let Some(u) = until {
                        if created > u {
                            continue;
                        }
                    }
                    out.push(tx);
                    if out.len() >= per_source {
                        return Ok(out);
                    }
                }
                Err(_e) => (),
            }
        }

        if page_start == 0 || page_start < first_index {
            break;
        }
        start_excl = page_start;
    }

    Ok(out)
}

/// Collect up to `per_source` most recent payments within `[from, until]`,
/// decoding only the kept subset. `listpays` returns `created_at` directly, so
/// the whole payment history can be filtered before any decode.
async fn collect_pays(
    rpc: &mut ClnRpc,
    per_source: usize,
    from: Option<u64>,
    until: Option<u64>,
) -> Result<Vec<nip47::LookupInvoiceResponse>, nip47::NIP47Error> {
    let pays = rpc
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

    // Newest first.
    let mut pays = pays;
    pays.sort_by_key(|p| Reverse((p.created_at, p.created_index.unwrap_or(0))));

    let mut out = Vec::new();
    for pay in pays {
        if out.len() >= per_source {
            break;
        }
        if let Some(u) = until {
            if pay.created_at > u {
                continue;
            }
        }
        if let Some(f) = from {
            // Newest first, so the rest are also out of range.
            if pay.created_at < f {
                break;
            }
        }

        match make_lookup_response_from_listpays(rpc, pay).await {
            Ok(tx) => out.push(tx),
            Err(_e) => (),
        }
    }

    Ok(out)
}

async fn list_holdinvoices_to_transactions(
    hold_client: &mut HoldClient<Channel>,
    rpc: &mut ClnRpc,
    per_source: usize,
    from: Option<u64>,
    until: Option<u64>,
    transactions: &mut Vec<nip47::LookupInvoiceResponse>,
) -> Result<(), nip47::NIP47Error> {
    let lookup_request = ListRequest { constraint: None };

    let hold_lookup = hold_client
        .list(lookup_request)
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Other,
            message: format!("Could not fetch hold invoices: {e}"),
        })?
        .into_inner();

    // Decode only the most recent `per_source` hold invoices (newest first).
    let mut hold_invoices = hold_lookup.invoices;
    hold_invoices.sort_by_key(|inv| Reverse(inv.created_at));

    let mut kept = 0usize;
    for hold_invoice in hold_invoices {
        if kept >= per_source {
            break;
        }
        if let Some(u) = until {
            if hold_invoice.created_at > u {
                continue;
            }
        }
        if let Some(f) = from {
            // Newest first, so the rest are also out of range.
            if hold_invoice.created_at < f {
                break;
            }
        }

        match make_lookup_response_from_holdinvoice(rpc, &hold_invoice).await {
            Ok(tx) => {
                transactions.push(tx);
                kept += 1;
            }
            Err(_e) => (),
        }
    }

    Ok(())
}

async fn make_lookup_response_from_holdinvoice(
    rpc: &mut ClnRpc,
    hold_invoice: &hold::Invoice,
) -> Result<nip47::LookupInvoiceResponse, nip47::NIP47Error> {
    let not_invoice_err = Err(nip47::NIP47Error {
        code: nip47::ErrorCode::Other,
        message: NOT_INV_ERR.to_owned(),
    });

    let invoice_decoded = rpc
        .call_typed(&DecodeRequest {
            string: hold_invoice.invoice.clone(),
        })
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?;

    let description = match invoice_decoded.item_type {
        DecodeType::BOLT12_INVOICE => invoice_decoded.offer_description,
        DecodeType::BOLT11_INVOICE => invoice_decoded.description,
        _ => return not_invoice_err,
    };
    let description_hash = match invoice_decoded.item_type {
        DecodeType::BOLT12_INVOICE => None,
        DecodeType::BOLT11_INVOICE => invoice_decoded.description_hash.map(|h| h.to_string()),
        _ => return not_invoice_err,
    };

    let created_at = match invoice_decoded.item_type {
        DecodeType::BOLT12_INVOICE => {
            Timestamp::from_secs(invoice_decoded.invoice_created_at.unwrap())
        }
        DecodeType::BOLT11_INVOICE => Timestamp::from_secs(invoice_decoded.created_at.unwrap()),
        _ => return not_invoice_err,
    };

    let amount = match invoice_decoded.item_type {
        DecodeType::BOLT12_INVOICE => invoice_decoded.invoice_amount_msat.unwrap().msat(),
        DecodeType::BOLT11_INVOICE => {
            if let Some(amt) = invoice_decoded.amount_msat {
                amt.msat()
            } else {
                // amount: `any` but have to put a value...
                0
            }
        }
        _ => return not_invoice_err,
    };

    let expires_at = match invoice_decoded.item_type {
        DecodeType::BOLT12_INVOICE => invoice_decoded
            .invoice_relative_expiry
            .map(|e_at| created_at + Timestamp::from_secs(u64::from(e_at))),
        DecodeType::BOLT11_INVOICE => invoice_decoded
            .expiry
            .map(|e_at| created_at + Timestamp::from_secs(e_at)),
        _ => return not_invoice_err,
    };

    let state = match hold_invoice.state() {
        hold::InvoiceState::Unpaid => nip47::TransactionState::Pending,
        hold::InvoiceState::Accepted => nip47::TransactionState::Accepted,
        hold::InvoiceState::Paid => nip47::TransactionState::Settled,
        hold::InvoiceState::Cancelled => nip47::TransactionState::Expired,
    };

    let settled_at = if hold_invoice.settled_at() != 0 {
        Some(Timestamp::from_secs(hold_invoice.settled_at()))
    } else {
        None
    };

    let preimage = if hold_invoice.state() == hold::InvoiceState::Paid {
        Some(hex::encode(hold_invoice.preimage()))
    } else {
        None
    };

    Ok(nip47::LookupInvoiceResponse {
        transaction_type: Some(nip47::TransactionType::Incoming),
        invoice: Some(hold_invoice.invoice.clone()),
        description,
        description_hash,
        preimage,
        payment_hash: hex::encode(hold_invoice.payment_hash.clone()),
        amount,
        fees_paid: 0,
        created_at,
        expires_at,
        settled_at,
        metadata: None,
        state: Some(state),
    })
}

async fn make_lookup_response_from_listinvoices(
    rpc: &mut ClnRpc,
    list_invoice: ListinvoicesInvoices,
) -> Result<nip47::LookupInvoiceResponse, nip47::NIP47Error> {
    let not_invoice_err = Err(nip47::NIP47Error {
        code: nip47::ErrorCode::Other,
        message: NOT_INV_ERR.to_owned(),
    });

    let invstring = if let Some(bolt11) = list_invoice.bolt11 {
        bolt11
    } else if let Some(bolt12) = list_invoice.bolt12 {
        bolt12
    } else {
        return not_invoice_err;
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
        invoice: Some(invstring),
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

/// Keep only the newest transactions that fit within `max_size` bytes.
/// Transactions must be sorted newest first. Serializes each transaction only
/// once and chunks the remaining (O(n), not O(n^2)).
fn trim_to_size(
    mut transactions: Vec<nip47::LookupInvoiceResponse>,
    max_size: usize,
) -> Vec<nip47::LookupInvoiceResponse> {
    let mut total_size = 0usize;
    let mut keep = 0usize;
    for tx in &transactions {
        match serde_json::to_vec(tx) {
            Ok(serialized) => {
                total_size += serialized.len();
                if total_size > max_size {
                    break;
                }
            }
            Err(e) => {
                log::warn!("Failed to serialize transaction: {e}");
                break;
            }
        }
        keep += 1;
    }

    let trimmed = transactions.len() - keep;
    if trimmed > 0 {
        log::info!("Trimmed {trimmed} transactions to stay under {max_size} bytes");
    }
    transactions.truncate(keep);

    transactions
}

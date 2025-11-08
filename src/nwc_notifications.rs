use std::str::FromStr;

use anyhow::anyhow;
use cln_plugin::Plugin;
use cln_rpc::{
    model::{
        requests::{DecodeRequest, ListinvoicesRequest, ListpaysRequest, ListpeerchannelsRequest},
        responses::{
            DecodeResponse,
            ListinvoicesInvoices,
            ListinvoicesInvoicesStatus,
            ListpaysPays,
            ListpaysPaysStatus,
        },
    },
    primitives::Sha256,
    ClnRpc,
};
use nostr_sdk::{
    nips::nip47,
    nostr::{key::PublicKey, EventBuilder, Kind, Tag},
    Client,
    Timestamp,
};

use crate::{
    hold::{list_request::Constraint, InvoiceState, ListRequest, TrackRequest},
    structs::{PluginState, NOT_INV_ERR},
    OPT_NOTIFICATIONS,
};

pub async fn payment_received_handler(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<(), anyhow::Error> {
    if !plugin.option(&OPT_NOTIFICATIONS).unwrap() {
        return Ok(());
    }
    let label = args
        .get("invoice_payment")
        .ok_or_else(|| anyhow!("Malformed invoice_payment notification: missing invoice_payment"))?
        .get("label")
        .ok_or_else(|| anyhow!("Malformed invoice_payment notification: missing label"))?
        .as_str()
        .ok_or_else(|| anyhow!("label not a string"))?;

    let mut rpc = plugin.state().rpc_lock.lock().await;

    let invoice_resp = rpc
        .call_typed(&ListinvoicesRequest {
            index: None,
            invstring: None,
            label: Some(label.to_owned()),
            limit: None,
            offer_id: None,
            payment_hash: None,
            start: None,
        })
        .await?
        .invoices;

    let invoice = invoice_resp
        .first()
        .ok_or_else(|| anyhow!("invoice not found"))?;
    let invstring = if invoice.bolt11.is_some() {
        invoice.bolt11.as_ref().unwrap()
    } else {
        invoice.bolt12.as_ref().unwrap()
    };

    let invoice_decoded = rpc
        .call_typed(&DecodeRequest {
            string: invstring.clone(),
        })
        .await?;

    let notification =
        make_payment_received_from_listinvoices(invoice, invstring, invoice_decoded)?;

    let clients = plugin.state().handles.lock().await;

    for (client, client_pubkey) in clients.values() {
        send_notification(&notification, client, client_pubkey).await?;
    }

    Ok(())
}

fn make_payment_received_from_listinvoices(
    invoice: &ListinvoicesInvoices,
    invstring: &str,
    invoice_decoded: DecodeResponse,
) -> Result<String, anyhow::Error> {
    let not_invoice_err = Err(anyhow!(NOT_INV_ERR.to_owned()));

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
            } else if let Some(a) = invoice.amount_msat {
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
    let preimage = hex::encode(
        invoice
            .payment_preimage
            .ok_or_else(|| anyhow!("missing preimage from paid invoice"))?
            .to_vec(),
    );
    let settled_at = Timestamp::from_secs(
        invoice
            .paid_at
            .ok_or_else(|| anyhow!("paid invoice missing paid_at time"))?,
    );

    let state = match invoice.status {
        ListinvoicesInvoicesStatus::UNPAID => nip47::TransactionState::Pending,
        ListinvoicesInvoicesStatus::PAID => nip47::TransactionState::Settled,
        ListinvoicesInvoicesStatus::EXPIRED => nip47::TransactionState::Expired,
    };

    let content = nip47::Notification {
        notification_type: nip47::NotificationType::PaymentReceived,
        notification: nip47::NotificationResult::PaymentReceived(nip47::PaymentNotification {
            transaction_type: Some(nip47::TransactionType::Incoming),
            invoice: invstring.to_owned(),
            description: description.clone(),
            description_hash: description_hash.clone(),
            preimage: preimage.clone(),
            payment_hash: invoice.payment_hash.to_string(),
            amount,
            fees_paid: 0,
            created_at,
            expires_at: None,
            settled_at,
            metadata: None,
            state: Some(state),
        }),
    };

    let notification = serde_json::to_string(&content)?;

    Ok(notification)
}

pub async fn payment_sent_handler(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<(), anyhow::Error> {
    if !plugin.option(&OPT_NOTIFICATIONS).unwrap() {
        return Ok(());
    }
    let payment_hash = args
        .get("sendpay_success")
        .ok_or_else(|| anyhow!("Malformed sendpay_success notification: missing sendpay_success"))?
        .get("payment_hash")
        .ok_or_else(|| anyhow!("Malformed sendpay_success notification: missing payment_hash"))?
        .as_str()
        .ok_or_else(|| anyhow!("payment_hash not a string"))?
        .to_owned();

    let mut rpc = plugin.state().rpc_lock.lock().await;

    let mut pays_resp = rpc
        .call_typed(&ListpaysRequest {
            bolt11: None,
            index: None,
            limit: None,
            payment_hash: Some(Sha256::from_str(&payment_hash)?),
            start: None,
            status: None,
        })
        .await?
        .pays;

    pays_resp.retain(|p| p.status == ListpaysPaysStatus::COMPLETE);

    let pay = pays_resp
        .first()
        .ok_or_else(|| anyhow!("complete payment not found"))?;

    let notification = make_payment_sent_from_listpays(pay, &mut rpc).await?;

    let clients = plugin.state().handles.lock().await;

    for (client, client_pubkey) in clients.values() {
        send_notification(&notification, client, client_pubkey).await?;
    }

    Ok(())
}

async fn make_payment_sent_from_listpays(
    pay: &ListpaysPays,
    rpc: &mut ClnRpc,
) -> Result<String, anyhow::Error> {
    let invstring = if let Some(b11) = &pay.bolt11 {
        b11
    } else if let Some(b12) = &pay.bolt12 {
        b12
    } else {
        &String::new()
    };

    let description;
    let description_hash;
    let amount;
    let created_at = Timestamp::from_secs(pay.created_at);
    let preimage = hex::encode(
        pay.preimage
            .ok_or_else(|| anyhow!("missing preimage from paid invoice"))?
            .to_vec(),
    );
    let settled_at = Timestamp::from_secs(pay.completed_at.unwrap());

    if invstring.is_empty() {
        description = pay.description.clone();
        description_hash = None;
        amount = if let Some(amt) = pay.amount_msat {
            amt.msat()
        } else {
            // Amount missing but required
            0
        }
    } else {
        let invoice_decoded = rpc
            .call_typed(&DecodeRequest {
                string: invstring.clone(),
            })
            .await?;

        let not_invoice_err = Err(anyhow!(NOT_INV_ERR.to_owned()));

        if !invoice_decoded.valid {
            return not_invoice_err;
        }

        description = match invoice_decoded.item_type {
            cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => {
                invoice_decoded.offer_description
            }
            cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => invoice_decoded.description,
            _ => return not_invoice_err,
        };
        description_hash = match invoice_decoded.item_type {
            cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => None,
            cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
                invoice_decoded.description_hash.map(|h| h.to_string())
            }
            _ => return not_invoice_err,
        };
        amount = match invoice_decoded.item_type {
            cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => {
                invoice_decoded.invoice_amount_msat.unwrap().msat()
            }
            cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
                if let Some(amt) = invoice_decoded.amount_msat {
                    amt.msat()
                } else if let Some(a) = pay.amount_msat {
                    a.msat()
                } else {
                    // amount: `any` but have to put a value...
                    0
                }
            }
            _ => return not_invoice_err,
        };
    }

    let fees_paid = if let Some(amt_sent) = pay.amount_sent_msat {
        amt_sent.msat() - amount
    } else {
        0
    };

    let state = match pay.status {
        ListpaysPaysStatus::PENDING => nip47::TransactionState::Pending,
        ListpaysPaysStatus::FAILED => nip47::TransactionState::Failed,
        ListpaysPaysStatus::COMPLETE => nip47::TransactionState::Settled,
    };

    let content = nip47::Notification {
        notification_type: nip47::NotificationType::PaymentSent,
        notification: nip47::NotificationResult::PaymentSent(nip47::PaymentNotification {
            transaction_type: Some(nip47::TransactionType::Outgoing),
            invoice: invstring.clone(),
            description: description.clone(),
            description_hash: description_hash.clone(),
            preimage: preimage.clone(),
            payment_hash: pay.payment_hash.to_string(),
            amount,
            fees_paid,
            created_at,
            expires_at: None,
            settled_at,
            metadata: None,
            state: Some(state),
        }),
    };

    let notification = serde_json::to_string(&content)?;

    Ok(notification)
}

async fn send_notification(
    notification: &String,
    client: &Client,
    client_pubkey: &PublicKey,
) -> Result<(), anyhow::Error> {
    let signer = client.signer().await?;
    log::debug!("NOTIFICATION: {notification}");
    let content_encrypted_nip04 = signer.nip04_encrypt(client_pubkey, notification).await?;
    let event_nip04 = EventBuilder::new(Kind::from_u16(23196), content_encrypted_nip04)
        .tag(Tag::public_key(*client_pubkey))
        .sign(&signer)
        .await?;
    let nip04_result = client.send_event(&event_nip04).await?;
    if nip04_result.success.is_empty() {
        log::warn!(
            "None of the relays accepted our nip04 notification: {}",
            nip04_result
                .failed
                .into_values()
                .collect::<Vec<String>>()
                .join(", ")
        );
    }
    log::debug!("NIP04 NOTIFICATION SENT: {event_nip04:?}");

    let content_encrypted_nip44 = signer.nip44_encrypt(client_pubkey, notification).await?;
    let event_nip44 = EventBuilder::new(Kind::from_u16(23197), content_encrypted_nip44)
        .tag(Tag::public_key(*client_pubkey))
        .sign(&signer)
        .await?;
    let nip44_result = client.send_event(&event_nip44).await?;
    if nip44_result.success.is_empty() {
        log::warn!(
            "None of the relays accepted our nip44 notification: {}",
            nip44_result
                .failed
                .into_values()
                .collect::<Vec<String>>()
                .join(", ")
        );
    }
    log::debug!("NIP44 NOTIFICATION SENT: {event_nip44:?}");

    Ok(())
}

pub async fn holdinvoice_accepted_handler(
    plugin: Plugin<PluginState>,
    payment_hash: Vec<u8>,
) -> Result<(), anyhow::Error> {
    let mut hold_client = plugin.state().hold_client.lock().clone().unwrap();

    let track_request = TrackRequest {
        payment_hash: payment_hash.clone(),
    };
    let mut track_stream = hold_client.track(track_request).await?.into_inner();

    while let Some(response) = track_stream.message().await? {
        log::debug!("Invoice status: {}", response.state().as_str_name());
        if response.state() == InvoiceState::Accepted {
            break;
        }
    }

    let list_request = ListRequest {
        constraint: Some(Constraint::PaymentHash(payment_hash.clone())),
    };

    let hold_lookup = hold_client.list(list_request).await?.into_inner();

    if hold_lookup.invoices.len() != 1 {
        return Err(anyhow!("hold plugin did not return exactly one invoice"));
    }

    let hold_invoice = hold_lookup.invoices.first().unwrap();

    let mut rpc = plugin.state().rpc_lock.lock().await;

    let invoice_decoded = rpc
        .call_typed(&DecodeRequest {
            string: hold_invoice.invoice.clone(),
        })
        .await?;

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
        _ => return Err(anyhow!("hold plugin did not return an invoice string")),
    };

    let created_at = match invoice_decoded.item_type {
        cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => {
            Timestamp::from_secs(invoice_decoded.invoice_created_at.unwrap())
        }
        cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
            Timestamp::from_secs(invoice_decoded.created_at.unwrap())
        }
        _ => return Err(anyhow!("hold plugin did not return an invoice string")),
    };

    let expires_at = match invoice_decoded.item_type {
        cln_rpc::model::responses::DecodeType::BOLT12_INVOICE => {
            created_at
                + Timestamp::from_secs(u64::from(invoice_decoded.invoice_relative_expiry.unwrap()))
        }
        cln_rpc::model::responses::DecodeType::BOLT11_INVOICE => {
            created_at + Timestamp::from_secs(invoice_decoded.expiry.unwrap())
        }
        _ => return Err(anyhow!("hold plugin did not return an invoice string")),
    };

    let list_peer_channels = rpc
        .call_typed(&ListpeerchannelsRequest {
            id: None,
            short_channel_id: None,
        })
        .await?
        .channels;

    let payment_hash_hash = Sha256::from_str(&hex::encode(payment_hash))?;

    let mut lowest_htlc_expiry = 0;

    for peer in list_peer_channels {
        if let Some(htlcs) = peer.htlcs {
            for htlc in htlcs {
                if htlc.payment_hash != payment_hash_hash {
                    continue;
                }
                if htlc.expiry < lowest_htlc_expiry {
                    lowest_htlc_expiry = htlc.expiry;
                }
            }
        }
    }

    let clients = plugin.state().handles.lock().await;

    let content = nip47::Notification {
        notification_type: nip47::NotificationType::HoldInvoiceAccepted,
        notification: nip47::NotificationResult::HoldInvoiceAccepted(
            nip47::HoldInvoiceAcceptedNotification {
                transaction_type: nip47::TransactionType::Incoming,
                invoice: hold_invoice.invoice.clone(),
                description: None,
                description_hash: None,
                payment_hash: hex::encode(&hold_invoice.payment_hash),
                amount,
                created_at,
                expires_at,
                settle_deadline: lowest_htlc_expiry,
                metadata: None,
            },
        ),
    };
    let notification = serde_json::to_string(&content).unwrap();

    for (client, client_pubkey) in clients.values() {
        send_notification(&notification, client, client_pubkey).await?;
    }
    Ok(())
}

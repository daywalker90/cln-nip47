use std::str::FromStr;

use anyhow::anyhow;
use cln_plugin::Plugin;
use cln_rpc::{
    ClnRpc,
    Notification,
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
    notifications::{InvoicePaymentNotification, SendPaySuccessNotification},
    primitives::Sha256,
};
use nostr::{
    event::{EventBuilder, FinalizeEventAsync, Kind, Tag},
    nips::{nip04, nip44, nip47},
    types::Timestamp,
};

use crate::{
    OPT_NOTIFICATIONS,
    hold::{InvoiceState, ListRequest, TrackRequest, list_request::Constraint},
    structs::{NOT_INV_ERR, PluginState, WalletService},
};

/// Upper bound on how long a `holdinvoice_accepted_handler` waits for an
/// invoice to be accepted, so a client cannot pin a task and a gRPC stream
/// open forever with an absurd expiry.
const MAX_HOLD_WAIT_SECS: u64 = 24 * 60 * 60;

pub async fn payment_received_handler(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<(), anyhow::Error> {
    if !plugin.option(&OPT_NOTIFICATIONS).unwrap() {
        return Ok(());
    }

    let notif: Notification = serde_json::from_value(args)?;
    let inv_pay_notif: InvoicePaymentNotification = match notif {
        Notification::InvoicePayment(invoice_payment_notification) => invoice_payment_notification,
        _ => return Err(anyhow!("Wrong notification type, expected invoice_payment")),
    };

    let mut rpc = plugin.state().rpc_lock.lock().await;

    let invoice_resp = rpc
        .call_typed(&ListinvoicesRequest {
            index: None,
            invstring: None,
            label: Some(inv_pay_notif.label),
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
    let invstring = if let Some(bolt11) = &invoice.bolt11 {
        bolt11.clone()
    } else if let Some(bolt12) = &invoice.bolt12 {
        bolt12.clone()
    } else {
        return Err(anyhow!(
            "Listinvoices has neither returned bolt11 or bolt12 field"
        ));
    };

    let payment_hash_str = hex::encode(invoice.payment_hash);

    let invoice_decoded = rpc
        .call_typed(&DecodeRequest {
            string: invstring.clone(),
        })
        .await?;

    if !invoice_decoded.valid {
        return Err(anyhow!("Invalid invoice decoded for {payment_hash_str}"));
    }

    let notification =
        make_payment_received_from_listinvoices(invoice, invstring, invoice_decoded)?;

    let clients = plugin.state().handles.lock().await;

    for wallet_service in clients.values() {
        if let Err(e) = send_notification(&notification, wallet_service).await {
            log::warn!(
                "Failed sending payment_received notification for {payment_hash_str} \
                to client {}: {e}",
                wallet_service.client_pubkey
            );
        }
    }

    Ok(())
}

fn make_payment_received_from_listinvoices(
    invoice: &ListinvoicesInvoices,
    invstring: String,
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
            invoice: invstring,
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

    let notif: Notification = serde_json::from_value(args)?;
    let send_pay_notif: SendPaySuccessNotification = match notif {
        Notification::SendPaySuccess(send_pay_success_notification) => {
            send_pay_success_notification
        }
        _ => return Err(anyhow!("Wrong notification type, expected sendpay_success")),
    };
    let payment_hash = hex::encode(send_pay_notif.payment_hash);

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

    for wallet_service in clients.values() {
        if let Err(e) = send_notification(&notification, wallet_service).await {
            log::warn!(
                "Failed sending payment_sent notification for {payment_hash} to\
            client {}: {e}",
                wallet_service.client_pubkey
            );
        }
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
    wallet_service: &WalletService,
) -> Result<(), anyhow::Error> {
    log::trace!("NOTIFICATION: {notification}");
    let content_encrypted_nip04 = nip04::encrypt(
        wallet_service.wallet_secret.secret_key(),
        &wallet_service.client_pubkey,
        notification,
    )?;
    let event_nip04 = EventBuilder::new(Kind::from_u16(23196), content_encrypted_nip04)
        .tag(Tag::public_key(wallet_service.client_pubkey))
        .finalize_async(&wallet_service.wallet_secret)
        .await?;
    let nip04_result = wallet_service.client.send_event(&event_nip04).await?;
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
    log::trace!("NIP04 NOTIFICATION SENT: {event_nip04:?}");

    let content_encrypted_nip44 = nip44::encrypt(
        wallet_service.wallet_secret.secret_key(),
        &wallet_service.client_pubkey,
        notification,
        nip44::Version::V2,
    )?;
    let event_nip44 = EventBuilder::new(Kind::from_u16(23197), content_encrypted_nip44)
        .tag(Tag::public_key(wallet_service.client_pubkey))
        .finalize_async(&wallet_service.wallet_secret)
        .await?;
    let nip44_result = wallet_service.client.send_event(&event_nip44).await?;
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
    log::trace!("NIP44 NOTIFICATION SENT: {event_nip44:?}");

    Ok(())
}

#[allow(clippy::too_many_lines)]
pub async fn holdinvoice_accepted_handler(
    plugin: Plugin<PluginState>,
    payment_hash: Vec<u8>,
    expires_at: u64,
) -> Result<(), anyhow::Error> {
    let payment_hash_str = hex::encode(&payment_hash);

    let mut hold_client = plugin.state().hold_client.lock().clone().unwrap();

    let track_request = TrackRequest {
        payment_hash: payment_hash.clone(),
    };
    let mut track_stream = hold_client.track(track_request).await?.into_inner();

    // The hold plugin has no expired state: an invoice that expires without
    // being accepted stays in the Unpaid state and the track stream never ends
    // on its own. Bound the wait by the invoice's expiry (capped) so we do not
    // hold this task and gRPC stream open forever.
    let wait_duration = expires_at
        .saturating_sub(Timestamp::now().as_secs())
        .min(MAX_HOLD_WAIT_SECS);
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(wait_duration);

    let mut accepted = false;
    loop {
        match tokio::time::timeout_at(deadline, track_stream.message()).await {
            Err(_elapsed) => {
                log::debug!("Hold invoice {payment_hash_str} expired before being accepted");
                break;
            }
            Ok(Err(e)) => return Err(e.into()),
            // The stream ended without an Accepted state: the invoice was
            // cancelled before it ever got accepted.
            Ok(Ok(None)) => break,
            Ok(Ok(Some(response))) => {
                log::debug!("Invoice status: {}", response.state().as_str_name());
                match response.state() {
                    InvoiceState::Accepted => {
                        accepted = true;
                        break;
                    }
                    InvoiceState::Paid | InvoiceState::Cancelled => break,
                    InvoiceState::Unpaid => (),
                }
            }
        }
    }

    if !accepted {
        log::debug!("Hold invoice {payment_hash_str} was not accepted, skipping notification");
        return Ok(());
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
            channel_id: None,
        })
        .await?
        .channels;

    let payment_hash_hash = Sha256::from_str(&payment_hash_str)?;

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
                state: Some(nip47::TransactionState::Accepted),
            },
        ),
    };
    let notification = serde_json::to_string(&content).unwrap();

    for wallet_service in clients.values() {
        if let Err(e) = send_notification(&notification, wallet_service).await {
            log::warn!(
                "Failed sending hold_invoice_accepted {payment_hash_str} notification\
                to client {}: {e}",
                wallet_service.client_pubkey
            );
        }
    }
    Ok(())
}

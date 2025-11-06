use std::str::FromStr;

use anyhow::anyhow;
use cln_plugin::Plugin;
use cln_rpc::model::requests::{DecodeRequest, ListinvoicesRequest, ListpaysRequest};
use cln_rpc::model::responses::{ListinvoicesInvoicesStatus, ListpaysPaysStatus};
use cln_rpc::primitives::Sha256;

use crate::structs::PluginState;
use crate::OPT_NOTIFICATIONS;

use nostr_sdk::nips::nip47;
use nostr_sdk::nostr::EventBuilder;
use nostr_sdk::nostr::Kind;
use nostr_sdk::nostr::Tag;
use nostr_sdk::Timestamp;

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

    let not_invoice_err = Err(anyhow!("Not an invoice or invalid invoice".to_owned()));

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

    let clients = plugin.state().handles.lock().await;

    let state = match invoice.status {
        ListinvoicesInvoicesStatus::UNPAID => nip47::TransactionState::Pending,
        ListinvoicesInvoicesStatus::PAID => nip47::TransactionState::Settled,
        ListinvoicesInvoicesStatus::EXPIRED => nip47::TransactionState::Expired,
    };

    for (client, client_pubkey) in clients.values() {
        let signer = client.signer().await?;
        let content = nip47::Notification {
            notification_type: nip47::NotificationType::PaymentReceived,
            notification: nip47::NotificationResult::PaymentReceived(nip47::PaymentNotification {
                transaction_type: Some(nip47::TransactionType::Incoming),
                invoice: invstring.clone(),
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
        log::debug!("NOTIFICATION: {notification}");
        let content_encrypted_nip04 = signer.nip04_encrypt(client_pubkey, &notification).await?;
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

        let content_encrypted_nip44 = signer.nip44_encrypt(client_pubkey, &notification).await?;
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
    }

    Ok(())
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

    let pays_resp = rpc
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

    let pay = pays_resp
        .first()
        .ok_or_else(|| anyhow!("payment not found"))?;

    if pay.status != ListpaysPaysStatus::COMPLETE {
        return Err(anyhow!("Payment not complete"));
    }

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

        let not_invoice_err = Err(anyhow!("Not an invoice".to_owned()));

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

    let clients = plugin.state().handles.lock().await;

    let state = match pay.status {
        ListpaysPaysStatus::PENDING => nip47::TransactionState::Pending,
        ListpaysPaysStatus::FAILED => nip47::TransactionState::Failed,
        ListpaysPaysStatus::COMPLETE => nip47::TransactionState::Settled,
    };

    for (client, client_pubkey) in clients.values() {
        let signer = client.signer().await?;
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
        log::debug!("NOTIFICATION: {notification}");
        let content_encrypted_nip04 = signer.nip04_encrypt(client_pubkey, &notification).await?;
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

        let content_encrypted_nip44 = signer.nip44_encrypt(client_pubkey, &notification).await?;
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
    }

    Ok(())
}

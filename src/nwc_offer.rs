use std::time::{SystemTime, UNIX_EPOCH};

use crate::structs::PluginState;
use cln_plugin::Plugin;
use cln_rpc::model::requests::{DecodeRequest, OfferRequest};
use nostr_sdk::nips::nip47;
use nostr_sdk::Timestamp;
use uuid::Uuid;

pub async fn lookup_offer(
    plugin: Plugin<PluginState>,
    params: nip47::LookupOfferRequest,
) -> Result<nip47::LookupOfferResponse, nip47::NIP47Error> {
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

    let metadata = decoded_offer.offer_metadata.map(serde_json::Value::String);

    let expires_at = decoded_offer
        .offer_absolute_expiry
        .map(Timestamp::from_secs);

    Ok(nip47::LookupOfferResponse {
        transaction_type: nip47::TransactionType::Incoming,
        offer: Some(params.offer),
        description,
        amount,
        issuer: decoded_offer.offer_issuer,
        expires_at,
        metadata,
    })
}

pub async fn make_offer(
    plugin: Plugin<PluginState>,
    params: nip47::MakeOfferRequest,
) -> Result<nip47::MakeOfferResponse, nip47::NIP47Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let absolute_expiry = params.expiry.map(|e| Timestamp::from_secs(now + e));

    let offer = rpc
        .call_typed(&OfferRequest {
            absolute_expiry: absolute_expiry.map(|e| e.as_u64()),
            description: params.description.clone(),
            issuer: params.issuer.clone(),
            label: Some("NWC offer -".to_owned() + Uuid::new_v4().to_string().as_str()),
            quantity_max: None,
            recurrence: None,
            recurrence_base: None,
            recurrence_limit: None,
            recurrence_paywindow: None,
            recurrence_start_any_period: None,
            single_use: None,
            amount: params
                .amount
                .map(|a| a.to_string())
                .unwrap_or("any".to_owned()),
        })
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?;

    Ok(nip47::MakeOfferResponse {
        offer: offer.bolt12,
        transaction_type: nip47::TransactionType::Incoming,
        description: params.description,
        amount: params.amount,
        issuer: params.issuer,
        expires_at: absolute_expiry,
        metadata: None,
    })
}

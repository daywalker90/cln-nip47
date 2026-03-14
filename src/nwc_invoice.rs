use std::str::FromStr;

use cln_plugin::Plugin;
use cln_rpc::{
    model::requests::InvoiceRequest,
    primitives::{Amount, AmountOrAny, Sha256},
};
use nostr::{nips::nip47, types::Timestamp};
use uuid::Uuid;

use crate::structs::PluginState;

pub async fn make_invoice_response(
    plugin: Plugin<PluginState>,
    params: nip47::MakeInvoiceRequest,
) -> Vec<(nip47::Response, Option<String>)> {
    vec![match make_invoice(plugin, params).await {
        Ok(o) => (
            nip47::Response {
                result_type: nip47::Method::MakeInvoice,
                error: None,
                result: Some(nip47::ResponseResult::MakeInvoice(o)),
            },
            None,
        ),
        Err(e) => (
            nip47::Response {
                result_type: nip47::Method::MakeInvoice,
                error: Some(e),
                result: None,
            },
            None,
        ),
    }]
}

async fn make_invoice(
    plugin: Plugin<PluginState>,
    params: nip47::MakeInvoiceRequest,
) -> Result<nip47::MakeInvoiceResponse, nip47::NIP47Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let mut deschashonly = None;

    if let Some(d_hash) = &params.description_hash {
        if params.description.is_none() {
            return Err(nip47::NIP47Error {
                code: nip47::ErrorCode::Other,
                message: "Must have description when using description_hash".to_owned(),
            });
        }
        let description = params.description.as_ref().unwrap();
        let my_description_hash = Sha256::const_hash(description.as_bytes());
        let description_hash = Sha256::from_str(d_hash).map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?;
        if my_description_hash != description_hash {
            return Err(nip47::NIP47Error {
                code: nip47::ErrorCode::Other,
                message: "description_hash not matching description".to_owned(),
            });
        }
        deschashonly = Some(true);
    }

    let amount_msat = if params.amount == 0 {
        AmountOrAny::Any
    } else {
        AmountOrAny::Amount(Amount::from_msat(params.amount))
    };

    match rpc
        .call_typed(&InvoiceRequest {
            cltv: None,
            deschashonly,
            expiry: params.expiry,
            preimage: None,
            exposeprivatechannels: None,
            fallbacks: None,
            amount_msat,
            description: params
                .description
                .clone()
                .unwrap_or("NWC make_invoice".to_owned()),
            label: Uuid::new_v4().to_string(),
        })
        .await
    {
        Ok(o) => Ok(nip47::MakeInvoiceResponse {
            invoice: o.bolt11,
            payment_hash: Some(o.payment_hash.to_string()),
            description: params.description,
            description_hash: params.description_hash,
            preimage: None,
            amount: Some(params.amount),
            created_at: Some(Timestamp::now()),
            expires_at: Some(Timestamp::from_secs(o.expires_at)),
        }),
        Err(e) => Err(nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        }),
    }
}

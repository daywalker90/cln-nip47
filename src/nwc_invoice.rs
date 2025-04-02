use std::{path::Path, str::FromStr};

use cln_plugin::Plugin;
use cln_rpc::{
    model::requests::InvoiceRequest,
    primitives::{Amount, AmountOrAny, Sha256},
    ClnRpc,
};
use nostr_sdk::nips::*;
use uuid::Uuid;

use crate::structs::PluginState;

pub async fn make_invoice(
    plugin: Plugin<PluginState>,
    params: nip47::MakeInvoiceRequest,
) -> Result<nip47::MakeInvoiceResponse, nip47::NIP47Error> {
    let mut rpc = ClnRpc::new(
        Path::new(&plugin.configuration().lightning_dir).join(&plugin.configuration().rpc_file),
    )
    .await
    .map_err(|e| nip47::NIP47Error {
        code: nip47::ErrorCode::Internal,
        message: e.to_string(),
    })?;

    let mut deschashonly = None;

    if let Some(d_hash) = params.description_hash {
        if params.description.is_none() {
            return Err(nip47::NIP47Error {
                code: nip47::ErrorCode::Other,
                message: "Must have description when using description_hash".to_owned(),
            });
        }
        let description = params.description.as_ref().unwrap();
        let my_description_hash = Sha256::const_hash(description.as_bytes());
        let description_hash = Sha256::from_str(&d_hash).map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?;
        if my_description_hash != description_hash {
            return Err(nip47::NIP47Error {
                code: nip47::ErrorCode::Other,
                message: "description_hash not matching description".to_owned(),
            });
        }
        deschashonly = Some(true)
    }

    match rpc
        .call_typed(&InvoiceRequest {
            cltv: None,
            deschashonly,
            expiry: params.expiry,
            preimage: None,
            exposeprivatechannels: None,
            fallbacks: None,
            amount_msat: AmountOrAny::Amount(Amount::from_msat(params.amount)),
            description: params.description.unwrap_or("NWC make_invoice".to_owned()),
            label: Uuid::new_v4().to_string(),
        })
        .await
    {
        Ok(o) => Ok(nip47::MakeInvoiceResponse {
            invoice: o.bolt11,
            payment_hash: o.payment_hash.to_string(),
        }),
        Err(e) => Err(nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        }),
    }
}

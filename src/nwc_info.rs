use std::{path::Path, str::FromStr};

use cln_plugin::Plugin;
use cln_rpc::{model::requests::GetinfoRequest, ClnRpc};
use nostr_sdk::nips::*;
use nostr_sdk::*;

use crate::structs::PluginState;
use crate::OPT_NOTIFICATIONS;

pub async fn get_info(
    plugin: Plugin<PluginState>,
) -> Result<nip47::GetInfoResponse, nip47::NIP47Error> {
    let mut rpc = ClnRpc::new(
        Path::new(&plugin.configuration().lightning_dir).join(&plugin.configuration().rpc_file),
    )
    .await
    .map_err(|e| nip47::NIP47Error {
        code: nip47::ErrorCode::Internal,
        message: e.to_string(),
    })?;

    let get_info = rpc
        .call_typed(&GetinfoRequest {})
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?;

    let pubkey =
        nostr_sdk::secp256k1::PublicKey::from_str(&get_info.id.to_string()).map_err(|e| {
            nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            }
        })?;

    let network = match get_info.network.as_str() {
        "bitcoin" => "mainnet".to_owned(),
        _ => get_info.network,
    };
    let notifications = if plugin.option(&OPT_NOTIFICATIONS).unwrap() {
        vec!["payment_received".to_owned(), "payment_sent".to_owned()]
    } else {
        vec![]
    };

    Ok(nip47::GetInfoResponse {
        alias: get_info.alias,
        color: Some(get_info.color),
        pubkey: Some(pubkey),
        network: Some(network),
        block_height: Some(get_info.blockheight),
        block_hash: None,
        methods: vec![
            "pay_invoice".to_owned(),
            "multi_pay_invoice".to_owned(),
            "pay_keysend".to_owned(),
            "multi_pay_keysend".to_owned(),
            "make_invoice".to_owned(),
            "lookup_invoice".to_owned(),
            "list_transactions".to_owned(),
            "get_balance".to_owned(),
            "get_info".to_owned(),
        ],
        notifications,
    })
}

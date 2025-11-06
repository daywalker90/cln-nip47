use cln_plugin::Plugin;
use cln_rpc::model::requests::GetinfoRequest;

use crate::structs::PluginState;
use crate::util::{is_read_only_nwc, load_nwc_store};
use crate::{OPT_NOTIFICATIONS, WALLET_ALL_METHODS, WALLET_READ_METHODS};
use nostr_sdk::nips::nip47;

pub async fn get_info(
    plugin: Plugin<PluginState>,
    label: &String,
) -> Result<nip47::GetInfoResponse, nip47::NIP47Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let get_info = rpc
        .call_typed(&GetinfoRequest {})
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?;

    let pubkey = get_info.id.to_string();

    let network = match get_info.network.as_str() {
        "bitcoin" => "mainnet".to_owned(),
        _ => get_info.network,
    };
    let notifications = if plugin.option(&OPT_NOTIFICATIONS).unwrap() {
        vec!["payment_received".to_owned(), "payment_sent".to_owned()]
    } else {
        vec![]
    };

    let nwc_store = load_nwc_store(&mut rpc, label)
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?;

    let methods = if is_read_only_nwc(&nwc_store) {
        WALLET_READ_METHODS.to_vec()
    } else {
        WALLET_ALL_METHODS.to_vec()
    };

    Ok(nip47::GetInfoResponse {
        alias: get_info.alias,
        color: Some(get_info.color),
        pubkey: Some(pubkey),
        network: Some(network),
        block_height: Some(get_info.blockheight),
        block_hash: None,
        methods,
        notifications,
    })
}

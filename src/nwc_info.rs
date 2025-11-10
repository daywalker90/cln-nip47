use cln_plugin::Plugin;
use cln_rpc::model::requests::GetinfoRequest;
use nostr_sdk::nips::nip47;

use crate::{
    structs::PluginState,
    util::{build_methods_vec, build_notifications_vec, is_read_only_nwc, load_nwc_store},
};

pub async fn get_info_response(
    plugin: Plugin<PluginState>,
    label: &str,
) -> Vec<(nip47::Response, Option<String>)> {
    vec![match get_info(plugin, label).await {
        Ok(o) => (
            nip47::Response {
                result_type: nip47::Method::GetInfo,
                error: None,
                result: Some(nip47::ResponseResult::GetInfo(o)),
            },
            None,
        ),
        Err(e) => (
            nip47::Response {
                result_type: nip47::Method::GetInfo,
                error: Some(e),
                result: None,
            },
            None,
        ),
    }]
}

async fn get_info(
    plugin: Plugin<PluginState>,
    label: &str,
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

    let nwc_store = load_nwc_store(&mut rpc, label)
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?;

    let notifications = build_notifications_vec(&plugin);

    let methods = build_methods_vec(is_read_only_nwc(&nwc_store), &plugin);

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

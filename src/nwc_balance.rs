use cln_plugin::Plugin;
use cln_rpc::{model::requests::ListpeerchannelsRequest, primitives::ChannelState};
use nostr_sdk::nips::nip47;

use crate::{structs::PluginState, util::load_nwc_store};

pub async fn get_balance(
    plugin: Plugin<PluginState>,
    label: &str,
) -> Result<nip47::GetBalanceResponse, nip47::NIP47Error> {
    let mut rpc = plugin.state().rpc_lock.lock().await;

    let nwc_store = load_nwc_store(&mut rpc, label)
        .await
        .map_err(|e| nip47::NIP47Error {
            code: nip47::ErrorCode::Internal,
            message: e.to_string(),
        })?;

    let balance = if let Some(bdgt_amt) = nwc_store.budget_msat {
        bdgt_amt
    } else {
        let listpeerchannels = rpc
            .call_typed(&ListpeerchannelsRequest { id: None })
            .await
            .map_err(|e| nip47::NIP47Error {
                code: nip47::ErrorCode::Internal,
                message: e.to_string(),
            })?;

        let mut amount_msat = 0;
        for chan in listpeerchannels.channels {
            if chan.state == ChannelState::CHANNELD_NORMAL
                || chan.state == ChannelState::CHANNELD_AWAITING_SPLICE
            {
                if let Some(spend) = chan.spendable_msat {
                    amount_msat += spend.msat();
                }
            }
        }
        amount_msat
    };
    Ok(nip47::GetBalanceResponse { balance })
}

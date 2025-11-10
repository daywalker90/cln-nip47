use cln_plugin::Plugin;
use nostr_sdk::nips::nip47;

use crate::structs::PluginState;

pub async fn make_hold_invoice_response(
    _plugin: Plugin<PluginState>,
    _params: nip47::MakeHoldInvoiceRequest,
    _label: &str,
) -> Vec<(nip47::Response, Option<String>)> {
    vec![(
        nip47::Response {
            result_type: nip47::Method::MakeHoldInvoice,
            error: Some(nip47::NIP47Error {
                code: nip47::ErrorCode::NotImplemented,
                message: "Not implemented".to_owned(),
            }),
            result: None,
        },
        None,
    )]
}

pub async fn cancel_hold_invoice_response(
    _plugin: Plugin<PluginState>,
    _params: nip47::CancelHoldInvoiceRequest,
    _label: &str,
) -> Vec<(nip47::Response, Option<String>)> {
    vec![(
        nip47::Response {
            result_type: nip47::Method::MakeHoldInvoice,
            error: Some(nip47::NIP47Error {
                code: nip47::ErrorCode::NotImplemented,
                message: "Not implemented".to_owned(),
            }),
            result: None,
        },
        None,
    )]
}

pub async fn settle_hold_invoice_response(
    _plugin: Plugin<PluginState>,
    _params: nip47::SettleHoldInvoiceRequest,
    _label: &str,
) -> Vec<(nip47::Response, Option<String>)> {
    vec![(
        nip47::Response {
            result_type: nip47::Method::MakeHoldInvoice,
            error: Some(nip47::NIP47Error {
                code: nip47::ErrorCode::NotImplemented,
                message: "Not implemented".to_owned(),
            }),
            result: None,
        },
        None,
    )]
}

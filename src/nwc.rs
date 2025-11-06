use std::time::Duration;

use crate::nwc_balance::get_balance;
use crate::nwc_info::get_info;
use crate::nwc_invoice::make_invoice;
use crate::nwc_keysend::{multi_pay_keysend, pay_keysend};
use crate::nwc_lookups::{list_transactions, lookup_invoice};
use crate::nwc_pay::{multi_pay_invoice, pay_invoice};
use crate::structs::{NwcStore, PluginState};
use crate::tasks::budget_task;
use crate::util::is_read_only_nwc;
use crate::{OPT_NOTIFICATIONS, WALLET_ALL_METHODS, WALLET_READ_METHODS};
use anyhow::anyhow;
use cln_plugin::Plugin;
use nostr_sdk::client;
use nostr_sdk::nips::nip04;
use nostr_sdk::nips::nip44;
use nostr_sdk::nips::nip47;
use nostr_sdk::nostr::Filter;
use nostr_sdk::nostr::Kind;
use nostr_sdk::nostr::Tag;
use nostr_sdk::Alphabet;
use nostr_sdk::Client;
use nostr_sdk::EventBuilder;
use nostr_sdk::Keys;
use nostr_sdk::PublicKey;
use nostr_sdk::RelayPoolNotification;
use nostr_sdk::RelayStatus;
use nostr_sdk::SecretKey;
use nostr_sdk::SignerError;
use nostr_sdk::SingleLetterTag;
use nostr_sdk::TagKind;
use nostr_sdk::Timestamp;
use tokio::sync::oneshot;
use tokio::time;

pub async fn run_nwc(
    plugin: Plugin<PluginState>,
    label: String,
    nwc_store: NwcStore,
) -> Result<(), client::Error> {
    let capabilities = if is_read_only_nwc(&nwc_store) {
        WALLET_READ_METHODS.map(|c| c.as_str().to_owned()).join(" ")
    } else {
        WALLET_ALL_METHODS.map(|c| c.as_str().to_owned()).join(" ")
    };

    let wallet_keys = Keys::new(
        SecretKey::from_hex(&nwc_store.walletkey)
            .map_err(|e| client::Error::Signer(SignerError::backend(e)))?,
    );
    let client_pubkey = Keys::new(nwc_store.uri.secret.clone()).public_key();

    let client = Client::new(wallet_keys.clone());

    log::debug!("relay_count:{}", nwc_store.uri.relays.len());

    for relay in &nwc_store.uri.relays {
        log::debug!("Adding relay: {relay}");
        client.add_relay(relay).await?;
    }

    if nwc_store.interval_config.is_some() {
        start_nwc_budget_job(&plugin, label.clone());
    }

    let client_clone = client.clone();
    let plugin_clone = plugin.clone();
    let label_clone = label.clone();
    tokio::spawn(async move {
        loop {
            client_clone.connect().await;
            client_clone
                .wait_for_connection(Duration::from_secs(30))
                .await;
            let relays = client_clone.relays().await;
            if relays.is_empty() {
                log::info!("No more relays left, we probably shut down. Exiting...");
                break;
            }
            let mut connected = false;
            for (url, relay) in relays {
                if relay.status() == RelayStatus::Connected {
                    connected = true;
                } else {
                    log::info!("Could not connect to {url}");
                }
            }
            if !connected {
                log::warn!("Could not connect to any relays!");
                time::sleep(Duration::from_secs(5)).await;
                continue;
            }

            if let Err(e) = send_nwc_info_event(
                client_clone.clone(),
                plugin_clone.option(&OPT_NOTIFICATIONS).unwrap(),
                capabilities.clone(),
                wallet_keys.clone(),
            )
            .await
            {
                log::warn!("{e}");
                client_clone.disconnect().await;
                time::sleep(Duration::from_secs(5)).await;
                continue;
            }

            let filter = Filter::new()
                .kind(Kind::WalletConnectRequest)
                .author(client_pubkey);

            if let Err(e) = client_clone.subscribe(filter, None).await {
                log::warn!("Could not subscribe to nwc events! {e}");
                client_clone.disconnect().await;
                time::sleep(Duration::from_secs(5)).await;
                continue;
            }

            let client_clone_handler = client_clone.clone();
            match client_clone
                .handle_notifications(|notification| {
                    let client_clone_handler = client_clone_handler.clone();
                    let plugin_clone = plugin_clone.clone();
                    let label_clone = label_clone.clone();
                    let wallet_keys_clone = wallet_keys.clone();
                    nwc_request_handler(
                        notification,
                        client_clone_handler,
                        plugin_clone,
                        label_clone,
                        wallet_keys_clone,
                        client_pubkey,
                    )
                })
                .await
            {
                Ok(()) => {
                    log::info!("NWC handler for `{label_clone}` stopped");
                    break;
                }
                Err(e) => log::warn!("NWC handler for `{label_clone}` had an error: {e}"),
            };
        }
    });

    let mut locked_handles = plugin.state().handles.lock().await;
    locked_handles.insert(
        label.clone(),
        (client, Keys::new(nwc_store.uri.secret).public_key()),
    );
    Ok(())
}

pub async fn send_nwc_info_event(
    client: Client,
    notifications: bool,
    capabilities: String,
    wallet_keys: Keys,
) -> Result<(), anyhow::Error> {
    let mut info_event_builder = EventBuilder::new(Kind::WalletConnectInfo, capabilities.clone())
        .tag(Tag::parse(vec!["encryption", "nip44_v2 nip04"]).unwrap());

    if notifications {
        info_event_builder = info_event_builder
            .tag(Tag::parse(vec!["notifications", "payment_received payment_sent"]).unwrap());
    }

    let info_event = match info_event_builder.sign_with_keys(&wallet_keys) {
        Ok(o) => o,
        Err(e) => {
            return Err(anyhow!("Could not sign info_event! {e}"));
        }
    };
    log::debug!("info_event:{info_event:?}");
    let send_result = match client.send_event(&info_event).await {
        Ok(o) => o,
        Err(e) => {
            return Err(anyhow!("Could not send info_event! {e}"));
        }
    };
    if send_result.success.is_empty() {
        return Err(anyhow!(
            "None of the relays received the info_event! {}",
            send_result
                .failed
                .into_values()
                .collect::<Vec<String>>()
                .join(", ")
        ));
    }
    Ok(())
}

pub async fn stop_nwc(plugin: Plugin<PluginState>, label: &String) {
    let mut locked_handles = plugin.state().handles.lock().await;
    if let Some((client, _client_pubkey)) = locked_handles.remove(label) {
        client.shutdown().await;
    }

    stop_nwc_budget_job(&plugin, label);
}

pub fn start_nwc_budget_job(plugin: &Plugin<PluginState>, label: String) {
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(budget_task(rx, plugin.clone(), label.clone()));
    plugin.state().budget_jobs.lock().insert(label, tx);
}

pub fn stop_nwc_budget_job(plugin: &Plugin<PluginState>, label: &String) {
    let mut budget_jobs = plugin.state().budget_jobs.lock();
    let job = budget_jobs.remove(label);
    if let Some(j) = job {
        let _ = j.send(());
    }
}

async fn nwc_request_handler(
    notification: RelayPoolNotification,
    client: client::Client,
    plugin: Plugin<PluginState>,
    label: String,
    wallet_keys: Keys,
    client_pubkey: PublicKey,
) -> Result<bool, Box<dyn std::error::Error>> {
    let (relay_url, subscription_id, event) = match notification {
        RelayPoolNotification::Event {
            relay_url,
            subscription_id,
            event,
        } => (relay_url, subscription_id, event),
        RelayPoolNotification::Message {
            relay_url: _,
            message: _,
        } => return Ok(false),
        RelayPoolNotification::Shutdown => return Ok(true),
    };

    if let Some(expi) = event.tags.expiration() {
        if *expi < Timestamp::now() {
            return Ok(false);
        }
    }
    log::debug!("relay_url:{relay_url} subscription_id:{subscription_id} {event:?}");
    let use_nip44;
    let content = match nip44::decrypt(wallet_keys.secret_key(), &client_pubkey, &event.content) {
        Ok(o) => {
            use_nip44 = true;
            o
        }
        Err(e) => {
            log::debug!("Could not decrypt using NIP-44:{e}. Trying NIP-04");
            match nip04::decrypt(wallet_keys.secret_key(), &client_pubkey, &event.content) {
                Ok(o) => {
                    use_nip44 = false;
                    o
                }
                Err(e) => {
                    log::warn!("Could not decrypt using NIP-04 or NIP-44:{e}");
                    return Ok(false);
                }
            }
        }
    };
    log::debug!("Decrypted:{content}");
    let request: nip47::Request = match serde_json::from_str(&content) {
        Ok(o) => o,
        Err(e) => {
            log::warn!("Error parsing nip47::Request! {e}");
            return Ok(false);
        }
    };

    let responses = match request.params {
        nip47::RequestParams::PayInvoice(pay_invoice_request) => {
            vec![
                match pay_invoice(plugin.clone(), pay_invoice_request, &label).await {
                    Ok((o, id)) => (
                        nip47::Response {
                            result_type: nip47::Method::PayInvoice,
                            error: None,
                            result: Some(nip47::ResponseResult::PayInvoice(o)),
                        },
                        id,
                    ),
                    Err((e, id)) => (
                        nip47::Response {
                            result_type: nip47::Method::PayInvoice,
                            error: Some(e),
                            result: None,
                        },
                        id,
                    ),
                },
            ]
        }
        nip47::RequestParams::MultiPayInvoice(multi_pay_invoice_request) => {
            multi_pay_invoice(plugin.clone(), multi_pay_invoice_request, &label).await
        }
        nip47::RequestParams::PayKeysend(pay_keysend_request) => {
            let id = if let Some(i) = pay_keysend_request.id.clone() {
                i
            } else {
                pay_keysend_request.pubkey.clone()
            };
            vec![
                match pay_keysend(plugin.clone(), pay_keysend_request, &label).await {
                    Ok(o) => (
                        nip47::Response {
                            result_type: nip47::Method::PayKeysend,
                            error: None,
                            result: Some(nip47::ResponseResult::PayKeysend(o)),
                        },
                        id,
                    ),
                    Err(e) => (
                        nip47::Response {
                            result_type: nip47::Method::PayKeysend,
                            error: Some(e),
                            result: None,
                        },
                        id,
                    ),
                },
            ]
        }
        nip47::RequestParams::MultiPayKeysend(multi_pay_keysend_request) => {
            multi_pay_keysend(plugin.clone(), multi_pay_keysend_request, &label).await
        }
        nip47::RequestParams::MakeInvoice(make_invoice_request) => {
            vec![
                match make_invoice(plugin.clone(), make_invoice_request).await {
                    Ok(o) => (
                        nip47::Response {
                            result_type: nip47::Method::MakeInvoice,
                            error: None,
                            result: Some(nip47::ResponseResult::MakeInvoice(o)),
                        },
                        String::new(),
                    ),
                    Err(e) => (
                        nip47::Response {
                            result_type: nip47::Method::MakeInvoice,
                            error: Some(e),
                            result: None,
                        },
                        String::new(),
                    ),
                },
            ]
        }
        nip47::RequestParams::LookupInvoice(lookup_invoice_request) => {
            vec![
                match lookup_invoice(plugin.clone(), lookup_invoice_request).await {
                    Ok(o) => (
                        nip47::Response {
                            result_type: nip47::Method::LookupInvoice,
                            error: None,
                            result: Some(nip47::ResponseResult::LookupInvoice(o)),
                        },
                        String::new(),
                    ),
                    Err(e) => (
                        nip47::Response {
                            result_type: nip47::Method::LookupInvoice,
                            error: Some(e),
                            result: None,
                        },
                        String::new(),
                    ),
                },
            ]
        }
        nip47::RequestParams::ListTransactions(list_transactions_request) => {
            vec![
                match list_transactions(plugin.clone(), list_transactions_request).await {
                    Ok(o) => (
                        nip47::Response {
                            result_type: nip47::Method::ListTransactions,
                            error: None,
                            result: Some(nip47::ResponseResult::ListTransactions(o)),
                        },
                        String::new(),
                    ),
                    Err(e) => (
                        nip47::Response {
                            result_type: nip47::Method::ListTransactions,
                            error: Some(e),
                            result: None,
                        },
                        String::new(),
                    ),
                },
            ]
        }
        nip47::RequestParams::GetBalance => {
            vec![match get_balance(plugin.clone(), &label).await {
                Ok(o) => (
                    nip47::Response {
                        result_type: nip47::Method::GetBalance,
                        error: None,
                        result: Some(nip47::ResponseResult::GetBalance(o)),
                    },
                    String::new(),
                ),
                Err(e) => (
                    nip47::Response {
                        result_type: nip47::Method::GetBalance,
                        error: Some(e),
                        result: None,
                    },
                    String::new(),
                ),
            }]
        }
        nip47::RequestParams::GetInfo => {
            vec![match get_info(plugin.clone(), &label).await {
                Ok(o) => (
                    nip47::Response {
                        result_type: nip47::Method::GetInfo,
                        error: None,
                        result: Some(nip47::ResponseResult::GetInfo(o)),
                    },
                    String::new(),
                ),
                Err(e) => (
                    nip47::Response {
                        result_type: nip47::Method::GetInfo,
                        error: Some(e),
                        result: None,
                    },
                    String::new(),
                ),
            }]
        }
        nip47::RequestParams::MakeHoldInvoice(_make_hold_invoice_request) => {
            vec![(
                nip47::Response {
                    result_type: nip47::Method::MakeHoldInvoice,
                    error: Some(nip47::NIP47Error {
                        code: nip47::ErrorCode::NotImplemented,
                        message: "Not implemented".to_owned(),
                    }),
                    result: None,
                },
                String::new(),
            )]
        }
        nip47::RequestParams::CancelHoldInvoice(_cancel_hold_invoice_request) => {
            vec![(
                nip47::Response {
                    result_type: nip47::Method::CancelHoldInvoice,
                    error: Some(nip47::NIP47Error {
                        code: nip47::ErrorCode::NotImplemented,
                        message: "Not implemented".to_owned(),
                    }),
                    result: None,
                },
                String::new(),
            )]
        }
        nip47::RequestParams::SettleHoldInvoice(_settle_hold_invoice_request) => {
            vec![(
                nip47::Response {
                    result_type: nip47::Method::SettleHoldInvoice,
                    error: Some(nip47::NIP47Error {
                        code: nip47::ErrorCode::NotImplemented,
                        message: "Not implemented".to_owned(),
                    }),
                    result: None,
                },
                String::new(),
            )]
        }
    };
    for (response, id) in responses {
        let response_str = match serde_json::to_string(&response) {
            Ok(o) => o,
            Err(e) => {
                log::warn!("Error serializing response! {e}");
                continue;
            }
        };
        log::debug!("RESPONSE:{response_str}");

        let content = if use_nip44 {
            match nip44::encrypt(
                wallet_keys.secret_key(),
                &client_pubkey,
                response_str,
                nip44::Version::V2,
            ) {
                Ok(o) => o,
                Err(e) => {
                    log::warn!("Error encrypting response with nip44! {e}");
                    continue;
                }
            }
        } else {
            match nip04::encrypt(wallet_keys.secret_key(), &client_pubkey, response_str) {
                Ok(o) => o,
                Err(e) => {
                    log::warn!("Error encrypting response with nip04! {e}");
                    continue;
                }
            }
        };
        let mut response_builder = EventBuilder::new(Kind::WalletConnectResponse, content)
            .tag(Tag::event(event.id))
            .tag(Tag::public_key(client_pubkey));
        if !id.is_empty() {
            response_builder = response_builder.tag(Tag::custom(
                TagKind::SingleLetter(SingleLetterTag {
                    character: Alphabet::D,
                    uppercase: false,
                }),
                vec![id],
            ));
        }
        let response_event = match response_builder.sign_with_keys(&wallet_keys) {
            Ok(o) => o,
            Err(e) => {
                log::warn!("Error signing reponse event! {e}");
                continue;
            }
        };
        let send_result = match client.send_event(&response_event).await {
            Ok(o) => o,
            Err(e) => {
                log::warn!("Error sending response event! {e}");
                continue;
            }
        };
        if send_result.success.is_empty() {
            log::warn!(
                "None of the relays accepted our nwc response: {}",
                send_result
                    .failed
                    .into_values()
                    .collect::<Vec<String>>()
                    .join(", ")
            );
            continue;
        }
        log::debug!("SENT RESPONSE {response_event:?}");
    }

    Ok(false)
}

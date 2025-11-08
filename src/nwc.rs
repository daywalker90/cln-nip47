use std::{borrow::Cow, time::Duration};

use anyhow::anyhow;
use cln_plugin::Plugin;
use nostr_sdk::{
    client,
    nips::{nip04, nip44, nip47},
    nostr::{Filter, Kind, Tag},
    Alphabet,
    Client,
    Event,
    EventBuilder,
    EventId,
    Keys,
    PublicKey,
    RelayPoolNotification,
    RelayStatus,
    SecretKey,
    SignerError,
    SingleLetterTag,
    TagKind,
    Timestamp,
};
use tokio::{sync::oneshot, time};

use crate::{
    nwc_balance::get_balance_response,
    nwc_hold::{
        cancel_hold_invoice_response,
        make_hold_invoice_response,
        settle_hold_invoice_response,
    },
    nwc_info::get_info_response,
    nwc_invoice::make_invoice_response,
    nwc_keysend::{multi_pay_keysend, pay_keysend_response},
    nwc_lookups::{list_transactions_response, lookup_invoice_response},
    nwc_pay::{multi_pay_invoice, pay_invoice_response},
    structs::{NwcStore, PluginState},
    tasks::budget_task,
    util::{build_capabilities, build_notifications_vec, is_read_only_nwc},
    OPT_NOTIFICATIONS,
    STARTUP_DELAY,
};

pub async fn run_nwc(
    plugin: Plugin<PluginState>,
    label: String,
    nwc_store: NwcStore,
) -> Result<(), client::Error> {
    let (method_capabilities, _) = build_capabilities(is_read_only_nwc(&nwc_store), &plugin);

    let wallet_keys = Keys::new(
        SecretKey::from_hex(&nwc_store.walletkey)
            .map_err(|e| client::Error::Signer(SignerError::backend(e)))?,
    );
    let client_pubkey = Keys::new(nwc_store.uri.secret.clone()).public_key();

    let nostr_client = Client::new(wallet_keys.clone());

    log::debug!("relay_count:{}", nwc_store.uri.relays.len());

    for relay in &nwc_store.uri.relays {
        log::debug!("Adding relay: {relay}");
        nostr_client.add_relay(relay).await?;
    }

    if nwc_store.interval_config.is_some() {
        start_nwc_budget_job(&plugin, label.clone());
    }

    let nostr_client_clone = nostr_client.clone();
    let plugin_clone = plugin.clone();
    let label_clone = label.clone();
    tokio::spawn(async move {
        loop {
            nostr_client_clone.connect().await;
            nostr_client_clone
                .wait_for_connection(Duration::from_secs(30))
                .await;
            let relays = nostr_client_clone.relays().await;
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
                plugin_clone.clone(),
                nostr_client_clone.clone(),
                method_capabilities.clone(),
                wallet_keys.clone(),
            )
            .await
            {
                log::warn!("{e}");
                nostr_client_clone.disconnect().await;
                time::sleep(Duration::from_secs(5)).await;
                continue;
            }

            let filter = Filter::new()
                .kind(Kind::WalletConnectRequest)
                .author(client_pubkey)
                .since(Timestamp::now() - STARTUP_DELAY - 1);

            if let Err(e) = nostr_client_clone.subscribe(filter, None).await {
                log::warn!("Could not subscribe to nwc events! {e}");
                nostr_client_clone.disconnect().await;
                time::sleep(Duration::from_secs(5)).await;
                continue;
            }

            let client_clone_handler = nostr_client_clone.clone();
            match nostr_client_clone
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
        (nostr_client, Keys::new(nwc_store.uri.secret).public_key()),
    );
    Ok(())
}

pub async fn send_nwc_info_event(
    plugin: Plugin<PluginState>,
    client: Client,
    capabilities: String,
    wallet_keys: Keys,
) -> Result<(), anyhow::Error> {
    let mut capabilities = capabilities;
    if plugin.option(&OPT_NOTIFICATIONS).unwrap() {
        capabilities.push_str(" notifications");
    }
    let mut info_event_builder = EventBuilder::new(Kind::WalletConnectInfo, capabilities)
        .tag(Tag::parse(vec!["encryption", "nip44_v2 nip04"]).unwrap());

    if plugin.option(&OPT_NOTIFICATIONS).unwrap() {
        let notification_capabilities = build_notifications_vec(&plugin).join(" ");
        info_event_builder = info_event_builder
            .tag(Tag::parse(vec!["notifications", &notification_capabilities]).unwrap());
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
    nostr_client: client::Client,
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
    let mut use_nip44 = check_nip44_support(&event);

    let request = decrypt_request(&event.content, &wallet_keys, &client_pubkey, &mut use_nip44)?;

    let responses = match request.params {
        nip47::RequestParams::PayInvoice(pay_invoice_request) => {
            pay_invoice_response(plugin.clone(), pay_invoice_request, &label).await
        }
        nip47::RequestParams::MultiPayInvoice(multi_pay_invoice_request) => {
            multi_pay_invoice(plugin.clone(), multi_pay_invoice_request, &label).await
        }
        nip47::RequestParams::PayKeysend(pay_keysend_request) => {
            pay_keysend_response(plugin, pay_keysend_request, &label).await
        }
        nip47::RequestParams::MultiPayKeysend(multi_pay_keysend_request) => {
            multi_pay_keysend(plugin.clone(), multi_pay_keysend_request, &label).await
        }
        nip47::RequestParams::MakeInvoice(make_invoice_request) => {
            make_invoice_response(plugin.clone(), make_invoice_request).await
        }
        nip47::RequestParams::LookupInvoice(lookup_invoice_request) => {
            lookup_invoice_response(plugin.clone(), lookup_invoice_request).await
        }
        nip47::RequestParams::ListTransactions(list_transactions_request) => {
            list_transactions_response(plugin.clone(), list_transactions_request).await
        }
        nip47::RequestParams::GetBalance => get_balance_response(plugin.clone(), &label).await,
        nip47::RequestParams::GetInfo => get_info_response(plugin.clone(), &label).await,
        nip47::RequestParams::MakeHoldInvoice(make_hold_invoice_request) => {
            make_hold_invoice_response(plugin.clone(), make_hold_invoice_request).await
        }
        nip47::RequestParams::CancelHoldInvoice(cancel_hold_invoice_request) => {
            cancel_hold_invoice_response(plugin.clone(), cancel_hold_invoice_request).await
        }
        nip47::RequestParams::SettleHoldInvoice(settle_hold_invoice_request) => {
            settle_hold_invoice_response(plugin.clone(), settle_hold_invoice_request).await
        }
    };
    for (response, id) in responses {
        let content =
            match encrypt_response_content(&response, &wallet_keys, &client_pubkey, use_nip44) {
                Ok(o) => o,
                Err(e) => {
                    log::warn!("{e}");
                    continue;
                }
            };

        let response_event =
            match build_response_event(event.id, content, &wallet_keys, client_pubkey, id) {
                Ok(o) => o,
                Err(e) => {
                    log::warn!("Error signing reponse event! {e}");
                    continue;
                }
            };

        let send_result = match nostr_client.send_event(&response_event).await {
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

fn check_nip44_support(event: &Event) -> bool {
    if let Some(encryption_tag) = event
        .tags
        .find(TagKind::Custom(Cow::Borrowed("encryption")))
    {
        if let Some(enc_tag_content) = encryption_tag.content() {
            if enc_tag_content.contains("nip44_v2") {
                return true;
            }
        }
    }
    false
}

fn decrypt_request(
    event_content: &str,
    wallet_keys: &Keys,
    client_pubkey: &PublicKey,
    use_nip44: &mut bool,
) -> Result<nip47::Request, anyhow::Error> {
    let content = if *use_nip44 {
        match nip44::decrypt(wallet_keys.secret_key(), client_pubkey, event_content) {
            Ok(o) => o,
            Err(e) => {
                log::debug!("Could not decrypt using NIP-44:{e}. Trying NIP-04");
                match nip04::decrypt(wallet_keys.secret_key(), client_pubkey, event_content) {
                    Ok(o) => {
                        *use_nip44 = false;
                        o
                    }
                    Err(e) => {
                        log::warn!("Could not decrypt using NIP-04 or NIP-44:{e}");
                        return Err(e.into());
                    }
                }
            }
        }
    } else {
        match nip04::decrypt(wallet_keys.secret_key(), client_pubkey, event_content) {
            Ok(o) => o,
            Err(e) => {
                log::debug!("Could not decrypt using NIP-04:{e}. Trying NIP-44");
                match nip44::decrypt(wallet_keys.secret_key(), client_pubkey, event_content) {
                    Ok(o) => {
                        *use_nip44 = true;
                        o
                    }
                    Err(e) => {
                        log::warn!("Could not decrypt using NIP-04 or NIP-44:{e}");
                        return Err(e.into());
                    }
                }
            }
        }
    };
    log::debug!("Decrypted (nip44_v2:{use_nip44}):{content}");
    let request: nip47::Request = match serde_json::from_str(&content) {
        Ok(o) => o,
        Err(e) => {
            log::warn!("Error parsing nip47::Request! {e}");
            return Err(e.into());
        }
    };
    Ok(request)
}

fn encrypt_response_content(
    response: &nip47::Response,
    wallet_keys: &Keys,
    client_pubkey: &PublicKey,
    use_nip44: bool,
) -> Result<String, anyhow::Error> {
    let response_str = match serde_json::to_string(&response) {
        Ok(o) => o,
        Err(e) => {
            return Err(anyhow!("Error serializing response! {e}"));
        }
    };
    log::debug!("RESPONSE:{response_str}");
    if use_nip44 {
        match nip44::encrypt(
            wallet_keys.secret_key(),
            client_pubkey,
            response_str,
            nip44::Version::V2,
        ) {
            Ok(o) => Ok(o),
            Err(e) => Err(anyhow!("Error encrypting response with nip44! {e}")),
        }
    } else {
        match nip04::encrypt(wallet_keys.secret_key(), client_pubkey, response_str) {
            Ok(o) => Ok(o),
            Err(e) => Err(anyhow!("Error encrypting response with nip04! {e}")),
        }
    }
}

fn build_response_event(
    event_id: EventId,
    content: String,
    wallet_keys: &Keys,
    client_pubkey: PublicKey,
    id: Option<String>,
) -> Result<Event, anyhow::Error> {
    let mut response_builder = EventBuilder::new(Kind::WalletConnectResponse, content)
        .tag(Tag::event(event_id))
        .tag(Tag::public_key(client_pubkey));
    if let Some(i) = id {
        response_builder = response_builder.tag(Tag::custom(
            TagKind::SingleLetter(SingleLetterTag {
                character: Alphabet::D,
                uppercase: false,
            }),
            vec![i],
        ));
    }
    match response_builder.sign_with_keys(wallet_keys) {
        Ok(o) => Ok(o),
        Err(e) => Err(e.into()),
    }
}

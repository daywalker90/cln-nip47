import hashlib
import inspect
import json
import logging
import time
import asyncio
from datetime import datetime, timedelta
from typing import Any, Awaitable, Callable, Union

import pytest
from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import RpcError, wait_for, TIMEOUT
from util import generate_random_label, get_plugin  # noqa: F401

from nostr_sdk import (
    Alphabet,
    Client,
    RelayUrl,
    EventBuilder,
    Filter,
    Event,
    Keys,
    KeysendTlvRecord,
    Kind,
    HandleNotification,
    ListTransactionsRequest,
    LookupInvoiceRequest,
    MakeInvoiceRequest,
    NostrSdkError,
    NostrSigner,
    NostrWalletConnectUri,
    Nwc,
    PayInvoiceRequest,
    PayKeysendRequest,
    SingleLetterTag,
    Tag,
    TagKind,
    Method,
    PublicKey,
)

LOGGER = logging.getLogger(__name__)


class NotificationHandler(HandleNotification):
    def __init__(self, events_list, stop_after):
        self.events_list = events_list
        self.stop_after = stop_after
        self._done = asyncio.Event()

    async def handle(self, relay_url, subscription_id, event: Event):
        LOGGER.info(f"Received new event from {relay_url}: {event.as_json()}")
        self.events_list.append(event)
        if len(self.events_list) >= self.stop_after:
            self._done.set()

    async def handle_msg(self, relay_url, msg):
        _var = None


Action = Union[
    Callable[[], Awaitable[None]],
    Callable[[], None],
    Awaitable[None],
]


async def fetch_event_responses(
    client: Client,
    client_pubkey: PublicKey,
    event_kind: int,
    action: Action,
    stop_after: int,
    timeout: int = TIMEOUT,
) -> tuple[list[Event], Any]:
    events = []
    response_filter = Filter().kind(Kind(event_kind)).pubkey(client_pubkey)
    await client.subscribe(response_filter)

    handler = NotificationHandler(events, stop_after)
    task = asyncio.create_task(client.handle_notifications(handler))

    time.sleep(1)
    if inspect.iscoroutine(action):
        action_result = await action
    elif inspect.iscoroutinefunction(action):
        action_result = await action()
    elif callable(action):
        action_result = await asyncio.to_thread(action)
    else:
        raise TypeError("action must be a callable or an awaitable")

    try:
        await asyncio.wait_for(handler._done.wait(), timeout=timeout)
    except asyncio.TimeoutError:
        print(
            f"Timeout reached after {timeout} seconds, collected {len(events)} events"
        )
    finally:
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass

    await client.unsubscribe_all()
    assert len(events) == stop_after
    return (events, action_result)


async def fetch_info_event(
    client: Client,
    uri: NostrWalletConnectUri,
) -> Event:
    response_filter = Filter().kind(Kind(13194)).author(uri.public_key())
    events = await client.fetch_events(
        response_filter, timeout=timedelta(seconds=TIMEOUT)
    )
    start_time = datetime.now()
    while events.len() < 1 and (datetime.now() - start_time) < timedelta(
        seconds=TIMEOUT
    ):
        time.sleep(1)
        events = await client.fetch_events(
            response_filter, timeout=timedelta(seconds=1)
        )
    assert events.len() == 1

    return events.first()


@pytest.mark.asyncio
async def test_get_balance(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
            },
            {"log-level": "debug"},
        ],
    )
    node_balance = l1.rpc.call("listpeerchannels", {})["channels"][0]["spendable_msat"]
    uri_str = l1.rpc.call("nip47-create", ["test1", 3000])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = Nwc(uri)
    balance = await nwc.get_balance()
    assert balance == 3000

    uri_str = l1.rpc.call("nip47-create", ["test2"])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = Nwc(uri)
    balance = await nwc.get_balance()
    assert balance == node_balance

    uri_str = l1.rpc.call("nip47-create", ["test3", 0])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = Nwc(uri)
    balance = await nwc.get_balance()
    assert balance == 0

    with pytest.raises(RpcError, match="not an integer"):
        uri_str = l1.rpc.call("nip47-create", ["test3", -1])["uri"]


@pytest.mark.asyncio
async def test_get_info(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    l1 = node_factory.get_node(
        options={
            "log-level": "debug",
            "plugin": get_plugin,
            "nip47-relays": url,
        },
    )
    node_get_info = l1.rpc.call("getinfo", {})
    uri_str = l1.rpc.call("nip47-create", ["test1", 3000])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = Nwc(uri)
    get_info = await nwc.get_info()
    assert get_info.alias == node_get_info["alias"]
    assert get_info.block_height == node_get_info["blockheight"]
    assert get_info.color == node_get_info["color"]
    assert get_info.methods == [
        Method.MAKE_INVOICE,
        Method.LOOKUP_INVOICE,
        Method.LIST_TRANSACTIONS,
        Method.GET_BALANCE,
        Method.GET_INFO,
        Method.PAY_INVOICE,
        Method.MULTI_PAY_INVOICE,
        Method.PAY_KEYSEND,
        Method.MULTI_PAY_KEYSEND,
        "make_offer",
        "lookup_offer",
    ]
    assert get_info.network == "regtest"
    assert get_info.notifications == ["payment_received", "payment_sent"]
    assert get_info.pubkey == node_get_info["id"]

    l1.rpc.call("plugin", {"subcommand": "stop", "plugin": "cln-nip47"})
    l1.rpc.call(
        "plugin",
        {
            "subcommand": "start",
            "plugin": str(get_plugin),
            "nip47-notifications": False,
        },
    )
    l1.daemon.wait_for_log("All NWC's loaded")
    time.sleep(5)
    await client.connect()
    info_event = await fetch_info_event(client, uri)
    get_info = await nwc.get_info()
    assert get_info.alias == node_get_info["alias"]
    assert get_info.block_height == node_get_info["blockheight"]
    assert get_info.color == node_get_info["color"]
    assert get_info.methods.__str__ == [
        Method.MAKE_INVOICE,
        Method.LOOKUP_INVOICE,
        Method.LIST_TRANSACTIONS,
        Method.GET_BALANCE,
        Method.GET_INFO,
        Method.PAY_INVOICE,
        Method.MULTI_PAY_INVOICE,
        Method.PAY_KEYSEND,
        Method.MULTI_PAY_KEYSEND,
        "make_offer",
        "lookup_offer",
    ]
    assert get_info.network == "regtest"
    assert get_info.notifications == []
    assert get_info.pubkey == node_get_info["id"]

    assert (
        info_event.content()
        == "make_invoice lookup_invoice list_transactions get_balance get_info pay_invoice multi_pay_invoice pay_keysend multi_pay_keysend make_offer lookup_offer"
    )
    assert (
        info_event.tags().find(TagKind.UNKNOWN("encryption")).content()
        == "nip44_v2 nip04"
    )
    assert info_event.tags().find(TagKind.UNKNOWN("notifications")) is None

    uri_str = l1.rpc.call("nip47-create", ["test2", 0])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = Nwc(uri)
    get_info = await nwc.get_info()
    assert get_info.methods == [
        Method.MAKE_INVOICE,
        Method.LOOKUP_INVOICE,
        Method.LIST_TRANSACTIONS,
        Method.GET_BALANCE,
        Method.GET_INFO,
        "make_offer",
        "lookup_offer",
    ]

    info_event = await fetch_info_event(client, uri)
    assert (
        info_event.content()
        == "make_invoice lookup_invoice list_transactions get_balance get_info make_offer lookup_offer"
    )
    assert (
        info_event.tags().find(TagKind.UNKNOWN("encryption")).content()
        == "nip44_v2 nip04"
    )
    assert info_event.tags().find(TagKind.UNKNOWN("notifications")) is None


@pytest.mark.asyncio
async def test_make_invoice(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    l1 = node_factory.get_node(
        options={
            "log-level": "debug",
            "plugin": get_plugin,
            "nip47-relays": url,
        },
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 3000])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = Nwc(uri)
    timestamp = int(time.time())
    invoice = await nwc.make_invoice(
        MakeInvoiceRequest(
            amount=3000, description="test1", description_hash=None, expiry=None
        )
    )
    node_invoice = l1.rpc.call("decode", [invoice.invoice])
    assert invoice.payment_hash == node_invoice["payment_hash"]
    assert node_invoice["amount_msat"] == invoice.amount
    assert timestamp + node_invoice["expiry"] == pytest.approx(
        invoice.expires_at.as_secs(), abs=3
    )
    assert node_invoice["created_at"] == pytest.approx(
        invoice.created_at.as_secs(), abs=3
    )
    assert node_invoice["description"] == invoice.description
    assert "description_hash" not in node_invoice
    assert invoice.description_hash is None

    timestamp = int(time.time())
    invoice = await nwc.make_invoice(
        MakeInvoiceRequest(
            amount=3001,
            description="test2",
            description_hash=hashlib.sha256("test2".encode()).hexdigest(),
            expiry=120,
        )
    )
    node_invoice = l1.rpc.call("listinvoices", {"invstring": invoice.invoice})[
        "invoices"
    ][0]
    node_invoice_decode = l1.rpc.call("decode", [invoice.invoice])
    assert invoice.payment_hash == node_invoice["payment_hash"]
    assert node_invoice["amount_msat"] == invoice.amount
    assert timestamp + node_invoice_decode["expiry"] == pytest.approx(
        invoice.expires_at.as_secs(), abs=3
    )
    assert node_invoice_decode["created_at"] == pytest.approx(
        invoice.created_at.as_secs(), abs=3
    )
    assert node_invoice["description"] == invoice.description
    assert node_invoice_decode["description_hash"] == invoice.description_hash

    with pytest.raises(
        NostrSdkError.Generic, match="Must have description when using description_hash"
    ):
        await nwc.make_invoice(
            MakeInvoiceRequest(
                amount=3001,
                description=None,
                description_hash=hashlib.sha256("test2".encode()).hexdigest(),
                expiry=120,
            )
        )
    with pytest.raises(
        NostrSdkError.Generic, match="description_hash not matching description"
    ):
        await nwc.make_invoice(
            MakeInvoiceRequest(
                amount=3001,
                description="test1",
                description_hash=hashlib.sha256("test2".encode()).hexdigest(),
                expiry=120,
            )
        )


@pytest.mark.asyncio
async def test_make_offer(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    options = {
        "log-level": "debug",
        "plugin": get_plugin,
        "nip47-relays": url,
    }
    l1 = node_factory.get_node(
        options=options,
    )
    uri_res = l1.rpc.call("nip47-create", ["test1", 3000])
    uri_str = uri_res["uri"]
    client_pubkey = PublicKey.parse(uri_res["clientkey_public"])
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    content = {
        "method": "make_offer",
        "params": {
            "absolute_expiry": 1762986599,
            "amount": 3000,
            "description": "test1",
            "issuer": "me :)",
        },
    }
    content = json.dumps(content)
    signer = NostrSigner.keys(Keys(uri.secret()))
    encrypted_content = await signer.nip04_encrypt(uri.public_key(), content)
    event = (
        await EventBuilder(Kind(23194), encrypted_content)
        .tags([Tag.public_key(uri.public_key())])
        .sign(signer)
    )
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)

    (responses1, _res) = await fetch_event_responses(
        client,
        client_pubkey,
        23195,
        client.send_event(event),
        1,
    )
    error_events = []
    success_events = []
    for event in responses1:
        LOGGER.info(event)
        content = await signer.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        assert content["result_type"] == "make_offer"
        if "result" in content and content["result"] is not None:
            success_events.append(content)
        if "error" in content and content["error"] is not None:
            error_events.append(content)

    assert len(success_events) == 1
    assert len(error_events) == 0

    node_offer = l1.rpc.call("decode", [success_events[0]["result"]["offer"]])
    assert node_offer["offer_amount_msat"] == 3000
    assert node_offer["offer_absolute_expiry"] == 1762986599
    assert node_offer["offer_description"] == "test1"
    assert node_offer["offer_issuer"] == "me :)"
    assert success_events[0]["result"]["amount"] == 3000
    assert success_events[0]["result"]["description"] == "test1"
    assert success_events[0]["result"]["expires_at"] == 1762986599
    assert success_events[0]["result"]["issuer"] == "me :)"


@pytest.mark.asyncio
async def test_get_offer_info(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    opts = [
        {
            "log-level": "debug",
            "plugin": get_plugin,
            "nip47-relays": url,
        },
        {"log-level": "debug"},
    ]
    l1, l2 = node_factory.get_nodes(
        2,
        opts=opts,
    )

    timestamp = int(time.time())
    offer = l2.rpc.call(
        "offer",
        {
            "amount": "1000",
            "description": "test1",
            "absolute_expiry": timestamp + 4000,
            "issuer": "me :)",
        },
    )
    uri_res = l1.rpc.call("nip47-create", ["test1", 3000])
    uri_str = uri_res["uri"]
    client_pubkey = PublicKey.parse(uri_res["clientkey_public"])

    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    content = {
        "method": "get_offer_info",
        "params": {
            "offer": offer["bolt12"],
        },
    }
    content = json.dumps(content)
    signer = NostrSigner.keys(Keys(uri.secret()))
    encrypted_content = await signer.nip04_encrypt(uri.public_key(), content)
    event = (
        await EventBuilder(Kind(23194), encrypted_content)
        .tags([Tag.public_key(uri.public_key())])
        .sign(signer)
    )
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)

    (responses1, _res) = await fetch_event_responses(
        client,
        client_pubkey,
        23195,
        client.send_event(event),
        1,
    )
    error_events = []
    success_events = []
    for event in responses1:
        LOGGER.info(event)
        content = await signer.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        assert content["result_type"] == "get_offer_info"
        if "result" in content and content["result"] is not None:
            success_events.append(content)
        if "error" in content and content["error"] is not None:
            error_events.append(content)

    assert len(success_events) == 1
    assert len(error_events) == 0

    assert success_events[0]["result"]["amount"] == 1000
    assert success_events[0]["result"]["description"] == "test1"
    assert success_events[0]["result"]["expires_at"] == timestamp + 4000
    assert success_events[0]["result"]["issuer"] == "me :)"


@pytest.mark.asyncio
async def test_pay_keysend(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    l1, l2, l3 = node_factory.line_graph(
        3,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
            },
            {"log-level": "debug"},
            {"log-level": "debug"},
        ],
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 3000])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = Nwc(uri)
    result = await nwc.pay_keysend(
        PayKeysendRequest(
            id="id123", amount=1000, pubkey=l3.info["id"], preimage=None, tlv_records=[]
        )
    )
    pay = l1.rpc.call("listpays", {})["pays"][0]
    assert result.preimage == pay["preimage"]
    assert result.fees_paid == pay["amount_sent_msat"] - pay["amount_msat"]
    assert result.fees_paid == 1

    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_keysend(
            PayKeysendRequest(
                id="id123",
                amount=2001,
                pubkey=l2.info["id"],
                preimage=None,
                tlv_records=[KeysendTlvRecord(tlv_type=1234, value="a5c7e3d9b")],
            )
        )
    with pytest.raises(
        NostrSdkError.Generic, match="CLN generates the preimage itself"
    ):
        await nwc.pay_keysend(
            PayKeysendRequest(
                id="id123",
                amount=2001,
                pubkey=l2.info["id"],
                preimage="or3ijro3ijroi",
                tlv_records=[KeysendTlvRecord(tlv_type=1234, value="a5c7e3d9b")],
            )
        )


@pytest.mark.asyncio
async def test_multi_keysend(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    l1, l2, l3 = node_factory.line_graph(
        3,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
            },
            {"log-level": "debug"},
            {"log-level": "debug"},
        ],
    )
    uri_res = l1.rpc.call("nip47-create", ["test1", 3010])
    uri_str = uri_res["uri"]
    client_pubkey = PublicKey.parse(uri_res["clientkey_public"])
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    content = {
        "method": "multi_pay_keysend",
        "params": {
            "keysends": [
                {"id": "4da52c32a1", "pubkey": l2.info["id"], "amount": 1000},
                {"id": "3da52c32a1", "pubkey": l3.info["id"], "amount": 2000},
            ],
        },
    }
    content = json.dumps(content)
    signer = NostrSigner.keys(Keys(uri.secret()))
    encrypted_content = await signer.nip04_encrypt(uri.public_key(), content)
    event = (
        await EventBuilder(Kind(23194), encrypted_content)
        .tags([Tag.public_key(uri.public_key())])
        .sign(signer)
    )
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    (responses1, _res) = await fetch_event_responses(
        client, client_pubkey, 23195, client.send_event(event), 2
    )

    content = {
        "method": "multi_pay_keysend",
        "params": {
            "keysends": [
                {"id": "5da52c32a1", "pubkey": l2.info["id"], "amount": 5},
                {"id": "2da52c32a1", "pubkey": l3.info["id"], "amount": 5},
            ],
        },
    }
    content = json.dumps(content)
    encrypted_content = await signer.nip04_encrypt(uri.public_key(), content)
    event = (
        await EventBuilder(Kind(23194), encrypted_content)
        .tags([Tag.public_key(uri.public_key())])
        .sign(signer)
    )
    (responses2, _res) = await fetch_event_responses(
        client, client_pubkey, 23195, client.send_event(event), 2
    )

    reponses = responses1 + responses2

    error_events = []
    success_events = []
    for event in reponses:
        LOGGER.info(event)
        assert event.tags().find(
            TagKind.SINGLE_LETTER(SingleLetterTag.lowercase(Alphabet.D))
        )
        content = await signer.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        if "result" in content and content["result"] is not None:
            success_events.append(content)
        if "error" in content and content["error"] is not None:
            error_events.append(content)

    assert len(success_events) == 3
    assert len(error_events) == 1
    for content in success_events:
        assert content["result_type"] == "multi_pay_keysend"
        assert content["result"]["preimage"] is not None
    for content in error_events:
        assert content["result_type"] == "multi_pay_keysend"
        assert content["error"]["message"] == "Payment exceeds budget!"
        assert content["error"]["code"] == "QUOTA_EXCEEDED"


@pytest.mark.asyncio
async def test_lookup_invoice(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    l1, l2, l3 = node_factory.line_graph(
        3,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
            },
            {"log-level": "debug"},
            {"log-level": "debug"},
        ],
    )
    l1.rpc.call(
        "pay",
        {
            "bolt11": l2.rpc.call(
                "invoice",
                {
                    "amount_msat": 500000000,
                    "label": generate_random_label(),
                    "description": "balancechannel",
                },
            )["bolt11"]
        },
    )
    wait_for(
        lambda: l2.rpc.call("listpeerchannels", [l1.info["id"]])["channels"][0][
            "spendable_msat"
        ]
        > 3001
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 3000])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = Nwc(uri)
    invoice = await nwc.make_invoice(
        MakeInvoiceRequest(
            amount=3000, description="test1", description_hash=None, expiry=None
        )
    )

    with pytest.raises(
        NostrSdkError.Generic, match="Neither invoice nor payment_hash given"
    ):
        await nwc.lookup_invoice(
            LookupInvoiceRequest(
                payment_hash=None,
                invoice=None,
            )
        )

    listpays_rpc = l1.rpc.call("listinvoices", {"invstring": invoice.invoice})[
        "invoices"
    ][0]
    invoice_decode = l1.rpc.call("decode", [invoice.invoice])

    invoice_lookup = await nwc.lookup_invoice(
        LookupInvoiceRequest(
            payment_hash=invoice.payment_hash,
            invoice=None,
        )
    )
    assert invoice_lookup.invoice == invoice.invoice
    assert invoice_lookup.amount == 3000
    assert invoice_lookup.description == "test1"
    assert invoice_lookup.created_at.as_secs() == pytest.approx(
        invoice_decode["created_at"], abs=3
    )
    assert invoice_lookup.description_hash is None
    assert invoice_lookup.expires_at.as_secs() == pytest.approx(
        listpays_rpc["expires_at"], abs=3
    )
    assert invoice_lookup.fees_paid == 0
    assert invoice_lookup.metadata is None
    assert invoice_lookup.payment_hash == listpays_rpc["payment_hash"]
    assert invoice_lookup.transaction_type.name == "INCOMING"
    assert invoice_lookup.state.name == "PENDING"
    assert invoice_lookup.settled_at is None

    invoice_lookup = await nwc.lookup_invoice(
        LookupInvoiceRequest(
            payment_hash=None,
            invoice=invoice.invoice,
        )
    )
    assert invoice_lookup.invoice == invoice.invoice
    assert invoice_lookup.amount == 3000
    assert invoice_lookup.description == "test1"
    assert invoice_lookup.created_at.as_secs() == pytest.approx(
        invoice_decode["created_at"], abs=3
    )
    assert invoice_lookup.description_hash is None
    assert invoice_lookup.expires_at.as_secs() == pytest.approx(
        listpays_rpc["expires_at"], abs=3
    )
    assert invoice_lookup.fees_paid == 0
    assert invoice_lookup.metadata is None
    assert invoice_lookup.payment_hash == listpays_rpc["payment_hash"]
    assert invoice_lookup.transaction_type.name == "INCOMING"
    assert invoice_lookup.state.name == "PENDING"
    assert invoice_lookup.settled_at is None

    invoice = await nwc.make_invoice(
        MakeInvoiceRequest(
            amount=3001,
            description="test2",
            description_hash=hashlib.sha256("test2".encode()).hexdigest(),
            expiry=1000,
        )
    )

    listpays_rpc = l1.rpc.call("listinvoices", {"invstring": invoice.invoice})[
        "invoices"
    ][0]
    invoice_decode = l1.rpc.call("decode", [invoice.invoice])

    invoice_lookup = await nwc.lookup_invoice(
        LookupInvoiceRequest(
            payment_hash=invoice.payment_hash,
            invoice=None,
        )
    )
    assert invoice_lookup.invoice == invoice.invoice
    assert invoice_lookup.amount == 3001
    assert invoice_lookup.description is None
    assert invoice_lookup.created_at.as_secs() == pytest.approx(
        invoice_decode["created_at"], abs=3
    )
    assert (
        invoice_lookup.description_hash == hashlib.sha256("test2".encode()).hexdigest()
    )
    assert invoice_lookup.expires_at.as_secs() == pytest.approx(
        listpays_rpc["expires_at"], abs=3
    )
    assert invoice_lookup.fees_paid == 0
    assert invoice_lookup.metadata is None
    assert invoice_lookup.payment_hash == listpays_rpc["payment_hash"]
    assert invoice_lookup.transaction_type.name == "INCOMING"
    assert invoice_lookup.state.name == "PENDING"
    assert invoice_lookup.settled_at is None

    l2.rpc.call("pay", {"bolt11": invoice.invoice})
    listpays_rpc = l1.rpc.call("listinvoices", {"invstring": invoice.invoice})[
        "invoices"
    ][0]
    invoice_lookup = await nwc.lookup_invoice(
        LookupInvoiceRequest(
            payment_hash=invoice.payment_hash,
            invoice=None,
        )
    )
    assert invoice_lookup.invoice == invoice.invoice
    assert invoice_lookup.amount == 3001
    assert invoice_lookup.description is None
    assert invoice_lookup.created_at.as_secs() == pytest.approx(
        invoice_decode["created_at"], abs=3
    )
    assert (
        invoice_lookup.description_hash == hashlib.sha256("test2".encode()).hexdigest()
    )
    assert invoice_lookup.expires_at.as_secs() == pytest.approx(
        listpays_rpc["expires_at"], abs=3
    )
    assert invoice_lookup.fees_paid == 0
    assert invoice_lookup.metadata is None
    assert invoice_lookup.payment_hash == listpays_rpc["payment_hash"]
    assert invoice_lookup.transaction_type.name == "INCOMING"
    assert invoice_lookup.state.name == "SETTLED"
    assert invoice_lookup.settled_at.as_secs() == pytest.approx(
        listpays_rpc["paid_at"], abs=3
    )

    invoice = l3.rpc.call(
        "invoice",
        {
            "amount_msat": 4000,
            "label": generate_random_label(),
            "description": "outgoing",
        },
    )
    invoice_decode = l3.rpc.call("decode", [invoice["bolt11"]])
    pay = l1.rpc.call("pay", {"bolt11": invoice["bolt11"]})
    listpays_rpc = l1.rpc.call("listpays", {"bolt11": invoice["bolt11"]})["pays"][0]
    invoice_lookup = await nwc.lookup_invoice(
        LookupInvoiceRequest(
            payment_hash=pay["payment_hash"],
            invoice=None,
        )
    )
    assert invoice_lookup.invoice == invoice["bolt11"]
    assert invoice_lookup.amount == 4000
    assert invoice_lookup.description == "outgoing"
    assert invoice_lookup.created_at.as_secs() == pytest.approx(
        invoice_decode["created_at"], abs=3
    )
    assert invoice_lookup.description_hash is None
    assert invoice_lookup.expires_at is None
    assert invoice_lookup.fees_paid == 1
    assert invoice_lookup.metadata is None
    assert invoice_lookup.payment_hash == listpays_rpc["payment_hash"]
    assert invoice_lookup.transaction_type.name == "OUTGOING"
    assert invoice_lookup.state.name == "SETTLED"
    assert invoice_lookup.settled_at.as_secs() == pytest.approx(
        listpays_rpc["completed_at"], abs=3
    )

    invoice = await nwc.make_invoice(
        MakeInvoiceRequest(
            amount=0, description="test_0_amt", description_hash=None, expiry=None
        )
    )
    invoice_lookup = await nwc.lookup_invoice(
        LookupInvoiceRequest(
            payment_hash=invoice.payment_hash,
            invoice=None,
        )
    )
    assert invoice_lookup.amount == 0


@pytest.mark.asyncio
async def test_list_transactions(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
            },
            {"log-level": "debug"},
        ],
    )
    l1.rpc.call(
        "pay",
        {
            "bolt11": l2.rpc.call(
                "invoice",
                {
                    "amount_msat": 500000000,
                    "label": generate_random_label(),
                    "description": "balancechannel",
                },
            )["bolt11"]
        },
    )
    wait_for(
        lambda: l2.rpc.call("listpeerchannels", [l1.info["id"]])["channels"][0][
            "spendable_msat"
        ]
        > 30001
    )
    uri_str = l1.rpc.call("nip47-create", ["test1"])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = Nwc(uri)
    for i in range(10):
        invoice = l2.rpc.call(
            "invoice",
            {
                "label": generate_random_label(),
                "description": "test1",
                "amount_msat": 3000,
            },
        )
        result = await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
        )
        assert result.preimage is not None
    for i in range(10):
        invoice = await nwc.make_invoice(
            MakeInvoiceRequest(
                amount=3000, description="test2", description_hash=None, expiry=None
            )
        )
        result = l2.rpc.call("pay", [invoice.invoice])

    invoice = await nwc.make_invoice(
        MakeInvoiceRequest(
            amount=0, description="test_0_amt", description_hash=None, expiry=None
        )
    )
    result = l2.rpc.call("pay", [invoice.invoice, 1111])

    result = await nwc.list_transactions(
        ListTransactionsRequest(
            _from=None,
            until=None,
            limit=None,
            offset=None,
            unpaid=None,
            transaction_type=None,
        )
    )
    assert len(result) == 22
    for tx in result:
        tx.description is not None
        tx.invoice is not None
        tx.amount is not None
        tx.created_at is not None
        tx.description_hash is None
        tx.expires_at is None
        tx.preimage is not None
        tx.settled_at is not None
        tx.metadata is None
        tx.transaction_type is not None
        tx.state is not None
        tx.payment_hash is not None
        tx.fees_paid is not None


@pytest.mark.asyncio
async def test_notifications(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    l1, l2, l3 = node_factory.line_graph(
        3,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
            },
            {"log-level": "debug"},
            {"log-level": "debug", "plugin": get_plugin, "nip47-relays": url},
        ],
    )
    uri_res = l1.rpc.call("nip47-create", ["test1"])
    uri_str = uri_res["uri"]
    client_pubkey = PublicKey.parse(uri_res["clientkey_public"])
    LOGGER.info(uri_str)

    uri = NostrWalletConnectUri.parse(uri_str)
    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = Nwc(uri)

    invoice = l3.rpc.call(
        "invoice",
        {
            "label": generate_random_label(),
            "description": "test1",
            "amount_msat": 500000000,
        },
    )

    (responses1, pay1) = await fetch_event_responses(
        client,
        client_pubkey,
        23196,
        lambda: l1.rpc.call("pay", [invoice["bolt11"]]),
        1,
    )

    invoice1_rpc = l3.rpc.call("listinvoices", {"invstring": invoice["bolt11"]})[
        "invoices"
    ][0]
    invoice1_decode = l3.rpc.call("decode", [invoice["bolt11"]])
    pay1_list = l1.rpc.call("listpays", {"bolt11": invoice["bolt11"]})["pays"][0]

    wait_for(
        lambda: l2.rpc.call("listpeerchannels", [l1.info["id"]])["channels"][0][
            "spendable_msat"
        ]
        > 3000
    )
    wait_for(
        lambda: l3.rpc.call("listpeerchannels", [l2.info["id"]])["channels"][0][
            "spendable_msat"
        ]
        > 3000
    )

    result = await nwc.make_invoice(
        MakeInvoiceRequest(
            amount=3000, description="test2", description_hash=None, expiry=None
        )
    )
    (responses2, pay2) = await fetch_event_responses(
        client,
        client_pubkey,
        23196,
        lambda: l3.rpc.call("pay", [result.invoice]),
        1,
    )
    invoice2_list = l1.rpc.call("listinvoices", {"invstring": result.invoice})[
        "invoices"
    ][0]
    invoice2_decode = l3.rpc.call("decode", [result.invoice])

    responses = responses1 + responses2
    LOGGER.info(f"response1: {responses1} response2: {responses2}")
    assert len(responses) == 2
    signer = NostrSigner.keys(Keys(uri.secret()))
    received_events = []
    sent_events = []
    for event in responses:
        content = await signer.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        LOGGER.info(content)
        if content["notification_type"] == "payment_received":
            received_events.append(content)
        if content["notification_type"] == "payment_sent":
            sent_events.append(content)
        assert content["notification"]["preimage"] is not None
    assert len(received_events) == 1
    assert len(sent_events) == 1
    assert received_events[0]["notification"]["type"] == "incoming"
    assert received_events[0]["notification"]["invoice"] == result.invoice
    assert received_events[0]["notification"]["description"] == "test2"
    assert "description_hash" not in received_events[0]["notification"]
    assert received_events[0]["notification"]["preimage"] == pay2["payment_preimage"]
    assert received_events[0]["notification"]["payment_hash"] == pay2["payment_hash"]
    assert received_events[0]["notification"]["amount"] == 3000
    assert received_events[0]["notification"]["fees_paid"] == 0
    assert received_events[0]["notification"]["created_at"] == pytest.approx(
        invoice2_decode["created_at"], abs=3
    )
    assert "expires_at" not in received_events[0]["notification"]
    assert received_events[0]["notification"]["settled_at"] == pytest.approx(
        invoice2_list["paid_at"], abs=3
    )
    assert "metadata" not in received_events[0]["notification"]

    assert sent_events[0]["notification"]["type"] == "outgoing"
    assert sent_events[0]["notification"]["invoice"] == invoice["bolt11"]
    assert sent_events[0]["notification"]["description"] == "test1"
    assert "description_hash" not in sent_events[0]["notification"]
    assert sent_events[0]["notification"]["preimage"] == pay1["payment_preimage"]
    assert (
        sent_events[0]["notification"]["payment_hash"] == invoice1_rpc["payment_hash"]
    )
    assert sent_events[0]["notification"]["amount"] == 500000000
    assert sent_events[0]["notification"]["fees_paid"] == 5001
    assert sent_events[0]["notification"]["created_at"] == pytest.approx(
        invoice1_decode["created_at"], abs=3
    )
    assert "expires_at" not in sent_events[0]["notification"]
    assert sent_events[0]["notification"]["settled_at"] == pytest.approx(
        pay1_list["completed_at"], abs=3
    )
    assert "metadata" not in sent_events[0]["notification"]

    l1.rpc.call("plugin", {"subcommand": "stop", "plugin": "cln-nip47"})
    l1.rpc.call(
        "plugin",
        {
            "subcommand": "start",
            "plugin": str(get_plugin),
            "nip47-notifications": False,
        },
    )
    l1.daemon.wait_for_log("All NWC's loaded")
    time.sleep(3)
    await client.connect()
    await fetch_info_event(client, uri)

    invoice = l3.rpc.call(
        "invoice",
        {
            "label": generate_random_label(),
            "description": "test3",
            "amount_msat": 500,
        },
    )
    with pytest.raises(AssertionError, match="0 == 1"):
        (responses3, pay3) = await fetch_event_responses(
            client,
            client_pubkey,
            23196,
            nwc.pay_invoice(
                PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
            ),
            1,
            6,
        )


@pytest.mark.asyncio
async def test_pay_invoice(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
            },
            {"log-level": "debug"},
        ],
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 3001])["uri"]
    uri = NostrWalletConnectUri.parse(uri_str)
    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    LOGGER.info(uri_str)
    invoice = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test1", "amount_msat": 3000},
    )
    nwc = Nwc(NostrWalletConnectUri.parse(uri_str))
    result = await nwc.pay_invoice(
        PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
    )
    pay = l1.rpc.call("listpays", {"payment_hash": invoice["payment_hash"]})["pays"][0]
    assert result.preimage == pay["preimage"]

    invoice = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test2", "amount_msat": 1},
    )
    with pytest.raises(NostrSdkError.Generic, match="unnecessary"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=1, invoice=invoice["bolt11"])
        )
    invoice = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test3", "amount_msat": 2},
    )
    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
        )


@pytest.mark.asyncio
async def test_pay_offer(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    opts = [
        {
            "log-level": "debug",
            "plugin": get_plugin,
            "nip47-relays": url,
        },
        {"log-level": "debug"},
    ]
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=opts,
    )
    uri_res = l1.rpc.call("nip47-create", ["test1", 3001])
    uri_str = uri_res["uri"]
    client_pubkey = PublicKey.parse(uri_res["clientkey_public"])

    LOGGER.info(uri_str)
    offer1 = l2.rpc.call(
        "offer",
        {"amount": 3000, "description": "test1"},
    )
    uri = NostrWalletConnectUri.parse(uri_str)
    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)

    content = {
        "method": "pay_offer",
        "params": {
            "offer": offer1["bolt12"],
            "amount": 3000,
            "payer_note": "for pizza",
        },
    }
    json_content = json.dumps(content)
    encrypted_content1 = await signer.nip04_encrypt(uri.public_key(), json_content)
    event1 = (
        await EventBuilder(Kind(23194), encrypted_content1)
        .tags([Tag.public_key(uri.public_key())])
        .sign(signer)
    )

    (responses1, _res) = await fetch_event_responses(
        client,
        client_pubkey,
        23195,
        client.send_event(event1),
        1,
    )

    error_events = []
    success_events = []
    for event in responses1:
        LOGGER.info(event)
        assert event.tags().find(
            TagKind.SINGLE_LETTER(SingleLetterTag.lowercase(Alphabet.D))
        )
        content = await signer.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        if "result" in content and content["result"] is not None:
            success_events.append(content)
        if "error" in content and content["error"] is not None:
            error_events.append(content)

    assert len(success_events) == 1
    assert len(error_events) == 0

    pay = l1.rpc.call("listpays", {})["pays"][0]
    decoded_pay_inv = l2.rpc.call(
        "listinvoices", {"payment_hash": pay["payment_hash"]}
    )["invoices"][0]
    assert decoded_pay_inv["invreq_payer_note"] == "for pizza"
    assert success_events[0]["result"]["preimage"] == pay["preimage"]

    offer2 = l2.rpc.call(
        "offer",
        {"amount": "any", "description": "test2"},
    )

    content_err = {
        "method": "pay_offer",
        "params": {
            "offer": offer2["bolt12"],
        },
    }
    json_content_err = json.dumps(content_err)
    encrypted_content = await signer.nip04_encrypt(uri.public_key(), json_content_err)
    event = (
        await EventBuilder(Kind(23194), encrypted_content)
        .tags([Tag.public_key(uri.public_key())])
        .sign(signer)
    )
    (responses2, _res) = await fetch_event_responses(
        client,
        client_pubkey,
        23195,
        client.send_event(event),
        1,
    )
    error_events = []
    success_events = []
    for event in responses2:
        LOGGER.info(event)
        content = await signer.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        if "result" in content and content["result"] is not None:
            success_events.append(content)
        if "error" in content and content["error"] is not None:
            error_events.append(content)

    assert len(success_events) == 0
    assert len(error_events) == 1
    assert "amount_msat parameter required" in error_events[0]["error"]["message"]

    content2 = {
        "method": "pay_offer",
        "params": {
            "offer": offer2["bolt12"],
            "amount": 1,
        },
    }
    json_content2 = json.dumps(content2)
    encrypted_content2 = await signer.nip04_encrypt(uri.public_key(), json_content2)
    event2 = (
        await EventBuilder(Kind(23194), encrypted_content2)
        .tags([Tag.public_key(uri.public_key())])
        .sign(signer)
    )
    (responses3, _res) = await fetch_event_responses(
        client,
        client_pubkey,
        23195,
        client.send_event(event2),
        1,
    )
    error_events = []
    success_events = []
    for event in responses3:
        LOGGER.info(event)
        assert event.tags().find(
            TagKind.SINGLE_LETTER(SingleLetterTag.lowercase(Alphabet.D))
        )
        content = await signer.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        if "result" in content and content["result"] is not None:
            success_events.append(content)
        if "error" in content and content["error"] is not None:
            error_events.append(content)

    assert len(success_events) == 1
    assert len(error_events) == 0
    pay = l1.rpc.call("listpays", {})["pays"][1]
    assert success_events[0]["result"]["preimage"] == pay["preimage"]

    event2_again = (
        await EventBuilder(Kind(23194), encrypted_content2)
        .tags([Tag.public_key(uri.public_key())])
        .sign(signer)
    )

    (responses4, _res) = await fetch_event_responses(
        client,
        client_pubkey,
        23195,
        client.send_event(event2_again),
        1,
    )
    error_events = []
    success_events = []
    for event in responses4:
        LOGGER.info(event)
        assert event.tags().find(
            TagKind.SINGLE_LETTER(SingleLetterTag.lowercase(Alphabet.D))
        )
        content = await signer.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        if "result" in content and content["result"] is not None:
            success_events.append(content)
        if "error" in content and content["error"] is not None:
            error_events.append(content)

    assert len(success_events) == 0
    assert len(error_events) == 1
    assert "Payment exceeds budget" in error_events[0]["error"]["message"]

    event1_again = (
        await EventBuilder(Kind(23194), encrypted_content1)
        .tags([Tag.public_key(uri.public_key())])
        .sign(signer)
    )

    (responses5, _res) = await fetch_event_responses(
        client,
        client_pubkey,
        23195,
        client.send_event(event1_again),
        1,
    )
    error_events = []
    success_events = []
    for event in responses5:
        LOGGER.info(event)
        assert event.tags().find(
            TagKind.SINGLE_LETTER(SingleLetterTag.lowercase(Alphabet.D))
        )
        content = await signer.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        if "result" in content and content["result"] is not None:
            success_events.append(content)
        if "error" in content and content["error"] is not None:
            error_events.append(content)

    assert len(success_events) == 0
    assert len(error_events) == 1
    assert "Payment exceeds budget" in error_events[0]["error"]["message"]


@pytest.mark.asyncio
async def test_multi_pay_offer(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    opts = [
        {
            "log-level": "debug",
            "plugin": get_plugin,
            "nip47-relays": url,
        },
        {"log-level": "debug"},
    ]
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=opts,
    )
    uri_res = l1.rpc.call("nip47-create", ["test1", 30000])
    uri_str = uri_res["uri"]
    client_pubkey = PublicKey.parse(uri_res["clientkey_public"])

    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    offer1 = l2.rpc.call(
        "offer",
        {"description": "test1", "amount": 3000},
    )
    offer2 = l2.rpc.call(
        "offer",
        {"description": "test2", "amount": 4000},
    )
    offer3 = l2.rpc.call(
        "offer",
        {
            "description": "test3",
            "amount": 23001,
        },
    )
    content = {
        "method": "multi_pay_offer",
        "params": {
            "offers": [
                {"id": "4da52c32a1", "offer": offer1["bolt12"]},
                {"id": "3da52c32a1", "offer": offer2["bolt12"]},
                {"id": "af3g2k2o11", "offer": offer3["bolt12"]},
            ],
        },
    }
    content = json.dumps(content)
    signer = NostrSigner.keys(Keys(uri.secret()))
    encrypted_content = await signer.nip44_encrypt(uri.public_key(), content)
    event = (
        await EventBuilder(Kind(23194), encrypted_content)
        .tags([Tag.public_key(uri.public_key())])
        .sign(signer)
    )
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)

    (responses1, _res) = await fetch_event_responses(
        client,
        client_pubkey,
        23195,
        client.send_event(event),
        3,
    )
    success_pays = []
    error_pays = []
    for event in responses1:
        LOGGER.info(event)
        d_tag = event.tags().find(
            TagKind.SINGLE_LETTER(SingleLetterTag.lowercase(Alphabet.D))
        )
        content = await signer.nip44_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        assert content["result_type"] == "multi_pay_offer"
        if "result" in content and content["result"] is not None:
            assert d_tag is not None
            assert content["result"]["preimage"] is not None
            success_pays.append(content)
        if "error" in content and content["error"] is not None:
            assert d_tag.content() == "af3g2k2o11"
            assert content["error"]["code"] == "QUOTA_EXCEEDED"
            assert content["error"]["message"] == "Payment exceeds budget!"
            error_pays.append(content)
    assert len(success_pays) == 2
    assert len(error_pays) == 1


@pytest.mark.asyncio
async def test_multi_pay(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    opts = [
        {
            "log-level": "debug",
            "plugin": get_plugin,
            "nip47-relays": url,
        },
        {"log-level": "debug"},
    ]
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=opts,
    )
    uri_res = l1.rpc.call("nip47-create", ["test1", 30000])
    uri_str = uri_res["uri"]
    client_pubkey = PublicKey.parse(uri_res["clientkey_public"])
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    invoice1 = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test1", "amount_msat": 3000},
    )
    invoice2 = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test2", "amount_msat": 4000},
    )
    invoice3 = l2.rpc.call(
        "invoice",
        {
            "label": generate_random_label(),
            "description": "test3",
            "amount_msat": 23001,
        },
    )
    content = {
        "method": "multi_pay_invoice",
        "params": {
            "invoices": [
                {"id": "4da52c32a1", "invoice": invoice1["bolt11"]},
                {"id": "3da52c32a1", "invoice": invoice2["bolt11"]},
                {"id": "af3g2k2o11", "invoice": invoice3["bolt11"]},
            ],
        },
    }
    content = json.dumps(content)
    signer = NostrSigner.keys(Keys(uri.secret()))
    encrypted_content = await signer.nip44_encrypt(uri.public_key(), content)
    request_event = (
        await EventBuilder(Kind(23194), encrypted_content)
        .tags([Tag.public_key(uri.public_key())])
        .sign(signer)
    )
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()

    (responses, _res) = await fetch_event_responses(
        client, client_pubkey, 23195, client.send_event(request_event), 3
    )

    success_pays = []
    error_pays = []
    for response in responses:
        d_tag = response.tags().find(
            TagKind.SINGLE_LETTER(SingleLetterTag.lowercase(Alphabet.D))
        )
        content = await signer.nip44_decrypt(uri.public_key(), response.content())
        content = json.loads(content)
        assert content["result_type"] == "multi_pay_invoice"
        if "result" in content and content["result"] is not None:
            assert d_tag is not None
            assert content["result"]["preimage"] is not None
            success_pays.append(content)
        if "error" in content and content["error"] is not None:
            assert d_tag.content() == "af3g2k2o11"
            assert content["error"]["code"] == "QUOTA_EXCEEDED"
            assert content["error"]["message"] == "Payment exceeds budget!"
            error_pays.append(content)
    assert len(success_pays) == 2
    assert len(error_pays) == 1


@pytest.mark.asyncio
async def test_persistency(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
            },
            {"log-level": "debug"},
        ],
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 3000])["uri"]
    LOGGER.info(uri_str)
    invoice = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test1", "amount_msat": 3000},
    )
    l1.rpc.call("plugin", {"subcommand": "stop", "plugin": "cln-nip47"})
    l1.rpc.call(
        "plugin",
        {
            "subcommand": "start",
            "plugin": str(get_plugin),
        },
    )
    l1.daemon.wait_for_log("All NWC's loaded")
    time.sleep(3)
    uri = NostrWalletConnectUri.parse(uri_str)
    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = Nwc(uri)
    result = await nwc.pay_invoice(
        PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
    )
    assert result.preimage is not None

    invoice = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test1", "amount_msat": 1},
    )
    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
        )
    l1.rpc.call("plugin", {"subcommand": "stop", "plugin": "cln-nip47"})
    l1.rpc.call(
        "plugin",
        {
            "subcommand": "start",
            "plugin": str(get_plugin),
        },
    )
    l1.daemon.wait_for_log("All NWC's loaded")
    time.sleep(3)
    await client.connect()
    await fetch_info_event(client, uri)
    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
        )

    revoke = l1.rpc.call("nip47-revoke", ["test1"])
    assert revoke["revoked"] == "test1"

    uri_str = l1.rpc.call("nip47-create", ["test1", 3000, "10sec"])["uri"]
    uri = NostrWalletConnectUri.parse(uri_str)
    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = Nwc(uri)

    invoice = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test1", "amount_msat": 3000},
    )
    invoice_exceeded = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test1", "amount_msat": 3000},
    )
    result = await nwc.pay_invoice(
        PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
    )
    assert result.preimage is not None

    list = l1.rpc.call("nip47-list", ["test1"])[0]
    assert list["test1"]["budget_msat"] == 0

    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice_exceeded["bolt11"])
        )

    time.sleep(11)

    list = l1.rpc.call("nip47-list", ["test1"])[0]
    assert list["test1"]["budget_msat"] == 3000

    invoice = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test1", "amount_msat": 3000},
    )
    result = await nwc.pay_invoice(
        PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
    )
    assert result.preimage is not None

    list = l1.rpc.call("nip47-list", ["test1"])[0]
    assert list["test1"]["budget_msat"] == 0

    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice_exceeded["bolt11"])
        )

    l1.rpc.call("plugin", {"subcommand": "stop", "plugin": "cln-nip47"})
    l1.rpc.call(
        "plugin",
        {
            "subcommand": "start",
            "plugin": str(get_plugin),
        },
    )
    l1.daemon.wait_for_log("All NWC's loaded")
    time.sleep(3)
    await client.connect()
    await fetch_info_event(client, uri)

    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice_exceeded["bolt11"])
        )

    time.sleep(11)

    list = l1.rpc.call("nip47-list", ["test1"])[0]
    assert list["test1"]["budget_msat"] == 3000


@pytest.mark.asyncio
async def test_budget_command(node_factory, get_plugin, nostr_relay):  # noqa: F811
    url = nostr_relay
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
            },
            {"log-level": "debug"},
        ],
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 3000])["uri"]
    LOGGER.info(uri_str)
    invoice = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test1", "amount_msat": 5000},
    )
    uri = NostrWalletConnectUri.parse(uri_str)
    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = Nwc(uri)
    balance = await nwc.get_balance()
    assert balance == 3000

    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
        )

    l1.rpc.call("nip47-budget", ["test1", 4000])
    balance = await nwc.get_balance()
    assert balance == 4000

    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
        )

    l1.rpc.call("nip47-budget", ["test1", 5000, "15s"])
    balance = await nwc.get_balance()
    assert balance == 5000

    with pytest.raises(
        RpcError, match="`budget_msat` must be greater than 0 if you use `interval`"
    ):
        l1.rpc.call("nip47-budget", ["test1", 0, "1s"])

    pay = await nwc.pay_invoice(
        PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
    )
    assert pay.preimage is not None

    balance = await nwc.get_balance()
    assert balance == 0

    get_info = await nwc.get_info()
    assert get_info.methods == [
        Method.MAKE_INVOICE,
        Method.LOOKUP_INVOICE,
        Method.LIST_TRANSACTIONS,
        Method.GET_BALANCE,
        Method.GET_INFO,
        Method.PAY_INVOICE,
        Method.MULTI_PAY_INVOICE,
        Method.PAY_KEYSEND,
        Method.MULTI_PAY_KEYSEND,
        "make_offer",
        "lookup_offer",
    ]

    info_event = await fetch_info_event(client, uri)

    assert (
        info_event.content()
        == "make_invoice lookup_invoice list_transactions get_balance get_info pay_invoice multi_pay_invoice pay_keysend multi_pay_keysend make_offer lookup_offer notifications"
    )
    assert (
        info_event.tags().find(TagKind.UNKNOWN("encryption")).content()
        == "nip44_v2 nip04"
    )
    assert (
        info_event.tags().find(TagKind.UNKNOWN("notifications")).content()
        == "payment_received payment_sent"
    )

    time.sleep(18)

    balance = await nwc.get_balance()
    assert balance == 5000

    l1.rpc.call("nip47-budget", ["test1", 0])
    balance = await nwc.get_balance()
    assert balance == 0

    get_info = await nwc.get_info()
    assert get_info.methods == [
        Method.MAKE_INVOICE,
        Method.LOOKUP_INVOICE,
        Method.LIST_TRANSACTIONS,
        Method.GET_BALANCE,
        Method.GET_INFO,
        "make_offer",
        "lookup_offer",
    ]

    info_event = await fetch_info_event(client, uri)
    assert (
        info_event.content()
        == "make_invoice lookup_invoice list_transactions get_balance get_info make_offer lookup_offer notifications"
    )
    assert (
        info_event.tags().find(TagKind.UNKNOWN("encryption")).content()
        == "nip44_v2 nip04"
    )
    assert (
        info_event.tags().find(TagKind.UNKNOWN("notifications")).content()
        == "payment_received payment_sent"
    )

# ruff: noqa: DTZ005

import asyncio
import hashlib
import inspect
import json
import logging
import secrets
import time
import uuid
from collections.abc import Awaitable, Callable
from datetime import datetime, timedelta
from typing import Any, Union

import pytest
from nostr_sdk import (
    Client,
    Event,
    EventBuilder,
    Filter,
    Keys,
    KeysendTlvRecord,
    Kind,
    ListTransactionsRequest,
    LookupInvoiceRequest,
    MakeInvoiceRequest,
    Method,
    NostrSdkError,
    NostrWalletConnect,
    NostrWalletConnectUri,
    PayInvoiceRequest,
    PayKeysendRequest,
    PublicKey,
    RelayUrl,
    ReqTarget,
    Tag,
    TransactionType,
)
from pyln.testing.fixtures import *
from pyln.testing.utils import TIMEOUT, RpcError, wait_for
from util import generate_random_label, get_hold, get_plugin  # noqa: F401

LOGGER = logging.getLogger(__name__)


Action = Union[  # noqa: UP007
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
    target = ReqTarget.auto([response_filter])

    subscription_id = uuid.uuid4().hex
    LOGGER.info(f"Subscribing with id {subscription_id} to {response_filter}")

    await client.subscribe(target, subscription_id)

    async def collect_events():
        stream = client.notifications()

        while len(events) < stop_after:
            notification = await stream.next()

            if notification.is_new_event():
                event = notification.event
                relay_url = notification.relay_url

                LOGGER.info(f"Received new event from {relay_url}: {event.as_json()}")

                events.append(event)

    task = asyncio.create_task(collect_events())

    await asyncio.sleep(1)

    if inspect.iscoroutine(action):
        action_result = await action
    elif inspect.iscoroutinefunction(action):
        action_result = await action()
    elif callable(action):
        action_result = await asyncio.to_thread(action)
    else:
        raise TypeError("action must be a callable or an awaitable")

    try:
        await asyncio.wait_for(task, timeout=timeout)
    except asyncio.TimeoutError:
        print(
            f"Timeout reached after {timeout} seconds, collected {len(events)} events",
        )
    finally:
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass

        await client.unsubscribe_all()

    assert len(events) == stop_after
    return events, action_result


async def fetch_info_event(
    client: Client,
    uri: NostrWalletConnectUri,
) -> Event:
    response_filter = Filter().kind(Kind(13194)).author(uri.public_key())
    target = ReqTarget.auto([response_filter])
    events = await client.fetch_events(target, timeout=timedelta(seconds=TIMEOUT))
    start_time = datetime.now()
    while len(events) < 1 and (datetime.now() - start_time) < timedelta(
        seconds=TIMEOUT
    ):
        await asyncio.sleep(1)
        events = await client.fetch_events(target, timeout=timedelta(seconds=1))
    assert len(events) == 1

    return events[0]


@pytest.mark.asyncio
async def test_get_balance(nostr_relay, node_factory, get_plugin):  # noqa: F811
    url = nostr_relay
    l1, _l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
                "broken_log": r"Relay receiver exited with error|Connection failed",
            },
            {"log-level": "debug"},
        ],
    )
    node_balance = l1.rpc.call("listpeerchannels", {})["channels"][0]["spendable_msat"]
    uri_str = l1.rpc.call("nip47-create", ["test1", 3000])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    client = Client()
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = NostrWalletConnect(uri)
    balance = await nwc.get_balance()
    assert balance.balance == 3000

    uri_str = l1.rpc.call("nip47-create", ["test2"])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    client = Client()
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = NostrWalletConnect(uri)
    balance = await nwc.get_balance()
    assert balance.balance == node_balance

    uri_str = l1.rpc.call("nip47-create", ["test3", 0])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    client = Client()
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = NostrWalletConnect(uri)
    balance = await nwc.get_balance()
    assert balance.balance == 0

    with pytest.raises(RpcError, match="not an integer"):
        uri_str = l1.rpc.call("nip47-create", ["test3", -1])["uri"]


@pytest.mark.asyncio
async def test_get_info(nostr_relay, node_factory, get_plugin):  # noqa: F811
    url = nostr_relay
    l1 = node_factory.get_node(
        options={
            "log-level": "debug",
            "plugin": get_plugin,
            "nip47-relays": url,
        },
        broken_log=r"Relay receiver exited with error|Connection failed",
    )
    node_get_info = l1.rpc.call("getinfo", {})
    uri_str = l1.rpc.call("nip47-create", ["test1", 3000])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    client = Client()
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = NostrWalletConnect(uri)
    get_info = await nwc.get_info()
    assert get_info.alias == node_get_info["alias"]
    assert get_info.block_height == node_get_info["blockheight"]
    assert get_info.color == node_get_info["color"]
    assert get_info.methods == [
        Method.MAKE_INVOICE(),
        Method.LOOKUP_INVOICE(),
        Method.LIST_TRANSACTIONS(),
        Method.GET_BALANCE(),
        Method.GET_INFO(),
        Method.PAY_INVOICE(),
        Method.PAY_KEYSEND(),
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
    await asyncio.sleep(5)
    await client.connect()
    info_event = await fetch_info_event(client, uri)
    get_info = await nwc.get_info()
    assert get_info.alias == node_get_info["alias"]
    assert get_info.block_height == node_get_info["blockheight"]
    assert get_info.color == node_get_info["color"]
    assert get_info.methods == [
        Method.MAKE_INVOICE(),
        Method.LOOKUP_INVOICE(),
        Method.LIST_TRANSACTIONS(),
        Method.GET_BALANCE(),
        Method.GET_INFO(),
        Method.PAY_INVOICE(),
        Method.PAY_KEYSEND(),
    ]
    assert get_info.network == "regtest"
    assert get_info.notifications == []
    assert get_info.pubkey == node_get_info["id"]

    assert (
        info_event.content()
        == "make_invoice lookup_invoice list_transactions get_balance get_info pay_invoice pay_keysend"
    )
    encryption_tag = next(
        tag for tag in info_event.tags() if tag.kind() == "encryption"
    )
    assert encryption_tag.content() == "nip44_v2 nip04"
    assert not any(tag.kind() == "notifications" for tag in info_event.tags())

    uri_str = l1.rpc.call("nip47-create", ["test2", 0])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    client = Client()
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = NostrWalletConnect(uri)
    get_info = await nwc.get_info()
    assert get_info.methods == [
        Method.MAKE_INVOICE(),
        Method.LOOKUP_INVOICE(),
        Method.LIST_TRANSACTIONS(),
        Method.GET_BALANCE(),
        Method.GET_INFO(),
    ]

    info_event = await fetch_info_event(client, uri)
    assert (
        info_event.content()
        == "make_invoice lookup_invoice list_transactions get_balance get_info"
    )
    encryption_tag = next(
        tag for tag in info_event.tags() if tag.kind() == "encryption"
    )
    assert encryption_tag.content() == "nip44_v2 nip04"
    assert not any(tag.kind() == "notifications" for tag in info_event.tags())


@pytest.mark.asyncio
async def test_make_invoice(nostr_relay, node_factory, get_plugin):  # noqa: F811
    url = nostr_relay
    l1 = node_factory.get_node(
        options={
            "log-level": "debug",
            "plugin": get_plugin,
            "nip47-relays": url,
        },
        broken_log=r"Relay receiver exited with error|Connection failed",
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 3000])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    client = Client()
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = NostrWalletConnect(uri)
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
            description_hash=hashlib.sha256(b"test2").hexdigest(),
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
                description_hash=hashlib.sha256(b"test2").hexdigest(),
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
                description_hash=hashlib.sha256(b"test2").hexdigest(),
                expiry=120,
            )
        )


@pytest.mark.asyncio
async def test_pay_keysend(nostr_relay, node_factory, get_plugin):  # noqa: F811
    url = nostr_relay
    l1, l2, l3 = node_factory.line_graph(
        3,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
                "broken_log": r"Relay receiver exited with error|Connection failed",
            },
            {"log-level": "debug"},
            {"log-level": "debug"},
        ],
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 8000])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    client = Client()
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = NostrWalletConnect(uri)
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
                amount=7500,
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
                amount=7500,
                pubkey=l2.info["id"],
                preimage="or3ijro3ijroi",
                tlv_records=[KeysendTlvRecord(tlv_type=1234, value="a5c7e3d9b")],
            )
        )


@pytest.mark.asyncio
async def test_lookup_invoice(nostr_relay, node_factory, get_plugin):  # noqa: F811
    url = nostr_relay
    l1, l2, l3 = node_factory.line_graph(
        3,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
                "broken_log": r"Relay receiver exited with error|Connection failed",
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
        lambda: (
            l2.rpc.call("listpeerchannels", [l1.info["id"]])["channels"][0][
                "spendable_msat"
            ]
            > 3001
        )
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 3000])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    client = Client()
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = NostrWalletConnect(uri)
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
            description_hash=hashlib.sha256(b"test2").hexdigest(),
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
    assert invoice_lookup.description_hash == hashlib.sha256(b"test2").hexdigest()
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
    assert invoice_lookup.description_hash == hashlib.sha256(b"test2").hexdigest()
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
async def test_list_transactions(nostr_relay, node_factory, get_plugin):  # noqa: F811
    url = nostr_relay
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
                "broken_log": r"Relay receiver exited with error|Connection failed",
            },
            {"log-level": "debug"},
        ],
    )
    l1.rpc.call(
        "xpay",
        {
            "invstring": l2.rpc.call(
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
        lambda: (
            l2.rpc.call("listpeerchannels", [l1.info["id"]])["channels"][0][
                "spendable_msat"
            ]
            > 400000000
        )
    )
    uri_str = l1.rpc.call("nip47-create", ["test1"])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    client = Client()
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = NostrWalletConnect(uri)
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
        result = l2.rpc.call("xpay", [invoice.invoice])

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
        assert tx.description is not None
        assert tx.invoice is not None
        assert tx.amount is not None
        assert tx.created_at is not None
        assert tx.description_hash is None
        assert tx.preimage is not None
        assert tx.settled_at is not None
        assert tx.metadata is None
        assert tx.transaction_type is not None
        assert tx.state is not None
        assert tx.payment_hash is not None
        assert tx.fees_paid is not None

        if tx.transaction_type == TransactionType.INCOMING:
            assert tx.expires_at is not None
        else:
            assert tx.expires_at is None


@pytest.mark.asyncio
async def test_notifications(nostr_relay, node_factory, get_plugin):  # noqa: F811
    url = nostr_relay
    l1, l2, l3 = node_factory.line_graph(
        3,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
                "broken_log": r"Relay receiver exited with error|Connection failed",
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
    keys = Keys(uri.secret())
    client = Client()
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = NostrWalletConnect(uri)

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
        lambda: (
            l2.rpc.call("listpeerchannels", [l1.info["id"]])["channels"][0][
                "spendable_msat"
            ]
            > 3000
        )
    )
    wait_for(
        lambda: (
            l3.rpc.call("listpeerchannels", [l2.info["id"]])["channels"][0][
                "spendable_msat"
            ]
            > 3000
        )
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
    received_events = []
    sent_events = []
    for event in responses:
        content = keys.nip04_decrypt(uri.public_key(), event.content())
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
    await asyncio.sleep(3)
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
        (_responses3, _pay3) = await fetch_event_responses(
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
async def test_pay_invoice(nostr_relay, node_factory, get_plugin):  # noqa: F811
    url = nostr_relay
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
                "broken_log": r"Relay receiver exited with error|Connection failed",
            },
            {"log-level": "debug"},
        ],
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 9000])["uri"]
    uri = NostrWalletConnectUri.parse(uri_str)
    client = Client()
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    LOGGER.info(uri_str)
    invoice = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test1", "amount_msat": 3000},
    )
    nwc = NostrWalletConnect(NostrWalletConnectUri.parse(uri_str))
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
        {"label": generate_random_label(), "description": "test3", "amount_msat": 6500},
    )
    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
        )


@pytest.mark.asyncio
async def test_persistency(nostr_relay, node_factory, get_plugin):  # noqa: F811
    url = nostr_relay
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
                "broken_log": r"Relay receiver exited with error|Connection failed",
            },
            {"log-level": "debug"},
        ],
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 8000])["uri"]
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
    await asyncio.sleep(3)
    uri = NostrWalletConnectUri.parse(uri_str)
    client = Client()
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = NostrWalletConnect(uri)
    result = await nwc.pay_invoice(
        PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
    )
    assert result.preimage is not None

    invoice = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test1", "amount_msat": 5500},
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
    await asyncio.sleep(3)
    await client.connect()
    await fetch_info_event(client, uri)
    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
        )

    revoke = l1.rpc.call("nip47-revoke", ["test1"])
    assert revoke["revoked"] == "test1"

    uri_str = l1.rpc.call("nip47-create", ["test1", 8000, "10sec"])["uri"]
    uri = NostrWalletConnectUri.parse(uri_str)
    client = Client()
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = NostrWalletConnect(uri)

    invoice = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test1", "amount_msat": 3000},
    )
    invoice_exceeded = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test1", "amount_msat": 5500},
    )
    invoice_fee_exceeded = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test1", "amount_msat": 1},
    )
    result = await nwc.pay_invoice(
        PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
    )
    assert result.preimage is not None

    list = l1.rpc.call("nip47-list", ["test1"])[0]
    assert list["test1"]["budget_msat"] == 5000

    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice_exceeded["bolt11"])
        )
    with pytest.raises(
        NostrSdkError.Generic,
        match="Payment and estimated fees exceed the available budget",
    ):
        await nwc.pay_invoice(
            PayInvoiceRequest(
                id=None, amount=None, invoice=invoice_fee_exceeded["bolt11"]
            )
        )

    await asyncio.sleep(11)

    list = l1.rpc.call("nip47-list", ["test1"])[0]
    assert list["test1"]["budget_msat"] == 8000

    invoice = l2.rpc.call(
        "invoice",
        {"label": generate_random_label(), "description": "test1", "amount_msat": 3000},
    )
    result = await nwc.pay_invoice(
        PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
    )
    assert result.preimage is not None

    list = l1.rpc.call("nip47-list", ["test1"])[0]
    assert list["test1"]["budget_msat"] == 5000

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
    await asyncio.sleep(3)
    await client.connect()
    await fetch_info_event(client, uri)

    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice_exceeded["bolt11"])
        )

    await asyncio.sleep(11)

    list = l1.rpc.call("nip47-list", ["test1"])[0]
    assert list["test1"]["budget_msat"] == 8000


@pytest.mark.asyncio
async def test_budget_command(nostr_relay, node_factory, get_plugin):  # noqa: F811
    url = nostr_relay
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": url,
                "broken_log": r"Relay receiver exited with error|Connection failed",
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
    client = Client()
    await client.add_relay(RelayUrl.parse(url))
    await client.connect()
    await fetch_info_event(client, uri)
    nwc = NostrWalletConnect(uri)
    balance = await nwc.get_balance()
    assert balance.balance == 3000

    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
        )

    l1.rpc.call("nip47-budget", ["test1", 4000])
    balance = await nwc.get_balance()
    assert balance.balance == 4000

    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
        )

    l1.rpc.call("nip47-budget", ["test1", 10000, "15s"])
    balance = await nwc.get_balance()
    assert balance.balance == 10000

    with pytest.raises(
        RpcError, match="`budget_msat` must be greater than 0 if you use `interval`"
    ):
        l1.rpc.call("nip47-budget", ["test1", 0, "1s"])

    pay = await nwc.pay_invoice(
        PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
    )
    assert pay.preimage is not None

    balance = await nwc.get_balance()
    assert balance.balance == 5000

    get_info = await nwc.get_info()
    assert get_info.methods == [
        Method.MAKE_INVOICE(),
        Method.LOOKUP_INVOICE(),
        Method.LIST_TRANSACTIONS(),
        Method.GET_BALANCE(),
        Method.GET_INFO(),
        Method.PAY_INVOICE(),
        Method.PAY_KEYSEND(),
    ]

    info_event = await fetch_info_event(client, uri)

    assert (
        info_event.content()
        == "make_invoice lookup_invoice list_transactions get_balance get_info pay_invoice pay_keysend notifications"
    )
    encryption_tag = next(
        tag for tag in info_event.tags() if tag.kind() == "encryption"
    )
    assert encryption_tag.content() == "nip44_v2 nip04"
    notification_tag = next(
        tag for tag in info_event.tags() if tag.kind() == "notifications"
    )
    assert notification_tag.content() == "payment_received payment_sent"

    await asyncio.sleep(18)

    balance = await nwc.get_balance()
    assert balance.balance == 10000

    l1.rpc.call("nip47-budget", ["test1", 0])
    balance = await nwc.get_balance()
    assert balance.balance == 0

    get_info = await nwc.get_info()
    assert get_info.methods == [
        Method.MAKE_INVOICE(),
        Method.LOOKUP_INVOICE(),
        Method.LIST_TRANSACTIONS(),
        Method.GET_BALANCE(),
        Method.GET_INFO(),
    ]

    info_event = await fetch_info_event(client, uri)
    assert (
        info_event.content()
        == "make_invoice lookup_invoice list_transactions get_balance get_info notifications"
    )
    encryption_tag = next(
        tag for tag in info_event.tags() if tag.kind() == "encryption"
    )
    assert encryption_tag.content() == "nip44_v2 nip04"
    notification_tag = next(
        tag for tag in info_event.tags() if tag.kind() == "notifications"
    )
    assert notification_tag.content() == "payment_received payment_sent"


@pytest.mark.asyncio
async def test_hold_invoice(
    node_factory,
    executor,
    get_plugin,  # noqa: F811
    get_hold,  # noqa: F811
    nostr_relay,
):
    url = nostr_relay
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "may_reconnect": True,
            },
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "important-plugin": get_hold,
                "hold-grpc-port": node_factory.get_unused_port(),
                "nip47-relays": url,
                "may_reconnect": True,
                "broken_log": r"Relay receiver exited with error|Connection failed",
            },
        ],
    )
    uri_res = l2.rpc.call("nip47-create", ["test1", 3010])
    uri_str = uri_res["uri"]
    client_pubkey = PublicKey.parse(uri_res["clientkey_public"])
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)

    nwc = NostrWalletConnect(uri)

    preimage = secrets.token_hex(32)
    payment_hash = hashlib.sha256(bytes.fromhex(preimage)).hexdigest()
    LOGGER.info(f"preimage: {preimage}")
    LOGGER.info(f"payment_hash: {payment_hash}")
    content = {
        "method": "make_hold_invoice",
        "params": {
            "amount": 5000,
            "payment_hash": payment_hash,
        },
    }
    content = json.dumps(content)
    keys = Keys(uri.secret())
    encrypted_content = keys.nip04_encrypt(uri.public_key(), content)
    event = (
        await EventBuilder(Kind(23194), encrypted_content)
        .tags([Tag.public_key(uri.public_key())])
        .finalize_async(keys)
    )
    client = Client()
    relay_url = RelayUrl.parse(url)
    await client.add_relay(relay_url)
    await client.connect()

    await fetch_info_event(client, uri)

    (responses1, _res) = await fetch_event_responses(
        client, client_pubkey, 23195, client.send_event(event), 1
    )
    error_events = []
    success_events = []
    for event in responses1:
        LOGGER.info(event)
        content = keys.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        LOGGER.info(content)
        if "result" in content and content["result"] is not None:
            success_events.append(content)
        if "error" in content and content["error"] is not None:
            error_events.append(content)

    assert len(success_events) == 1
    assert len(error_events) == 0

    assert success_events[0]["result_type"] == "make_hold_invoice"
    assert success_events[0]["result"]["payment_hash"] == payment_hash
    assert success_events[0]["result"]["type"] == "incoming"
    assert success_events[0]["result"]["invoice"] is not None
    invoice1 = success_events[0]["result"]["invoice"]
    assert "description" not in success_events[0]["result"]
    assert "description_hash" not in success_events[0]["result"]
    assert success_events[0]["result"]["amount"] == 5000
    invoice1_created_at = pytest.approx(int(time.time()), abs=1)
    assert success_events[0]["result"]["created_at"] == invoice1_created_at
    invoice1_expires_at = pytest.approx(int(time.time()) + 3600, abs=1)
    assert success_events[0]["result"]["expires_at"] == invoice1_expires_at
    assert "metadata" not in success_events[0]["result"]

    lookup_hold = await nwc.lookup_invoice(
        LookupInvoiceRequest(
            payment_hash=payment_hash,
            invoice=None,
        )
    )
    assert lookup_hold.invoice == success_events[0]["result"]["invoice"]
    assert lookup_hold.amount == 5000
    assert lookup_hold.description is None
    assert lookup_hold.created_at.as_secs() == success_events[0]["result"]["created_at"]
    assert lookup_hold.description_hash is None
    assert lookup_hold.expires_at.as_secs() == success_events[0]["result"]["expires_at"]
    assert lookup_hold.fees_paid == 0
    assert lookup_hold.metadata is None
    assert lookup_hold.preimage is None
    assert lookup_hold.payment_hash == payment_hash
    assert lookup_hold.transaction_type.name == "INCOMING"
    assert lookup_hold.state.name == "PENDING"
    assert lookup_hold.settled_at is None

    (responses2, _res) = await fetch_event_responses(
        client,
        client_pubkey,
        23196,
        lambda: executor.submit(
            l1.rpc.call, "xpay", [success_events[0]["result"]["invoice"]]
        ),
        1,
    )
    hold_events = []
    for event in responses2:
        LOGGER.info(event)
        content = keys.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        LOGGER.info(content)
        if content["notification_type"] == "hold_invoice_accepted":
            hold_events.append(content)
        assert content["notification"]["payment_hash"] == payment_hash

    lookup_hold = await nwc.lookup_invoice(
        LookupInvoiceRequest(
            payment_hash=None,
            invoice=invoice1,
        )
    )
    assert lookup_hold.invoice == invoice1
    assert lookup_hold.amount == 5000
    assert lookup_hold.description is None
    assert lookup_hold.created_at.as_secs() == invoice1_created_at
    assert lookup_hold.description_hash is None
    assert lookup_hold.expires_at.as_secs() == invoice1_expires_at
    assert lookup_hold.fees_paid == 0
    assert lookup_hold.metadata is None
    assert lookup_hold.preimage is None
    assert lookup_hold.payment_hash == payment_hash
    assert lookup_hold.transaction_type.name == "INCOMING"
    assert lookup_hold.state.name == "ACCEPTED"
    assert lookup_hold.settled_at is None

    content = {
        "method": "settle_hold_invoice",
        "params": {
            "preimage": preimage,
        },
    }
    content = json.dumps(content)
    encrypted_content = keys.nip04_encrypt(uri.public_key(), content)
    event = (
        await EventBuilder(Kind(23194), encrypted_content)
        .tags([Tag.public_key(uri.public_key())])
        .finalize_async(keys)
    )

    (responses3, _res) = await fetch_event_responses(
        client, client_pubkey, 23195, client.send_event(event), 1
    )
    error_events = []
    success_events = []
    for event in responses3:
        LOGGER.info(event)
        content = keys.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        LOGGER.info(content)
        if (
            "result" in content
            and content["result"] is not None
            and content["result_type"] == "settle_hold_invoice"
        ):
            success_events.append(content)
        if "error" in content and content["error"] is not None:
            error_events.append(content)

    assert len(success_events) == 1
    assert len(error_events) == 0
    for content in success_events:
        assert content["result_type"] == "settle_hold_invoice"
        lookup_hold = await nwc.lookup_invoice(
            LookupInvoiceRequest(
                payment_hash=None,
                invoice=invoice1,
            )
        )
        assert lookup_hold.invoice == invoice1
        assert lookup_hold.amount == 5000
        assert lookup_hold.description is None
        assert lookup_hold.created_at.as_secs() == invoice1_created_at
        assert lookup_hold.description_hash is None
        assert lookup_hold.expires_at.as_secs() == invoice1_expires_at
        assert lookup_hold.fees_paid == 0
        assert lookup_hold.metadata is None
        assert lookup_hold.preimage == preimage
        assert lookup_hold.payment_hash == payment_hash
        assert lookup_hold.transaction_type.name == "INCOMING"
        assert lookup_hold.state.name == "SETTLED"
        assert lookup_hold.settled_at.as_secs() == pytest.approx(
            int(time.time()), abs=1
        )

    wait_for(
        lambda: (
            l1.rpc.call("listpays", {"payment_hash": payment_hash})["pays"][0]["status"]
            == "complete"
        )
    )

    preimage = secrets.token_hex(32)
    payment_hash = hashlib.sha256(bytes.fromhex(preimage)).hexdigest()
    content = {
        "method": "make_hold_invoice",
        "params": {
            "amount": 5000,
            "payment_hash": payment_hash,
            "description": "cancel_hold",
            "expiry": 1000,
            "min_cltv_expiry_delta": 200,
        },
    }
    content = json.dumps(content)
    encrypted_content = keys.nip04_encrypt(uri.public_key(), content)
    event = (
        await EventBuilder(Kind(23194), encrypted_content)
        .tags([Tag.public_key(uri.public_key())])
        .finalize_async(keys)
    )

    (responses4, _res) = await fetch_event_responses(
        client, client_pubkey, 23195, client.send_event(event), 1
    )
    error_events = []
    success_events = []
    for event in responses4:
        LOGGER.info(event)
        content = keys.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        LOGGER.info(content)
        if (
            "result" in content
            and content["result"] is not None
            and content["result_type"] == "make_hold_invoice"
            and content["result"]["payment_hash"] == payment_hash
        ):
            success_events.append(content)
        if "error" in content and content["error"] is not None:
            error_events.append(content)

    assert len(success_events) == 1
    assert len(error_events) == 0
    for content in success_events:
        assert content["result_type"] == "make_hold_invoice"
        assert content["result"]["payment_hash"] == payment_hash
        invoice2 = content["result"]["invoice"]
        invoice2_created_at = pytest.approx(int(time.time()), abs=1)
        invoice2_expires_at = pytest.approx(int(time.time()) + 1000, abs=1)

    (responses5, _res) = await fetch_event_responses(
        client,
        client_pubkey,
        23196,
        lambda: executor.submit(
            l1.rpc.call, "xpay", [success_events[0]["result"]["invoice"]]
        ),
        1,
    )
    hold_events = []
    for event in responses5:
        LOGGER.info(event)
        content = keys.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        LOGGER.info(content)
        if (
            content["notification_type"] == "hold_invoice_accepted"
            and content["notification"]["payment_hash"] == payment_hash
        ):
            hold_events.append(content)
    assert len(hold_events) == 1

    content = {
        "method": "cancel_hold_invoice",
        "params": {
            "payment_hash": payment_hash,
        },
    }
    content = json.dumps(content)
    encrypted_content = keys.nip04_encrypt(uri.public_key(), content)
    event = (
        await EventBuilder(Kind(23194), encrypted_content)
        .tags([Tag.public_key(uri.public_key())])
        .finalize_async(keys)
    )

    (responses6, _res) = await fetch_event_responses(
        client,
        client_pubkey,
        23195,
        client.send_event(event),
        1,
    )
    error_events = []
    success_events = []
    for event in responses6:
        LOGGER.info(event)
        content = keys.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        LOGGER.info(content)
        if (
            "result" in content
            and content["result"] is not None
            and content["result_type"] == "cancel_hold_invoice"
        ):
            success_events.append(content)
        if "error" in content and content["error"] is not None:
            error_events.append(content)

    assert len(success_events) == 1
    assert len(error_events) == 0
    for content in success_events:
        assert content["result_type"] == "cancel_hold_invoice"
        lookup_hold = await nwc.lookup_invoice(
            LookupInvoiceRequest(
                payment_hash=None,
                invoice=invoice2,
            )
        )
        assert lookup_hold.invoice == invoice2
        assert lookup_hold.amount == 5000
        assert lookup_hold.description == "cancel_hold"
        assert lookup_hold.created_at.as_secs() == invoice2_created_at
        assert lookup_hold.description_hash is None
        assert lookup_hold.expires_at.as_secs() == invoice2_expires_at
        assert lookup_hold.fees_paid == 0
        assert lookup_hold.metadata is None
        assert lookup_hold.preimage is None
        assert lookup_hold.payment_hash == payment_hash
        assert lookup_hold.transaction_type.name == "INCOMING"
        assert lookup_hold.state.name == "EXPIRED"
        assert lookup_hold.settled_at is None

    invoice2_decoded = l1.rpc.call("decode", [invoice2])
    assert invoice2_decoded["min_final_cltv_expiry"] == 200

    wait_for(
        lambda: (
            l1.rpc.call("listpays", {"payment_hash": payment_hash})["pays"][0]["status"]
            == "failed"
        )
    )

    nwc = NostrWalletConnect(uri)

    invoice_lookup1 = await nwc.lookup_invoice(
        LookupInvoiceRequest(
            payment_hash=payment_hash,
            invoice=None,
        )
    )
    invoice_lookup2 = await nwc.lookup_invoice(
        LookupInvoiceRequest(
            payment_hash=None,
            invoice=invoice2,
        )
    )
    invoice_lookup3 = await nwc.lookup_invoice(
        LookupInvoiceRequest(
            payment_hash=payment_hash,
            invoice=invoice2,
        )
    )
    assert invoice_lookup1 == invoice_lookup2
    assert invoice_lookup1 == invoice_lookup3

    invoice_lookup4 = await nwc.lookup_invoice(
        LookupInvoiceRequest(
            payment_hash=None,
            invoice=invoice1,
        )
    )

    result = await nwc.list_transactions(
        ListTransactionsRequest(
            _from=None,
            until=None,
            limit=None,
            offset=None,
            unpaid=True,
            transaction_type=None,
        )
    )
    assert len(result) == 2
    assert result == [invoice_lookup1, invoice_lookup4] or result == [
        invoice_lookup4,
        invoice_lookup1,
    ]

    description3 = "test3"
    description_hash3 = hashlib.sha256(description3.encode()).hexdigest()
    preimage = secrets.token_hex(32)
    payment_hash = hashlib.sha256(bytes.fromhex(preimage)).hexdigest()
    content = {
        "method": "make_hold_invoice",
        "params": {
            "amount": 5001,
            "payment_hash": payment_hash,
            "description": description3,
            "description_hash": description_hash3,
        },
    }
    content = json.dumps(content)
    encrypted_content = keys.nip04_encrypt(uri.public_key(), content)
    event = (
        await EventBuilder(Kind(23194), encrypted_content)
        .tags([Tag.public_key(uri.public_key())])
        .finalize_async(keys)
    )
    await client.send_event(event)

    start_time = datetime.now()
    while (datetime.now() - start_time) < timedelta(seconds=10):
        await asyncio.sleep(1)
        try:
            await nwc.lookup_invoice(
                LookupInvoiceRequest(
                    payment_hash=payment_hash,
                    invoice=None,
                )
            )
            break
        except Exception as e:  # noqa: BLE001
            LOGGER.error(e)
            continue

    lookup_hold = await nwc.lookup_invoice(
        LookupInvoiceRequest(
            payment_hash=payment_hash,
            invoice=None,
        )
    )
    assert lookup_hold.amount == 5001
    assert lookup_hold.description is None
    assert lookup_hold.description_hash == description_hash3
    assert lookup_hold.metadata is None
    assert lookup_hold.preimage is None
    assert lookup_hold.payment_hash == payment_hash
    assert lookup_hold.transaction_type.name == "INCOMING"
    assert lookup_hold.state.name == "PENDING"
    assert lookup_hold.settled_at is None

    description4 = "test4"
    description_hash4 = hashlib.sha256(description4.encode()).hexdigest()
    preimage = secrets.token_hex(32)
    payment_hash = hashlib.sha256(bytes.fromhex(preimage)).hexdigest()
    content = {
        "method": "make_hold_invoice",
        "params": {
            "amount": 5002,
            "payment_hash": payment_hash,
            "description_hash": description_hash4,
        },
    }
    content = json.dumps(content)
    encrypted_content = keys.nip04_encrypt(uri.public_key(), content)
    event = (
        await EventBuilder(Kind(23194), encrypted_content)
        .tags([Tag.public_key(uri.public_key())])
        .finalize_async(keys)
    )
    await client.send_event(event)

    start_time = datetime.now()
    while (datetime.now() - start_time) < timedelta(seconds=10):
        await asyncio.sleep(1)
        try:
            await nwc.lookup_invoice(
                LookupInvoiceRequest(
                    payment_hash=payment_hash,
                    invoice=None,
                )
            )
            break
        except Exception as e:  # noqa: BLE001
            LOGGER.error(e)
            continue

    lookup_hold = await nwc.lookup_invoice(
        LookupInvoiceRequest(
            payment_hash=payment_hash,
            invoice=None,
        )
    )
    assert lookup_hold.amount == 5002
    assert lookup_hold.description is None
    assert lookup_hold.description_hash == description_hash4
    assert lookup_hold.metadata is None
    assert lookup_hold.preimage is None
    assert lookup_hold.payment_hash == payment_hash
    assert lookup_hold.transaction_type.name == "INCOMING"
    assert lookup_hold.state.name == "PENDING"
    assert lookup_hold.settled_at is None

    info_event = await fetch_info_event(client, uri)
    assert (
        info_event.content()
        == "make_invoice lookup_invoice list_transactions get_balance get_info pay_invoice pay_keysend make_hold_invoice cancel_hold_invoice settle_hold_invoice notifications"
    )
    encryption_tag = next(
        tag for tag in info_event.tags() if tag.kind() == "encryption"
    )
    assert encryption_tag.content() == "nip44_v2 nip04"
    notification_tag = next(
        tag for tag in info_event.tags() if tag.kind() == "notifications"
    )
    assert (
        notification_tag.content()
        == "payment_received payment_sent hold_invoice_accepted"
    )

    l2.restart()

    l1.rpc.connect(l2.info["id"], "localhost", l2.port)

    (responses7, _res) = await fetch_event_responses(
        client,
        client_pubkey,
        23196,
        lambda: executor.submit(l1.rpc.call, "xpay", [lookup_hold.invoice]),
        1,
    )
    hold_events = []
    for event in responses7:
        LOGGER.info(event)
        content = keys.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        LOGGER.info(content)
        if (
            content["notification_type"] == "hold_invoice_accepted"
            and content["notification"]["payment_hash"] == payment_hash
        ):
            hold_events.append(content)
    assert len(hold_events) == 1


@pytest.mark.asyncio
async def test_hold_invoice_expiry(
    node_factory,
    get_plugin,  # noqa: F811
    get_hold,  # noqa: F811
    nostr_relay,
):
    url = nostr_relay
    l2 = node_factory.get_node(
        options={
            "log-level": "debug",
            "plugin": get_plugin,
            "important-plugin": get_hold,
            "hold-grpc-port": node_factory.get_unused_port(),
            "nip47-relays": url,
        },
        broken_log=r"Relay receiver exited with error|Connection failed",
    )
    uri_res = l2.rpc.call("nip47-create", ["test1", 3010])
    uri_str = uri_res["uri"]
    client_pubkey = PublicKey.parse(uri_res["clientkey_public"])
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)

    preimage = secrets.token_hex(32)
    payment_hash = hashlib.sha256(bytes.fromhex(preimage)).hexdigest()
    LOGGER.info(f"preimage: {preimage}")
    LOGGER.info(f"payment_hash: {payment_hash}")
    content = {
        "method": "make_hold_invoice",
        "params": {
            "amount": 5000,
            "payment_hash": payment_hash,
            "expiry": 5,
        },
    }
    content = json.dumps(content)
    keys = Keys(uri.secret())
    encrypted_content = keys.nip04_encrypt(uri.public_key(), content)
    event = (
        await EventBuilder(Kind(23194), encrypted_content)
        .tags([Tag.public_key(uri.public_key())])
        .finalize_async(keys)
    )
    client = Client()
    relay_url = RelayUrl.parse(url)
    await client.add_relay(relay_url)
    await client.connect()

    await fetch_info_event(client, uri)

    (responses1, _res) = await fetch_event_responses(
        client, client_pubkey, 23195, client.send_event(event), 1
    )
    error_events = []
    success_events = []
    for event in responses1:
        LOGGER.info(event)
        content = keys.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
        LOGGER.info(content)
        if "result" in content and content["result"] is not None:
            success_events.append(content)
        if "error" in content and content["error"] is not None:
            error_events.append(content)

    assert len(success_events) == 1
    assert len(error_events) == 0

    # The hold plugin has no expired state: an invoice that expires without
    # ever being accepted (and without being cancelled) stays `Unpaid` and the
    # track stream never ends on its own. The handler must give up at the
    # invoice's expiry instead of hanging, and must not send a spurious
    # accepted notification.
    l2.daemon.wait_for_log("was not accepted, skipping notification", timeout=20)

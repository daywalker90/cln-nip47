import hashlib
import importlib.resources as pkg_resources
import json
import logging
import socket
import subprocess
import time
from datetime import datetime, timedelta
from pathlib import Path
from threading import Thread

import pytest
import pytest_asyncio
import yaml
from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import RpcError, wait_for
from util import generate_random_label, get_plugin  # noqa: F401

from nostr_sdk import (
    Alphabet,
    Client,
    RelayUrl,
    EventBuilder,
    Filter,
    Keys,
    KeysendTlvRecord,
    Kind,
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
)

LOGGER = logging.getLogger(__name__)


@pytest.mark.asyncio
async def test_get_balance(node_factory, get_plugin, nostr_client):  # noqa: F811
    nostr_client, relay_port = nostr_client
    url = f"127.0.0.1:{relay_port}"
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": f"ws://{url}",
            },
            {"log-level": "debug"},
        ],
    )
    node_balance = l1.rpc.call("listpeerchannels", {})["channels"][0]["spendable_msat"]
    uri_str = l1.rpc.call("nip47-create", ["test1", 3000])["uri"]
    LOGGER.info(uri_str)
    nwc = Nwc(NostrWalletConnectUri.parse(uri_str))
    balance = await nwc.get_balance()
    assert balance == 3000

    uri_str = l1.rpc.call("nip47-create", ["test2"])["uri"]
    LOGGER.info(uri_str)
    nwc = Nwc(NostrWalletConnectUri.parse(uri_str))
    balance = await nwc.get_balance()
    assert balance == node_balance

    uri_str = l1.rpc.call("nip47-create", ["test3", 0])["uri"]
    LOGGER.info(uri_str)
    nwc = Nwc(NostrWalletConnectUri.parse(uri_str))
    balance = await nwc.get_balance()
    assert balance == 0

    with pytest.raises(RpcError, match="not an integer"):
        uri_str = l1.rpc.call("nip47-create", ["test3", -1])["uri"]


@pytest.mark.asyncio
async def test_get_info(node_factory, get_plugin, nostr_client):  # noqa: F811
    nostr_client, relay_port = nostr_client
    url = f"127.0.0.1:{relay_port}"
    l1 = node_factory.get_node(
        options={
            "log-level": "debug",
            "plugin": get_plugin,
            "nip47-relays": f"ws://{url}",
        },
    )
    node_get_info = l1.rpc.call("getinfo", {})
    uri_str = l1.rpc.call("nip47-create", ["test1", 3000])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    nwc = Nwc(uri)
    get_info = await nwc.get_info()
    assert get_info.alias == node_get_info["alias"]
    assert get_info.block_height == node_get_info["blockheight"]
    assert get_info.color == node_get_info["color"]
    assert get_info.methods == [
        "pay_invoice",
        "multi_pay_invoice",
        "pay_keysend",
        "multi_pay_keysend",
        "make_invoice",
        "lookup_invoice",
        "list_transactions",
        "get_balance",
        "get_info",
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
    get_info = await nwc.get_info()
    assert get_info.alias == node_get_info["alias"]
    assert get_info.block_height == node_get_info["blockheight"]
    assert get_info.color == node_get_info["color"]
    assert get_info.methods == [
        "pay_invoice",
        "multi_pay_invoice",
        "pay_keysend",
        "multi_pay_keysend",
        "make_invoice",
        "lookup_invoice",
        "list_transactions",
        "get_balance",
        "get_info",
    ]
    assert get_info.network == "regtest"
    assert get_info.notifications == []
    assert get_info.pubkey == node_get_info["id"]

    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(f"ws://{url}"))
    await client.connect()

    response_filter = Filter().kind(Kind(13194)).author(uri.public_key())
    events = await client.fetch_events(response_filter, timeout=timedelta(seconds=10))
    start_time = datetime.now()
    while events.len() < 1 and (datetime.now() - start_time) < timedelta(seconds=10):
        time.sleep(1)
        events = await client.fetch_events(
            response_filter, timeout=timedelta(seconds=1)
        )
    assert events.len() == 1
    assert (
        events.to_vec()[0].content()
        == "pay_invoice multi_pay_invoice pay_keysend multi_pay_keysend make_invoice lookup_invoice list_transactions get_balance get_info"
    )

    uri_str = l1.rpc.call("nip47-create", ["test2", 0])["uri"]
    LOGGER.info(uri_str)
    uri = NostrWalletConnectUri.parse(uri_str)
    nwc = Nwc(uri)
    get_info = await nwc.get_info()
    assert get_info.methods == [
        "make_invoice",
        "lookup_invoice",
        "list_transactions",
        "get_balance",
        "get_info",
    ]

    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(f"ws://{url}"))
    await client.connect()

    response_filter = Filter().kind(Kind(13194)).author(uri.public_key())
    events = await client.fetch_events(response_filter, timeout=timedelta(seconds=10))
    start_time = datetime.now()
    while events.len() < 1 and (datetime.now() - start_time) < timedelta(seconds=10):
        time.sleep(1)
        events = await client.fetch_events(
            response_filter, timeout=timedelta(seconds=1)
        )
    assert events.len() == 1
    assert (
        events.to_vec()[0].content()
        == "make_invoice lookup_invoice list_transactions get_balance get_info"
    )


@pytest.mark.asyncio
async def test_make_invoice(node_factory, get_plugin, nostr_client):  # noqa: F811
    nostr_client, relay_port = nostr_client
    url = f"127.0.0.1:{relay_port}"
    l1 = node_factory.get_node(
        options={
            "log-level": "debug",
            "plugin": get_plugin,
            "nip47-relays": f"ws://{url}",
        },
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 3000])["uri"]
    LOGGER.info(uri_str)
    nwc = Nwc(NostrWalletConnectUri.parse(uri_str))
    invoice = await nwc.make_invoice(
        MakeInvoiceRequest(
            amount=3000, description="test1", description_hash=None, expiry=None
        )
    )
    node_invoice = l1.rpc.call("decode", [invoice.invoice])
    assert invoice.payment_hash == node_invoice["payment_hash"]
    assert node_invoice["amount_msat"] == 3000
    assert node_invoice["expiry"] == 604800
    assert node_invoice["description"] == "test1"
    assert "description_hash" not in node_invoice

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
    assert node_invoice["amount_msat"] == 3001
    assert node_invoice_decode["expiry"] == 120
    assert node_invoice["description"] == "test2"
    assert (
        node_invoice_decode["description_hash"]
        == hashlib.sha256("test2".encode()).hexdigest()
    )
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
async def test_pay_keysend(node_factory, get_plugin, nostr_client):  # noqa: F811
    nostr_client, relay_port = nostr_client
    url = f"127.0.0.1:{relay_port}"
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": f"ws://{url}",
            },
            {"log-level": "debug"},
        ],
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 3000])["uri"]
    LOGGER.info(uri_str)
    nwc = Nwc(NostrWalletConnectUri.parse(uri_str))
    result = await nwc.pay_keysend(
        PayKeysendRequest(
            id="id123", amount=1000, pubkey=l2.info["id"], preimage=None, tlv_records=[]
        )
    )
    pay = l1.rpc.call("listpays", {})["pays"][0]
    assert result.preimage == pay["preimage"]

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
async def test_multi_keysend(node_factory, get_plugin, nostr_client):  # noqa: F811
    nostr_client, relay_port = nostr_client
    url = f"127.0.0.1:{relay_port}"
    l1, l2, l3 = node_factory.line_graph(
        3,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": f"ws://{url}",
            },
            {"log-level": "debug"},
            {"log-level": "debug"},
        ],
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 3010])["uri"]
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
    await client.add_relay(RelayUrl.parse(f"ws://{url}"))
    await client.connect()
    await client.send_event(event)

    content = {
        "method": "multi_pay_keysend",
        "params": {
            "keysends": [
                {"id": "4da52c32a1", "pubkey": l2.info["id"], "amount": 5},
                {"id": "3da52c32a1", "pubkey": l3.info["id"], "amount": 5},
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
    await client.add_relay(RelayUrl.parse(f"ws://{url}"))
    await client.connect()
    await client.send_event(event)

    response_filter = Filter().kind(Kind(23195)).author(uri.public_key())
    events = await client.fetch_events(response_filter, timeout=timedelta(seconds=10))
    start_time = datetime.now()
    while events.len() < 4 and (datetime.now() - start_time) < timedelta(seconds=10):
        time.sleep(1)
        events = await client.fetch_events(
            response_filter, timeout=timedelta(seconds=1)
        )
    assert events.len() == 4
    error_events = []
    success_events = []
    for event in events.to_vec():
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
async def test_lookup_invoice(node_factory, get_plugin, nostr_client):  # noqa: F811
    nostr_client, relay_port = nostr_client
    url = f"127.0.0.1:{relay_port}"
    l1, l2, l3 = node_factory.line_graph(
        3,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": f"ws://{url}",
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
    nwc = Nwc(NostrWalletConnectUri.parse(uri_str))
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
        invoice_decode["created_at"], abs=1
    )
    assert invoice_lookup.description_hash is None
    assert invoice_lookup.expires_at.as_secs() == pytest.approx(
        listpays_rpc["expires_at"], abs=1
    )
    assert invoice_lookup.fees_paid == 0
    assert invoice_lookup.metadata is None
    assert invoice_lookup.payment_hash == listpays_rpc["payment_hash"]
    assert invoice_lookup.transaction_type.name == "INCOMING"
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
        invoice_decode["created_at"], abs=1
    )
    assert invoice_lookup.description_hash is None
    assert invoice_lookup.expires_at.as_secs() == pytest.approx(
        listpays_rpc["expires_at"], abs=1
    )
    assert invoice_lookup.fees_paid == 0
    assert invoice_lookup.metadata is None
    assert invoice_lookup.payment_hash == listpays_rpc["payment_hash"]
    assert invoice_lookup.transaction_type.name == "INCOMING"
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
        invoice_decode["created_at"], abs=1
    )
    assert (
        invoice_lookup.description_hash == hashlib.sha256("test2".encode()).hexdigest()
    )
    assert invoice_lookup.expires_at.as_secs() == pytest.approx(
        listpays_rpc["expires_at"], abs=1
    )
    assert invoice_lookup.fees_paid == 0
    assert invoice_lookup.metadata is None
    assert invoice_lookup.payment_hash == listpays_rpc["payment_hash"]
    assert invoice_lookup.transaction_type.name == "INCOMING"
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
        invoice_decode["created_at"], abs=1
    )
    assert (
        invoice_lookup.description_hash == hashlib.sha256("test2".encode()).hexdigest()
    )
    assert invoice_lookup.expires_at.as_secs() == pytest.approx(
        listpays_rpc["expires_at"], abs=1
    )
    assert invoice_lookup.fees_paid == 0
    assert invoice_lookup.metadata is None
    assert invoice_lookup.payment_hash == listpays_rpc["payment_hash"]
    assert invoice_lookup.transaction_type.name == "INCOMING"
    assert invoice_lookup.settled_at.as_secs() == pytest.approx(
        listpays_rpc["paid_at"], abs=1
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
        invoice_decode["created_at"], abs=1
    )
    assert invoice_lookup.description_hash is None
    assert invoice_lookup.expires_at is None
    assert invoice_lookup.fees_paid == 1
    assert invoice_lookup.metadata is None
    assert invoice_lookup.payment_hash == listpays_rpc["payment_hash"]
    assert invoice_lookup.transaction_type.name == "OUTGOING"
    assert invoice_lookup.settled_at.as_secs() == pytest.approx(
        listpays_rpc["completed_at"], abs=1
    )


@pytest.mark.asyncio
async def test_list_transactions(node_factory, get_plugin, nostr_client):  # noqa: F811
    nostr_client, relay_port = nostr_client
    url = f"127.0.0.1:{relay_port}"
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": f"ws://{url}",
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
    nwc = Nwc(NostrWalletConnectUri.parse(uri_str))
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
    assert len(result) == 21
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
        tx.payment_hash is not None
        tx.fees_paid is not None


@pytest.mark.asyncio
async def test_notifications(node_factory, get_plugin, nostr_client):  # noqa: F811
    nostr_client, relay_port = nostr_client
    url = f"127.0.0.1:{relay_port}"
    l1, l2, l3 = node_factory.line_graph(
        3,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": f"ws://{url}",
            },
            {"log-level": "debug"},
            {"log-level": "debug"},
        ],
    )
    uri_str = l1.rpc.call("nip47-create", ["test1"])["uri"]
    LOGGER.info(uri_str)

    uri = NostrWalletConnectUri.parse(uri_str)
    nwc = Nwc(uri)

    invoice = l3.rpc.call(
        "invoice",
        {
            "label": generate_random_label(),
            "description": "test1",
            "amount_msat": 500000000,
        },
    )
    pay1 = await nwc.pay_invoice(
        PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
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
    pay2 = l3.rpc.call("pay", {"bolt11": result.invoice})
    invoice2_list = l1.rpc.call("listinvoices", {"invstring": result.invoice})[
        "invoices"
    ][0]
    invoice2_decode = l3.rpc.call("decode", [result.invoice])

    response_filter = Filter().kind(Kind(23196)).author(uri.public_key())
    events = await nostr_client.fetch_events(
        response_filter, timeout=timedelta(seconds=10)
    )
    start_time = datetime.now()
    while events.len() < 2 and (datetime.now() - start_time) < timedelta(seconds=10):
        time.sleep(1)
        events = await nostr_client.fetch_events(
            response_filter, timeout=timedelta(seconds=1)
        )
    assert events.len() == 2
    signer = NostrSigner.keys(Keys(uri.secret()))
    received_events = []
    sent_events = []
    for event in events.to_vec():
        LOGGER.info(event)
        content = await signer.nip04_decrypt(uri.public_key(), event.content())
        content = json.loads(content)
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
        invoice2_decode["created_at"], abs=1
    )
    assert "expires_at" not in received_events[0]["notification"]
    assert received_events[0]["notification"]["settled_at"] == pytest.approx(
        invoice2_list["paid_at"], abs=1
    )
    assert "metadata" not in received_events[0]["notification"]

    assert sent_events[0]["notification"]["type"] == "outgoing"
    assert sent_events[0]["notification"]["invoice"] == invoice["bolt11"]
    assert sent_events[0]["notification"]["description"] == "test1"
    assert "description_hash" not in sent_events[0]["notification"]
    assert sent_events[0]["notification"]["preimage"] == pay1.preimage
    assert (
        sent_events[0]["notification"]["payment_hash"] == invoice1_rpc["payment_hash"]
    )
    assert sent_events[0]["notification"]["amount"] == 500000000
    assert sent_events[0]["notification"]["fees_paid"] == 5001
    assert sent_events[0]["notification"]["created_at"] == pytest.approx(
        invoice1_decode["created_at"], abs=1
    )
    assert "expires_at" not in sent_events[0]["notification"]
    assert sent_events[0]["notification"]["settled_at"] == pytest.approx(
        pay1_list["completed_at"], abs=1
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

    invoice = l3.rpc.call(
        "invoice",
        {
            "label": generate_random_label(),
            "description": "test3",
            "amount_msat": 500,
        },
    )
    pay1 = await nwc.pay_invoice(
        PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
    )
    events = await nostr_client.fetch_events(
        response_filter, timeout=timedelta(seconds=10)
    )
    start_time = datetime.now()
    while (datetime.now() - start_time) < timedelta(seconds=10):
        time.sleep(1)
        events = await nostr_client.fetch_events(
            response_filter, timeout=timedelta(seconds=1)
        )
        assert events.len() == 2


@pytest.mark.asyncio
async def test_pay_invoice(node_factory, get_plugin, nostr_client):  # noqa: F811
    nostr_client, relay_port = nostr_client
    url = f"127.0.0.1:{relay_port}"
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": f"ws://{url}",
            },
            {"log-level": "debug"},
        ],
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 3001])["uri"]
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
async def test_multi_pay(node_factory, get_plugin, nostr_client):  # noqa: F811
    nostr_client, relay_port = nostr_client
    url = f"127.0.0.1:{relay_port}"
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": f"ws://{url}",
            },
            {"log-level": "debug"},
        ],
    )
    uri_str = l1.rpc.call("nip47-create", ["test1", 30000])["uri"]
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
    event = (
        await EventBuilder(Kind(23194), encrypted_content)
        .tags([Tag.public_key(uri.public_key())])
        .sign(signer)
    )
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(f"ws://{url}"))
    await client.connect()
    await client.send_event(event)

    response_filter = Filter().kind(Kind(23195)).author(uri.public_key())
    events = await client.fetch_events(response_filter, timeout=timedelta(seconds=10))
    start_time = datetime.now()
    while events.len() < 3 and (datetime.now() - start_time) < timedelta(seconds=10):
        time.sleep(1)
        events = await client.fetch_events(
            response_filter, timeout=timedelta(seconds=1)
        )
    assert events.len() == 3
    success_pays = []
    error_pays = []
    for event in events.to_vec():
        LOGGER.info(event)
        d_tag = event.tags().find(
            TagKind.SINGLE_LETTER(SingleLetterTag.lowercase(Alphabet.D))
        )
        content = await signer.nip44_decrypt(uri.public_key(), event.content())
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
async def test_persistency(node_factory, get_plugin, nostr_client):  # noqa: F811
    nostr_client, relay_port = nostr_client
    url = f"127.0.0.1:{relay_port}"
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": f"ws://{url}",
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
    nwc = Nwc(NostrWalletConnectUri.parse(uri_str))
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
    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice["bolt11"])
        )

    revoke = l1.rpc.call("nip47-revoke", ["test1"])
    assert revoke["revoked"] == "test1"

    uri_str = l1.rpc.call("nip47-create", ["test1", 3000, "10sec"])["uri"]
    nwc = Nwc(NostrWalletConnectUri.parse(uri_str))

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

    with pytest.raises(NostrSdkError.Generic, match="Payment exceeds budget"):
        await nwc.pay_invoice(
            PayInvoiceRequest(id=None, amount=None, invoice=invoice_exceeded["bolt11"])
        )

    time.sleep(11)

    list = l1.rpc.call("nip47-list", ["test1"])[0]
    assert list["test1"]["budget_msat"] == 3000


@pytest.mark.asyncio
async def test_budget_command(node_factory, get_plugin, nostr_client):  # noqa: F811
    nostr_client, relay_port = nostr_client
    url = f"127.0.0.1:{relay_port}"
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "nip47-relays": f"ws://{url}",
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
        "pay_invoice",
        "multi_pay_invoice",
        "pay_keysend",
        "multi_pay_keysend",
        "make_invoice",
        "lookup_invoice",
        "list_transactions",
        "get_balance",
        "get_info",
    ]

    signer = NostrSigner.keys(Keys(uri.secret()))
    client = Client(signer)
    await client.add_relay(RelayUrl.parse(f"ws://{url}"))
    await client.connect()

    response_filter = Filter().kind(Kind(13194)).author(uri.public_key())
    events = await client.fetch_events(response_filter, timeout=timedelta(seconds=10))
    start_time = datetime.now()
    while events.len() < 1 and (datetime.now() - start_time) < timedelta(seconds=10):
        time.sleep(1)
        events = await client.fetch_events(
            response_filter, timeout=timedelta(seconds=1)
        )
    assert events.len() == 1
    assert (
        events.to_vec()[0].content()
        == "pay_invoice multi_pay_invoice pay_keysend multi_pay_keysend make_invoice lookup_invoice list_transactions get_balance get_info"
    )

    time.sleep(16)

    balance = await nwc.get_balance()
    assert balance == 5000

    l1.rpc.call("nip47-budget", ["test1", 0])
    balance = await nwc.get_balance()
    assert balance == 0

    get_info = await nwc.get_info()
    assert get_info.methods == [
        "make_invoice",
        "lookup_invoice",
        "list_transactions",
        "get_balance",
        "get_info",
    ]

    events = await client.fetch_events(response_filter, timeout=timedelta(seconds=10))
    start_time = datetime.now()
    while events.len() < 1 and (datetime.now() - start_time) < timedelta(seconds=10):
        time.sleep(1)
        events = await client.fetch_events(
            response_filter, timeout=timedelta(seconds=1)
        )
    assert events.len() == 1
    assert (
        events.to_vec()[0].content()
        == "make_invoice lookup_invoice list_transactions get_balance get_info"
    )


@pytest_asyncio.fixture(scope="function")
async def nostr_client(nostr_relay):
    port = nostr_relay
    keys = Keys.generate()
    signer = NostrSigner.keys(keys)

    client = Client(signer)

    relay_url = RelayUrl.parse(f"ws://127.0.0.1:{port}")
    await client.add_relay(relay_url)
    await client.connect()

    yield client, port

    await client.disconnect()


@pytest_asyncio.fixture(scope="module")
async def nostr_relay(test_base_dir):
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    dynamic_port = s.getsockname()[1]
    s.close()

    try:
        config_file = pkg_resources.files("nostr_relay").joinpath("config.yaml")
    except KeyError:
        raise FileNotFoundError("config.yaml not found in the nostr package")

    with open(config_file, "r") as file:
        config = yaml.safe_load(file)

    config["gunicorn"]["bind"] = f"127.0.0.1:{dynamic_port}"
    config["authentication"]["valid_urls"] = [
        f"ws://localhost:{dynamic_port}",
        f"ws://127.0.0.1:{dynamic_port}",
    ]
    sqlite_file = Path(test_base_dir) / "nostr.sqlite3"
    config["storage"]["sqlalchemy.url"] = f"sqlite+aiosqlite:///{str(sqlite_file)}"
    config["storage"]["validators"] = [
        "nostr_relay.validators.is_signed",
        "nostr_relay.validators.is_recent",
        "nostr_relay.validators.is_not_hellthread",
    ]

    config_file = Path(test_base_dir) / "config.yaml"

    with open(config_file, "w") as file:
        yaml.safe_dump(config, file)

    LOGGER.info(f"{config_file}")
    process = subprocess.Popen(
        ["nostr-relay", "-c", config_file, "serve"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )

    stdout_thread = Thread(target=log_pipe, args=(process.stdout, LOGGER, logging.INFO))
    stderr_thread = Thread(
        target=log_pipe, args=(process.stderr, LOGGER, logging.ERROR)
    )
    stdout_thread.start()
    stderr_thread.start()

    time.sleep(2)

    yield dynamic_port

    process.terminate()
    process.wait()

    stdout_thread.join()
    stderr_thread.join()


def log_pipe(pipe, logger, log_level):
    while True:
        line = pipe.readline()
        if not line:
            break
        logger.log(log_level, line.strip())

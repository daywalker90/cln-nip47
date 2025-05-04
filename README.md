[![latest release on CLN v25.02](https://github.com/daywalker90/cln-nip47/actions/workflows/latest_v25.02.yml/badge.svg?branch=main)](https://github.com/daywalker90/cln-nip47/actions/workflows/latest_v25.02.yml) [![latest release on CLN v24.11](https://github.com/daywalker90/cln-nip47/actions/workflows/latest_v24.11.yml/badge.svg?branch=main)](https://github.com/daywalker90/cln-nip47/actions/workflows/latest_v24.11.yml) [![latest release on CLN v24.08.2](https://github.com/daywalker90/cln-nip47/actions/workflows/latest_v24.08.yml/badge.svg?branch=main)](https://github.com/daywalker90/cln-nip47/actions/workflows/latest_v24.08.yml)

[![main on CLN v25.02](https://github.com/daywalker90/cln-nip47/actions/workflows/main_v25.02.yml/badge.svg?branch=main)](https://github.com/daywalker90/cln-nip47/actions/workflows/main_v25.02.yml) [![main on CLN v24.11](https://github.com/daywalker90/cln-nip47/actions/workflows/main_v24.11.yml/badge.svg?branch=main)](https://github.com/daywalker90/cln-nip47/actions/workflows/main_v24.11.yml) [![main on CLN v24.08.2](https://github.com/daywalker90/cln-nip47/actions/workflows/main_v24.08.yml/badge.svg?branch=main)](https://github.com/daywalker90/cln-nip47/actions/workflows/main_v24.08.yml)

# cln-nip47
A core lightning plugin to connect wallets via Nostr Wallet Connect (NWC) as specified in [NIP-47](https://github.com/nostr-protocol/nips/blob/master/47.md).

* [Installation](#installation)
* [Building](#building)
* [Documentation](#documentation)

# Installation
For general plugin installation instructions see the plugins repo [README.md](https://github.com/lightningd/plugins/blob/master/README.md#Installation)

Release binaries for
* x86_64-linux
* armv7-linux (Raspberry Pi 32bit)
* aarch64-linux (Raspberry Pi 64bit)

can be found on the [release](https://github.com/daywalker90/cln-nip47/releases) page. If you are unsure about your architecture you can run ``uname -m``.

They require ``glibc>=2.31``, which you can check with ``ldd --version``.

# Building
You can build the plugin yourself instead of using the release binaries.
First clone the repo:

```
git clone https://github.com/daywalker90/cln-nip47.git
```

Install a recent rust version ([rustup](https://rustup.rs/) is recommended) and in the ``cln-nip47`` folder run:

```
cargo build --release
```

After that the binary will be here: ``target/release/cln-nip47``

Note: Release binaries are built using ``cross`` and the ``optimized`` profile.

# Documentation

## Receive-only NWC
If you want a receive-only NWC which also announces itself without any pay methods use `nip47-create` or `nip47-budget` and set `budget_msat` to `0`. Do NOT set an `interval` on these.

## Relays
It is highly recommended to use your own private relay since public relays may limit content length, amount of public keys per IP or require unsupported things like proof of work or payments. Each NWC you create is a separate public key and the ``list_transactions`` method can have quite a large content length! If you still want to use public relays, consider if you need nip47 notifications: if not, disable them with ``nip47-notifications=false``. This will reduce the amount of events send to the relay and maybe not get you rate limited as quickly.

For a private relay you can for example use [nostr-rs-relay](https://github.com/scsibug/nostr-rs-relay) with ``pubkey_whitelist`` set to both ``clientkey_public`` and ``walletkey_public`` (returned from ``nip47-create``/``nip47-list``).

## Options
* ``nip47-relays``: Specify the relays that you want to use with your NWC. Can be set multiple times to use multiple relays. NWC's you create will save these and even if you add or remove relays keep the relays from the moment you created that NWC. You must set this atleast one time.
* ``nip47-notifications``: Enable/disable nip47 notifications. Default is enabled (``true``)

## Methods
* **nip47-create** *label* [*budget_msat*] [*interval*]
     * create a new NWC string (`uri`) with the currently configured relays. For example: ``nip47-create mynwc 10000 1d`` will let you spend 10 satoshis every day using that NWC
     * ***label***: a label to identify this NWC
     * ***budget_msat***: optional. Set an absolute budget in msat that this NWC is allowed to use. This will also be your balance in your wallet. If you ***don't*** set this, the NWC will be allowed to use your ***whole*** node balance and show that aswell in your wallet! Set it to ``0`` to disable paying anything with this NWC
     * ***interval***: optional. Set an amount of time after which the budget will be refreshed ***to*** the amount specified in ``budget_msat``, e.g.:``5seconds`` or ``4weeks``. Supported time units are:
          * seconds: "second", "seconds", "sec", "secs", "s"
          * minutes: "minute", "minutes", "min", "mins", "m"
          * hours: "hour", "hours", "h"
          * days: "day", "days", "d"
          * weeks: "week", "weeks", "w"

* **nip47-revoke** *label*
     * revoke and remove all data related to a previously created NWC with ``label``
     * ***label***: the label the NWC was created with

* **nip47-budget** *label* [*budget_msat*] [*interval*]
     * update/add an existing NWC budget a new NWC string with the currently configured relays. For example: ``nip47-create mynwc 10000 1d`` will let you spend 10 satoshis every day using that NWC
     * ***label***: a label to identify this NWC
     * ***budget_msat***: optional. Set an absolute budget in msat that this NWC is allowed to use. This will also be your balance in your wallet. If you ***don't*** set this, the NWC will be allowed to use your ***whole*** node balance and show that aswell in your wallet! Set it to ``0`` to disable paying anything with this NWC
     * ***interval***: optional. Set an amount of time after which the budget will be refreshed ***to*** the amount specified in ``budget_msat``, e.g.:``5seconds`` or ``4weeks``. Supported time units are the same as in ``nip47-create``

* **nip47-list** [*label*]
     * list all NWC configurations or just the one with ``label``
     * ***label***: optional. The label the NWC was created with

## Supported NWC methods
* ``pay_invoice``
* ``multi_pay_invoice``
* ``pay_keysend`` (no ``preimage`` in request allowed since CLN only supports generating it itself)
* ``multi_pay_keysend`` (no ``preimage`` in request allowed since CLN only supports generating it itself)
* ``make_invoice``
* ``lookup_invoice``
* ``list_transactions``
* ``get_balance``
* ``get_info`` (no ``block_hash``)

## Supported NWC notifications
* ``payment_received``
* ``payment_sent``

## Supported content encryption:
* [NIP-04](https://github.com/nostr-protocol/nips/blob/master/04.md)
* [NIP-44v2](https://github.com/nostr-protocol/nips/blob/master/44.md)
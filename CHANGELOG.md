# Changelog

## Unreleased

### Added
- include pending and failed payments in ``list_transactions``, now that ``state`` exists wallets can display these more meaningfully
- include expired invoices in ``list_transactions``, now that ``state`` exists wallets can display these more meaningfully

### Changed
- upgrade ``nostr_sdk`` to ``v0.44`` and implement new fields in sync with the nip47 spec, e.g. ``state`` for transactions

### Fixed
- some minor fixes around bolt11 invoices with 0 amount (aka any amount)
- Don't ignore `offset` parameter in ``list_transactions``
- add ``notifications`` to the ``info_event``'s ``content`` if enabled

## [0.1.5] 2025-07-24

### Changed

- cap `list_transactions` to under 128kB since more can lead to incompatibilities with certain wallets

## [0.1.4] 2025-07-24

### Fixed
- no longer panic on missing both bolt11 and bolt12 strings from listpays response

## [0.1.3] 2025-05-04

### Changed
- NWC's with budget at 0 and no renewing interval set (aka "receive-only" NWC) will now stop announcing pay methods when announcing itself to relays, this is for services which demand a receive-only NWC (e.g. stacker.news)

## [0.1.2] 2025-04-18

### Added
- ``nip47-create`` and ``nip47-list``: add ``clientkey_public`` and ``walletkey_public`` to output. These are useful for private relay whitelists.
- ``nip47-notifications``: new option to enable/disable nip47 notifications. Usefule if you don't need them and want to use public relays that may rate limit you.

### Fixed
- ``nip47-revoke``: actually stop task if no relays were ever connected

## [0.1.1] 2025-04-07

### Changed
- use `xpay` instead of `pay` if CLN version supports it, this is a workaround for a library issue but also xpay is better

### Fixed
- added missing nip47-list labels

## [0.1.0] 2025-04-02

### Added
- initial release of cln-nip47

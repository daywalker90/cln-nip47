# Changelog

## [0.1.3] Unreleased

### Changed
- if your NWC does not have a renewing budget set with interval and the budget is 0 (aka "receive-only" NWC) it will now also not set any of the pay methods when announcing itself to relays, this is for services which demand a receive-only NWC (e.g. stacker.news) and determine it by the announced methods

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
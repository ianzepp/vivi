# Vivi 17.0.2

Security patch release on 17.0.1. No configuration or behavior changes.

## Root store: emSign ECC Root CA - G3 removed

In October 2026, the private keys of eight eMudhra intermediate certificates
were found to be trivially recoverable (Mozilla bug 2083449). Six of them are
TLS intermediates under **emSign ECC Root CA - G3**. Browsers and operating
systems blocked them within days, and Mozilla and Chrome removed the root.

Vivi's HTTPS clients use rustls with a root list compiled into the binary
(`webpki-roots`), not the operating system's store, and check no revocation.
So every Vivi binary up to 17.0.1 still trusted that root. That covers the
model proxy's calls to providers, sync, the judgment provider and pool
providers. Someone able to intercept that traffic could have presented a
certificate for any host chaining to the compromised keys.

17.0.2 updates `webpki-roots` to 1.0.10, which drops emSign ECC Root CA - G3.
The other emSign roots were not involved and stay. Sectigo's AAA Certificate
Services, which cross-signed the intermediates, was never in Vivi's list.

## Upgrade

Replace the binary and restart anything that runs it, above all a running
`vivi proxy`: a long-running process keeps the root list it started with.

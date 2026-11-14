# Vendored LocalSend protocol library

Source: https://github.com/CrossCopy/localsend-rs

Upstream main revision: `7e386ac916c2401a241c4a4d75672f7798227444` (0.2.0).
This includes upstream pull requests #4 (TLS), #6 (per-file receive progress),
and #7 (receive cancellation), plus their post-merge test fixes.

The vendored tree additionally includes the changes from draft pull request
[#8](https://github.com/CrossCopy/localsend-rs/pull/8), revision
`c84f20e7aa06063d97cc377a614284f4843ed409`, rebased on that main revision.
They expose TLS-authenticated client fingerprints to the application and bind
HTTPS discovery to the peer certificate. Client certificates are optional;
when present, the TLS handshake verifies possession of the private key.
Certificate expiry and self-signature are not treated as trust checks. A
fingerprint alone does not establish prior trust. Because the Web Share and API
use one TLS listener, browsers with client certificates may show a certificate
selection prompt.

The source, embedded browser assets, tests, manifest, README, agent guide,
license and third-party notices are included. The parent application supplies
the lockfile; upstream Git metadata, build output and Docker files are omitted.

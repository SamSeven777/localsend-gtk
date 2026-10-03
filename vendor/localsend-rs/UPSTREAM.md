# Vendored LocalSend protocol library

Source: https://github.com/CrossCopy/localsend-rs

Original pinned revision: `428981a012c77404e2605ae7b630b50bcbe8ce0f`

The library and its tests also include the source changes from upstream pull
request [#4](https://github.com/CrossCopy/localsend-rs/pull/4), head revision
`ef4daaf9d8b815731b9f7cea2a7fd02d5f2a46ac`. That pull request adds the
LocalSend v2.1 persistent TLS identity, handshake-time certificate pinning,
outgoing client certificates, and discovery compatibility used by this GTK
application. Its standalone Docker files and upstream lockfile are not
vendored because this repository builds the library through its own lockfile.

The source, embedded browser assets, test suite, package manifest, README, agent
guide, and third-party notices were copied from that revision. No Git metadata,
build output, dependency cache, or upstream lockfile is included.

The upstream package declares the MIT license in `Cargo.toml` and `README.md`.
That revision does not contain the `LICENSE` file mentioned in its README.
Existing source notices and `THIRD_PARTY_NOTICES.md` are retained unchanged.

Local modifications add a public local receive-cancellation API, active-owner
tracking in the existing receive lifecycle, and reservation checks preventing a
cancelled consent callback from reviving its session or clearing a later one.
Cancellation keeps the listener and established HTTP connections running. It
waits for an already-started publication before cancelling remaining files, then
waits for admitted receive owners, including ones not yet inside the sink. The
parent application also awaits its tracked sink cleanup before acknowledging
cancellation.

The cancellation hardening from upstream contribution
[#7](https://github.com/CrossCopy/localsend-rs/pull/7), commit
`fbaff1c8623b7063eb43673e3ca9ce368026d92a`, keeps a failed publication's receive
lease alive until sink cleanup finishes and emits a terminal session event only
from its owning completion/cancellation path. Its cleanup-ownership unit test
and five receiver-cancellation integration tests are included. These fixes
remain part of this vendored copy.
The integration test client initializes the vendored crate's explicit TLS
provider before constructing reqwest, matching the other vendored tests.

The HTTPS acceptor requests an optional LocalSend client certificate, validates
its complete DER encoding, validity period and self-signature, and lets rustls
verify proof of the private key during the handshake. Its authenticated SHA-256
fingerprint is attached to `PendingRequest`; register and prepare-upload reject a
body that claims a different fingerprint. The certificate stays optional so the
library's browser Web Share routes remain usable. Security-sensitive automatic
consent only runs when the authenticated fingerprint is present. HTTPS HTTP
discovery likewise derives the remote identity from the handshake leaf and
overrides the untrusted fingerprint in `/info` or `/register` JSON; plain HTTP
keeps its existing behavior.

Receive progress events also carry current-file bytes and size separately from
the existing session cumulative values. This lets the GTK client render accurate
per-file and total progress for multi-file sessions. Interoperability and
regression tests cover these local identity, cancellation and progress changes.
Two existing `fetch_update` calls carry a scoped deprecation allowance to keep
their upstream compiler compatibility when built on newer Rust toolchains.

Application-level regression tests in `src/network.rs` of the parent application
cover waiting consent, accepted sessions, streaming cleanup, publication races,
and a subsequent transfer using the same pooled HTTP client.

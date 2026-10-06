---
type: backlog
title: Switch crate publishing to crates.io trusted publishing
date: 2026-10-06
status: planned
priority: low
---

# Switch crate publishing to crates.io trusted publishing

The release pipeline (`ractive/release-workflows`, decision log
2026-10-06) publishes `kermit-proto` and `xmodem-proto` with the
repository secret `CARGO_TOKEN`. After the first version of both crates
is on crates.io, a trusted publisher can be configured per crate
(crates.io settings: repository `ractive/hptx`, workflow `release.yml`),
and the shared pipeline could take its token from
`rust-lang/crates-io-auth-action` instead of the secret. That is a change
in `ractive/release-workflows` for all its callers; trusted publishing
cannot be set up before a crate exists, which is why the token is needed
once.

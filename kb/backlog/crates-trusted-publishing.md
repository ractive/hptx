---
type: backlog
title: Switch crate publishing to crates.io trusted publishing
date: 2026-10-06
status: planned
priority: low
---

# Switch crate publishing to crates.io trusted publishing

`publish.yml` publishes `kermit-proto` and `xmodem-proto` with the
repository secret `CARGO_TOKEN` (decision log 2026-10-06). After
the first version of both crates is on crates.io, configure a trusted
publisher for each crate (crates.io settings: repository `ractive/hptx`,
workflow `publish.yml`, environment none), replace the token step with
`rust-lang/crates-io-auth-action` (`id-token: write` on the job, the
action's `token` output as `CARGO_REGISTRY_TOKEN`), and delete the secret.
Trusted publishing cannot be set up before a crate exists, which is why the
token is needed once.

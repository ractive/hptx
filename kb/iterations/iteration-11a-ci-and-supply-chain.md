---
type: iteration
title: "Iteration 11a: CI, supply chain and release dry run"
date: 2026-10-05
status: planned
tags:
  - iteration
  - infrastructure
branch: iter-11a/ci-and-supply-chain
---

# Iteration 11a: CI, supply chain and release dry run

Model: `~/devel/hyalo` (`deny.toml`, the SHA-pinned actions and the
`cargo-deny-action` step in its `quality-gates` job). Keep CI fast: the
check job must stay under about two minutes on a warm cache; record the
wall time before and after in this file.

## Tasks

- [ ] `deny.toml` after hyalo's: `[advisories]` with no ignores;
  `[licenses]` allow-list (MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception,
  BSD-2-Clause, BSD-3-Clause, Zlib, Unicode-3.0, plus whatever
  `cargo deny check licenses` shows the tree actually needs, each justified
  in a comment); `[bans] multiple-versions = "warn"`; `[sources]` crates.io
  only (`unknown-registry = "deny"`, `unknown-git = "deny"`,
  `allow-git = ["https://github.com/ractive/saturnus"]` for hptx-core's
  optional in-process transport); `[graph] all-features = true` so the
  optional saturnus subtree is checked and the allow-git entry matches
  something; a crate-scoped `[[licenses.exceptions]]` for `serialport`
  (MPL-2.0) with a comment, as saturnus's deny.toml does, rather than
  allowing MPL-2.0 globally.
- [ ] `EmbarkStudios/cargo-deny-action` (SHA-pinned, version in a comment)
  as a step of the `check` job; every other action in `ci.yml` SHA-pinned
  the same way (`actions/checkout`, `dtolnay/rust-toolchain`,
  `Swatinem/rust-cache`).
- [ ] `cargo deny check` in `just lint`; the CLAUDE.md gate line is
  proposed in the PR for the user to apply (agents do not edit CLAUDE.md).
- [ ] `--locked` on every CI cargo invocation.
- [ ] Windows and Linux: a matrix for the `check` job (ubuntu-latest,
  windows-latest, macos-latest) running fmt, clippy and the fast tests; the
  e2e job stays Linux-only. Fix what fails on Windows (history path, rustyline
  raw mode, serialport without libudev on Linux).
- [ ] Release pipeline as a dry run: a `release.yml` triggered by tags
  `v*` that builds `hptx` for macOS arm64 and x86_64, Linux x86_64 and
  Windows x86_64, runs the fast tests on each, and uploads the binaries as
  workflow artifacts; no GitHub Release is created and no tag is pushed in
  this iteration. `workflow_dispatch` only works once the file is on the
  default branch, so the workflow also triggers on `pull_request` with a
  `paths` filter on its own file: that is the dry run on the PR. Record the
  artifact sizes here.
- [ ] Decision-log entry: supply-chain policy and the release shape.

## Acceptance criteria

CI green on all three OSes; `cargo deny check` clean locally and in CI;
the release workflow runs green via `workflow_dispatch` and produces four
artifacts; check-job wall time recorded.

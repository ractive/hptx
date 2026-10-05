---
type: iteration
title: "Iteration 11a: CI, supply chain and release dry run"
date: 2026-10-05
status: in-progress
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

- [x] `deny.toml` after hyalo's: `[advisories]` with no ignores;
  `[licenses]` allow-list (MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception,
  BSD-2-Clause, BSD-3-Clause, Zlib, Unicode-3.0, plus whatever
  `cargo deny check licenses` shows the tree actually needs, each justified
  in a comment); `[bans] multiple-versions = "warn"`; `[sources]` crates.io
  only, `unknown-git = "deny"`, `allow-git = ["https://github.com/ractive/saturnus"]`
  (hptx-core's optional in-process transport); `[graph] all-features = true`
  so the saturnus subtree is checked; serialport's MPL-2.0 as a crate-scoped
  `[[licenses.exceptions]]`, not a global allowance.
- [x] `EmbarkStudios/cargo-deny-action` (SHA-pinned, version in a comment)
  as a step of the `check` job; every other action in `ci.yml` SHA-pinned
  the same way (`actions/checkout`, `dtolnay/rust-toolchain`,
  `Swatinem/rust-cache`).
- [x] `cargo deny check` in `just lint`. CLAUDE.md is the user's to change:
  the proposed gate line goes into the report, not the file.
- [x] `--locked` on every CI cargo invocation.
- [x] Windows and Linux: a matrix for the `check` job (ubuntu-latest,
  windows-latest, macos-latest) running fmt, clippy and the fast tests; the
  e2e job stays Linux-only. Fix what fails on Windows (history path, rustyline
  raw mode, serialport without libudev on Linux).
- [x] Release pipeline as a dry run: a `release.yml` triggered by tags
  `v*` that builds `hptx` for macOS arm64 and x86_64, Linux x86_64 and
  Windows x86_64, runs the fast tests on each, and uploads the binaries as
  workflow artifacts; no GitHub Release is created and no tag is pushed in
  this iteration. `workflow_dispatch` only works once the workflow is on the
  default branch, so it also triggers on a `pull_request` (and an `iter-*/**`
  push) that changes `release.yml`: that is the dry run. Record the artifact
  sizes here.
- [ ] Decision-log entry: supply-chain policy and the release shape.

## Acceptance criteria

CI green on all three OSes; `cargo deny check` clean locally and in CI;
the release workflow runs green (dry run on the branch) and produces four
artifacts; check-job wall time recorded.

## Results

CI and release runs on the branch (the `ci.yml` push trigger on
`iter-11a/**` was temporary, removed in the last commit):

- CI, all green: <https://github.com/ractive/hptx/actions/runs/37347483539>
  (check on ubuntu, macos and windows; e2e unchanged and green).
- Release dry run, green: <https://github.com/ractive/hptx/actions/runs/37347483545>.
  No GitHub Release, no tag (`contents: read`).

Check-job wall time (warm cache, job start to end):

| runner | before (main, ubuntu only) | after |
|---|---|---|
| ubuntu-latest | 33 s (run 37343141188), 43 s (run 37318363348) | 55 s |
| macos-latest | - | 45 s |
| windows-latest | - | 63 s |

The ubuntu increase is cargo-deny: building the action's Docker image
(about 13 s) and the check itself (about 11 s). The first, cold run on
Windows took 117 s.

Release artifacts (zipped size as GitHub stores it; unpacked binary):

| artifact | zipped | binary |
|---|---|---|
| hptx-aarch64-apple-darwin | 2.21 MB | 5.86 MB |
| hptx-x86_64-apple-darwin | 2.28 MB | 6.09 MB |
| hptx-x86_64-unknown-linux-gnu | 2.39 MB | 6.81 MB |
| hptx-x86_64-pc-windows-msvc | 2.14 MB | 5.48 MB |

Wall time per target: Linux 38 s, macOS arm64 60 s, macOS x86_64
(`macos-15-intel`) 96 s, Windows 163 s.

Platform fixes: one. `repl::history_path` checked `XDG_DATA_HOME` with
`Path::is_absolute`, which on a Windows host rejects `/data` (rooted, no
drive), so the Linux-rule unit test failed there; it now uses `has_root`
(identical on Linux). rustyline raw mode and serialport needed nothing;
ubuntu builds with no extra apt packages (serialport is built without
libudev).

cargo-deny (`cargo deny check`, all features): advisories ok, bans ok,
licenses ok, sources ok; also clean with the saturnus feature off.
Licences beyond hyalo's list: BSL-1.0 (clipboard-win and error-code,
Windows-only via rustyline) allowed globally, MPL-2.0 for serialport only;
BSD-3-Clause dropped (nothing uses it). Duplicate warnings: nix 0.26/0.31,
windows-sys 0.52/0.61, bitflags 1/2 and miniz_oxide 0.8/0.9, all from
serialport 4.10 and png 0.18.

Decision-log entry, proposed (the kb PR records it): "Supply chain and
release shape. cargo-deny gates every PR and push to main (advisories with
no ignores; crates.io plus the saturnus repository only; permissive
licences plus serialport's MPL-2.0 as a crate exception; duplicates warn).
Actions are SHA-pinned and CI cargo runs `--locked`. The release workflow
builds hptx for macOS arm64 and x86_64, Linux x86_64 and Windows x86_64,
tests each and uploads artifacts; publishing (GitHub Release, crates.io,
installers) is a later iteration."

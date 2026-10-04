---
title: "Iteration 1: Skeleton and CI"
type: iteration
date: 2026-10-04
status: completed
branch: iter-1/skeleton-and-ci
tags:
  - iteration
  - hptx
---

# Iteration 1: Skeleton and CI

## Tasks

- [x] Cargo workspace with the four crates (empty libs, `hptx --version`).
- [x] `justfile`: `test`, `e2e`, `lint` (clippy + fmt), `emulator-up/down`.
- [x] GitHub Actions: job 1 fmt+clippy+`cargo test`; job 2 builds the
  `emulator/` image for the 48SX, starts it, waits for the `bridged` log
  line, runs `cargo test -p hptx-core --test e2e` with `HPTX_E2E_ADDR`.
  Cache cargo, not the Docker image (it contains ROMs).
- [x] README stub. Acceptance: CI green on both jobs.

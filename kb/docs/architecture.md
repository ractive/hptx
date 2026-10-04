---
title: Architecture and goal
type: docs
date: 2026-10-04
status: active
tags:
  - architecture
  - hptx
---

# Architecture and goal

Status: skeleton only (2026-10-04). Nothing built yet except `emulator/`.

## Goal

`hptx` lets a person or an AI agent list, fetch, store, run and back up
objects on an HP48/49 (later 38/39/40) over a USB-serial adapter, from macOS,
Windows and Linux. A GUI follows once the CLI is solid.

## Architecture

```text
kermit-proto   sans-IO Kermit state machine (generic, publishable)
xmodem-proto   sans-IO XModem (+1k, CRC, HP variants, XSERV framing)
hptx-core      HP layer: server commands, object format, IOPAR, GROB,
               transports (serial via `serialport`, TCP, in-memory)
hptx-cli       binary `hptx`
```

Sans-IO seam, same for both protocol crates:

```text
fn handle_input(&mut self, bytes: &[u8]);
fn handle_timeout(&mut self, now: Instant);
fn poll_output(&mut self) -> Option<Vec<u8>>;
fn poll_event(&mut self) -> Option<Event>;   // FileStart, Data, FileEnd, Error, Done
fn next_timeout(&self) -> Option<Instant>;
```

Files cross the boundary as events and chunks. The only link property the
protocol must know is whether the link is 8-bit clean (Kermit prefixing);
baud, parity and port names belong to the transport.

## Milestones

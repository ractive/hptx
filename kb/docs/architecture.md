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

Status (2026-10-04): `kermit-proto` client implemented (iteration 2);
`xmodem-proto`, `hptx-core` and `hptx-cli` are still skeletons.

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

Sans-IO seam, same for both protocol crates (as built in `kermit-proto`):

```text
fn start(&mut self, now: Instant, command: Command) -> Result<(), Busy>;
fn handle_input(&mut self, now: Instant, bytes: &[u8]);
fn handle_timeout(&mut self, now: Instant);
fn poll_output(&mut self, now: Instant) -> Option<Vec<u8>>;  // one packet, write atomically
fn poll_event(&mut self) -> Option<Event>;
fn next_timeout(&self) -> Option<Instant>;
```

Every call that can queue output takes `now`, so the state machine owns
retransmit deadlines and the inter-packet pause without reading a clock.
Kermit events: `FileStart`, `Data`, `Progress`, `FileEnd`, `ServerText`,
`Error`, `Done`. Names, file data and server text are raw bytes in the
calculator's character set; `hptx-core` translates.

Files cross the boundary as events and chunks. The only link property the
protocol must know is whether the link is 8-bit clean (Kermit prefixing);
baud, parity and port names belong to the transport. Discarding stale input
for about 0.5 s after connecting is the transport's job.

## Milestones

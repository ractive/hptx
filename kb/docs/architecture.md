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

Status (2026-10-05): `kermit-proto` client implemented (iteration 2);
`hptx-core` implemented (iteration 3); `xmodem-proto` and `hptx-cli` are
still skeletons.

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

`hptx-core` layers, bottom up:

```text
transport   Transport trait: write one packet in one write, read with a
            timeout. Serial (9600 8N1, no flow control, DTR and RTS on),
            TCP (tcp://host:port), in-memory (closure or trace replay).
session     Session: drives one kermit-proto Client over a transport,
            drains stale input for 0.5 s on connect, maps errors.
charset     HP character set <-> UTF-8; ASCII trigraphs (\->, \<<).
reply       parsers for G D listings and C stack text (all three models).
object      HPHP48-x/HPHP49-x headers, prolog table, object length walk,
            %%HP: ASCII header.
grob        GROB decode/encode, PNG export.
calc        Calculator: list, cd, get/put (sets flag -35), run, mkdir,
            remove, rename, IOPAR, screenshot, backup, restore.
```

Since iteration 14 (decision log 2026-10-10) the session, calc and xmodem
logic is async code on a crate-private mailbox (`link`) that never reads a
clock or touches a port. `machine::Machine` drives it sans-I/O (fed bytes
and time; for a browser with Web Serial, wasm32 without the default
`native` feature); `Session`, `Calculator` and `XmodemSession` drive it
over a `Transport` with the system clock. Serial and TCP need `native`
(default), the in-process emulator `saturnus`.

Host commands (`C`) return the stack as display text and leave their
results on the user's stack, so every internal query drops what it pushed;
names are checked in a `G D` listing before they are evaluated (decision
log, iteration 3).

Files cross the boundary as events and chunks. The only link property the
protocol must know is whether the link is 8-bit clean (Kermit prefixing);
baud, parity and port names belong to the transport. Discarding stale input
for about 0.5 s after connecting is the transport's job.

## Milestones

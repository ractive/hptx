---
title: Knowledge sources
type: docs
date: 2026-10-04
status: active
tags:
  - sources
  - hptx
---

# Knowledge sources

- `~/devel/hp-literature/` is an LLM wiki about the calculators. Query it with
  `hyalo` from that directory (`hyalo summary`, `hyalo read index.md`,
  `hyalo find "kermit server"`). Protocol pages: `protocols/kermit`,
  `kermit-hp`, `server-commands`, `iopar`, `hp-object-format`, `xmodem`,
  `xmodem-hp`, `xserv`. Cite them in code comments as `wiki: protocols/kermit-hp`.
  New calculator facts go into that wiki (it has its own CLAUDE.md), not only
  into code comments.
- `~/devel/hpcomm/` is the 1999-2001 HPComm C++ source (GPL, co-owned by HP).
  Read `hpcomm/Kermit.cpp`, `Prot.cpp`, `Filer*.cpp`, `hpgcomm/XModem.cpp` to
  learn HP behaviour. Never copy or closely translate it: hptx is MIT.
- `emulator/` in this repo runs saturnng in Docker with the calculator's
  serial port on TCP 4848 (HP48SX, 48GX, 49G). See `emulator/README.md`. The
  image contains HP ROMs: never push it to a registry.
- `~/devel/saturnus/` is the sibling project, a from-scratch emulator that
  hptx will talk to in-process once its UART works.

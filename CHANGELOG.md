# Changelog

All notable changes to hptx are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- hptx-core: `Machine`, a sans-I/O face for every calculator operation
  (`Op`, `Reply`, `Progress`): fed bytes, the caller's time and link
  errors, it hands out bytes to write, progress events and results. It
  never blocks, reads a clock or touches a port, so it runs where bytes
  arrive by callback (a browser with Web Serial).
- hptx-core: a `native` feature (default) for the serial port and TCP
  transports; without it hptx-core builds for `wasm32-unknown-unknown`
  and pulls in neither `serialport` nor `saturnus`.
- hptx-core: `time` module (`Instant`, `Duration`; `web_time` on wasm32).
- hptx-core: README with the embedder API.
- CI: wasm32 build and clippy of hptx-core without default features
  (`just wasm` locally).

### Changed

- hptx-core: `Session`, `Calculator` and `XmodemSession` are thin blocking
  loops over the same protocol code as `Machine`; their API and behaviour
  are unchanged. A link error ends the current transaction only; the next
  one tries the transport again, as before.
- hptx-core: `Error::Serial`, `TcpTransport` and `SerialTransport` need the
  `native` feature; without it `transport::open` refuses serial and TCP
  addresses with `Error::Address`.
- hptx-cli: builds hptx-core with `native` and `saturnus`, so installed and
  released binaries open `saturnus://[MODEL@]ROM` addresses.

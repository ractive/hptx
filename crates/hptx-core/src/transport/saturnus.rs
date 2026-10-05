//! In-process link to the `saturnus` HP 48SX emulator (feature `saturnus`).
//!
//! The transport owns a [`saturnus::Machine`] and runs it in emulated time,
//! single-threaded, with no sockets:
//!
//! - [`Transport::read`] runs the machine until it has transmitted
//!   something and the line has gone quiet, or until `timeout` worth of
//!   emulated time has passed. On a timeout it then sleeps for whatever is
//!   left of `timeout` in wall-clock time, so the host's own deadlines
//!   (wall-clock `Instant`s in `kermit-proto`) see the same idle period the
//!   calculator saw, instead of a burst of many emulated timeouts.
//! - [`Transport::write_packet`] first runs the machine for the wall time
//!   the host spent outside the transport (at most [`MAX_CATCH_UP`]): the
//!   host's turnaround pause between transactions is time the calculator
//!   needs to get ready for the next command (calculator quirk: a command
//!   right after the final ACK is lost). Then it queues the packet, which
//!   the emulated UART receives at line rate.
//!
//! Opening boots the ROM, answers "Try To Recover Memory?" with NO and types
//! `SERVER`, like the saturnng container's `AUTOSTART`, then waits for the
//! idle server's first NAK as proof that the server runs. A ROM that never
//! reaches the prompt or never starts the server is [`Error::Emulator`].

use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use saturnus::io::Key;
use saturnus::{Machine, Model};

use super::Transport;
use crate::{Error, Result};

/// Longest wall-clock gap between transport calls that is replayed as
/// emulated time before a write.
pub const MAX_CATCH_UP: Duration = Duration::from_secs(2);
/// Emulated time run per step while waiting.
const STEP: Duration = Duration::from_millis(1);
/// Emulated quiet time after the last transmitted byte that ends a read: a
/// byte takes 1.2 ms at 9600 baud and the HP sends a packet back to back.
const QUIET: Duration = Duration::from_millis(4);
/// How long a key is held.
const KEY_HOLD: Duration = Duration::from_millis(60);
/// LCD stable, CPU in SHUTDN, this long: the ROM waits for a key.
const IDLE_STABLE: Duration = Duration::from_millis(300);
/// Longest wait for the boot prompt.
const BOOT_CAP: Duration = Duration::from_secs(60);
/// Longest wait for the ROM to settle after a key.
const KEY_CAP: Duration = Duration::from_secs(10);
/// Time the ROM gets after ENTER to start the server and draw its banner.
const SERVER_SETTLE: Duration = Duration::from_secs(1);
/// Longest emulated wait for the idle server's first NAK (it times out
/// after about 5 s).
const SERVER_CAP: Duration = Duration::from_secs(15);

/// The HP 48SX emulated in-process.
pub struct SaturnusTransport {
    machine: Machine,
    cycles_per_sec: u64,
    /// Transmitted bytes not handed to the host yet.
    pending: VecDeque<u8>,
    /// When the host last returned from a transport call.
    last_call: Instant,
}

impl std::fmt::Debug for SaturnusTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaturnusTransport")
            .field("cycles", &self.machine.cycles())
            .field("pending", &self.pending.len())
            .finish()
    }
}

fn emulator(e: impl std::fmt::Display) -> Error {
    Error::Emulator(e.to_string())
}

fn halt(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("emulated CPU halted: {e}"))
}

impl SaturnusTransport {
    /// Build an HP 48SX from the packed ROM image at `rom`, boot it and start
    /// the Kermit server.
    pub fn open(rom: &Path) -> Result<Self> {
        let image = std::fs::read(rom)
            .map_err(|e| Error::Emulator(format!("cannot read ROM {}: {e}", rom.display())))?;
        let mut t = Self::new(Machine::new(Model::Hp48sx, &image).map_err(emulator)?);
        t.autostart().map_err(|e| {
            Error::Emulator(format!(
                "{} did not start the Kermit server: {e}",
                rom.display()
            ))
        })?;
        t.last_call = Instant::now();
        Ok(t)
    }

    /// Wrap an already running machine (in server mode, or not: then the
    /// caller drives it through [`SaturnusTransport::machine`]).
    pub fn new(machine: Machine) -> Self {
        let cycles_per_sec = u64::from(machine.model().clock_hz());
        SaturnusTransport {
            machine,
            cycles_per_sec,
            pending: VecDeque::new(),
            last_call: Instant::now(),
        }
    }

    /// The emulated calculator.
    pub fn machine(&mut self) -> &mut Machine {
        &mut self.machine
    }

    fn cycles(&self, d: Duration) -> u64 {
        let c = d.as_nanos() * u128::from(self.cycles_per_sec) / 1_000_000_000;
        u64::try_from(c).unwrap_or(u64::MAX)
    }

    /// Run `n` cycles and collect what the calculator transmitted. Returns
    /// whether it transmitted anything.
    fn run(&mut self, n: u64) -> io::Result<bool> {
        self.machine.run_cycles(n).map_err(halt)?;
        let out = self.machine.serial_drain();
        self.pending.extend(&out);
        Ok(!out.is_empty())
    }

    /// Run `d` of emulated time.
    fn run_for(&mut self, d: Duration) -> io::Result<()> {
        let n = self.cycles(d);
        self.run(n).map(|_| ())
    }

    /// Run until the LCD has not changed for [`IDLE_STABLE`] with the CPU
    /// in SHUTDN (`true`), or for `cap` (`false`).
    fn wait_idle(&mut self, cap: Duration) -> io::Result<bool> {
        let step = self.cycles(Duration::from_millis(2));
        let stable = self.cycles(IDLE_STABLE);
        let end = self.machine.cycles().saturating_add(self.cycles(cap));
        let mut last = self.machine.lcd();
        let mut since = self.machine.cycles();
        while self.machine.cycles() < end {
            self.run(step)?;
            let lcd = self.machine.lcd();
            let now = self.machine.cycles();
            if lcd != last {
                last = lcd;
                since = now;
            } else if now - since >= stable && self.machine.is_shutdown() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Press and release `key`, then wait for the ROM to settle.
    fn press(&mut self, key: Key, name: &str) -> io::Result<()> {
        self.machine.key_down(key);
        self.run_for(KEY_HOLD)?;
        self.machine.key_up(key);
        if self.wait_idle(KEY_CAP)? {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "not idle {} s after pressing {name}",
                KEY_CAP.as_secs()
            )))
        }
    }

    /// Run until the calculator has sent a Kermit NAK, at most `cap`.
    fn wait_for_nak(&mut self, cap: Duration) -> io::Result<bool> {
        let step = self.cycles(Duration::from_millis(10));
        let end = self.machine.cycles().saturating_add(self.cycles(cap));
        while self.machine.cycles() < end {
            self.run(step)?;
            let bytes: Vec<u8> = self.pending.iter().copied().collect();
            // SOH, LEN, SEQ, TYPE: a NAK has type `N`.
            if bytes.windows(4).any(|w| w[0] == 0x01 && w[3] == b'N') {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Answer the boot prompt with NO, then ALPHA ALPHA S E R V E R ENTER.
    /// Then wait for the idle server's first NAK.
    fn autostart(&mut self) -> io::Result<()> {
        if !self.wait_idle(BOOT_CAP)? {
            return Err(io::Error::other(format!(
                "no boot prompt within {} s",
                BOOT_CAP.as_secs()
            )));
        }
        self.press(Key::F, "NO (softkey F)")?;
        // 48SX alpha letters: S = SIN, E = softkey E, R = right arrow,
        // V = square root.
        for (key, name) in [
            (Key::Alpha, "ALPHA"),
            (Key::Alpha, "ALPHA"),
            (Key::Sin, "S"),
            (Key::E, "E"),
            (Key::Right, "R"),
            (Key::Sqrt, "V"),
            (Key::E, "E"),
            (Key::Right, "R"),
        ] {
            self.press(key, name)?;
        }
        self.machine.key_down(Key::Enter);
        self.run_for(KEY_HOLD)?;
        self.machine.key_up(Key::Enter);
        self.run_for(SERVER_SETTLE)?;
        if !self.wait_for_nak(SERVER_CAP)? {
            return Err(io::Error::other(format!(
                "no Kermit NAK within {} s of SERVER",
                SERVER_CAP.as_secs()
            )));
        }
        self.pending.clear();
        Ok(())
    }

    /// Replay the wall time the host spent outside the transport.
    fn catch_up(&mut self) -> io::Result<()> {
        let gap = self.last_call.elapsed().min(MAX_CATCH_UP);
        self.run_for(gap)
    }

    fn take(&mut self, buf: &mut [u8]) -> usize {
        let n = buf.len().min(self.pending.len());
        for (dst, src) in buf.iter_mut().zip(self.pending.drain(..n)) {
            *dst = src;
        }
        n
    }
}

impl Transport for SaturnusTransport {
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<()> {
        self.catch_up()?;
        self.machine.serial_push(packet);
        self.last_call = Instant::now();
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let start = Instant::now();
        let step = self.cycles(STEP);
        let quiet = self.cycles(QUIET);
        let end = self.machine.cycles().saturating_add(self.cycles(timeout));
        let mut last_byte = self.machine.cycles();
        while self.pending.len() < buf.len() && self.machine.cycles() < end {
            // Input still on its way in means the calculator has more to
            // answer; otherwise stop once its output has gone quiet.
            if !self.pending.is_empty()
                && self.machine.cycles() - last_byte >= quiet
                && self.machine.serial_pending() == 0
            {
                break;
            }
            if self.run(step)? {
                last_byte = self.machine.cycles();
            }
        }
        let n = self.take(buf);
        if n == 0 {
            std::thread::sleep(timeout.saturating_sub(start.elapsed()));
        }
        self.last_call = Instant::now();
        Ok(n)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn zero_rom_is_an_emulator_error() {
        let dir = std::env::temp_dir().join(format!("hptx-zero-rom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rom = dir.join("zero-rom");
        std::fs::write(&rom, vec![0u8; Model::Hp48sx.rom_bytes()]).unwrap();
        let result = SaturnusTransport::open(&rom);
        std::fs::remove_dir_all(&dir).unwrap();
        match result {
            Err(Error::Emulator(msg)) => {
                assert!(msg.contains("did not start the Kermit server"), "{msg}");
            }
            other => panic!("expected Error::Emulator, got {other:?}"),
        }
    }

    #[test]
    fn short_rom_is_an_emulator_error() {
        let dir = std::env::temp_dir().join(format!("hptx-short-rom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rom = dir.join("short-rom");
        std::fs::write(&rom, [0u8; 100]).unwrap();
        let result = SaturnusTransport::open(&rom);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(matches!(result, Err(Error::Emulator(_))), "{result:?}");
    }
}

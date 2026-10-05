//! In-process link to the `saturnus` emulator (feature `saturnus`): an HP
//! 48SX, 48GX or 49G.
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
//! Opening boots the ROM and types `SERVER` with the model's key script
//! from [`saturnus_drive::autostart`] (the same choreography as the
//! saturnng container's `AUTOSTART`), then waits for the idle server's
//! first NAK as proof that the server runs. A model without a serial port
//! or a Kermit server, a ROM that never reaches its boot prompt or never
//! starts the server is [`Error::Emulator`].

use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use saturnus::{Machine, Model};
use saturnus_drive::autostart::autostart_script;
use saturnus_drive::script::Action;
use saturnus_drive::session::Session;

use super::{EmulatorModel, Transport};
use crate::{Error, Result};

/// Longest wall-clock gap between transport calls that is replayed as
/// emulated time before a write.
pub const MAX_CATCH_UP: Duration = Duration::from_secs(2);
/// Emulated time run per step while waiting.
const STEP: Duration = Duration::from_millis(1);
/// Emulated quiet time after the last transmitted byte that ends a read: a
/// byte takes 1.2 ms at 9600 baud and the HP sends a packet back to back.
const QUIET: Duration = Duration::from_millis(4);
/// Longest emulated wait for the idle server's first NAK (it times out
/// after about 5 s).
const SERVER_CAP: Duration = Duration::from_secs(15);

/// A saturnus calculator emulated in-process.
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
            .field("model", &self.machine.model())
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

/// The saturnus model for `model`, or [`Error::Emulator`] when hptx cannot
/// talk to it: the 42S has no serial port, the 38G, 39G and 40G have no
/// Kermit server command (their PC link is started on the calculator).
pub fn boot_model(model: EmulatorModel) -> Result<Model> {
    let found = Model::ALL
        .into_iter()
        .find(|m| m.name() == model.name())
        .ok_or_else(|| {
            Error::Emulator(format!(
                "saturnus does not emulate the {}",
                model.name().to_uppercase()
            ))
        })?;
    let upper = found.name().to_uppercase();
    if !found.has_serial() {
        return Err(Error::Emulator(format!(
            "the {upper} has no serial port; saturnus:// boots the 48SX, 48GX or 49G"
        )));
    }
    match found {
        Model::Hp48sx | Model::Hp48gx | Model::Hp49g => Ok(found),
        _ => Err(Error::Emulator(format!(
            "the {upper} has no Kermit server command; saturnus:// boots the 48SX, 48GX or 49G"
        ))),
    }
}

impl SaturnusTransport {
    /// Build `model` from the ROM image at `rom`, boot it and start the
    /// Kermit server.
    pub fn open(model: EmulatorModel, rom: &Path) -> Result<Self> {
        let model = boot_model(model)?;
        let image =
            saturnus_drive::rom::load(model, rom).map_err(|e| Error::Emulator(format!("{e:#}")))?;
        let machine = Machine::new(model, &image).map_err(emulator)?;
        let start_failed = |e: String| {
            Error::Emulator(format!(
                "{} as the {} did not start the Kermit server: {e}",
                rom.display(),
                model.name().to_uppercase()
            ))
        };
        let machine = autostart(machine).map_err(start_failed)?;
        let mut t = Self::new(machine);
        t.wait_for_server()
            .map_err(|e| start_failed(e.to_string()))?;
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

    /// Run until the calculator has sent a Kermit NAK (at most
    /// [`SERVER_CAP`]), then drop what it sent.
    fn wait_for_server(&mut self) -> io::Result<()> {
        let step = self.cycles(Duration::from_millis(10));
        let end = self
            .machine
            .cycles()
            .saturating_add(self.cycles(SERVER_CAP));
        while self.machine.cycles() < end {
            self.run(step)?;
            // SOH, LEN, SEQ, TYPE: a NAK has type `N`.
            let nak = self
                .pending
                .iter()
                .zip(self.pending.iter().skip(3))
                .any(|(&soh, &ty)| soh == 0x01 && ty == b'N');
            if nak {
                self.pending.clear();
                return Ok(());
            }
        }
        Err(io::Error::other(format!(
            "no Kermit NAK within {} s of SERVER",
            SERVER_CAP.as_secs()
        )))
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

/// Cold-boot `machine` and type `SERVER` with the model's autostart script.
/// The boot prompt not showing up within the script's cap is an error;
/// a key after it that leaves the screen busy is only noted (the NAK wait
/// decides), as the saturnus CLI does.
fn autostart(machine: Machine) -> std::result::Result<Machine, String> {
    let script = autostart_script(machine.model(), true).map_err(|e| format!("{e:#}"))?;
    let mut session = Session::new(machine, 0, false);
    session.set_echo_warnings(false);
    session.check_keys(&script).map_err(|e| format!("{e:#}"))?;
    for line in &script {
        session.apply(line).map_err(|e| format!("{e:#}"))?;
        if let Action::WaitIdle { cap_ms } = line.action
            && !session.take_warnings().is_empty()
        {
            return Err(format!("no boot prompt within {} s", cap_ms / 1000));
        }
    }
    Ok(session.machine)
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

    fn temp_rom(tag: &str, bytes: &[u8]) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("hptx-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rom = dir.join("rom");
        std::fs::write(&rom, bytes).unwrap();
        (dir, rom)
    }

    #[test]
    fn zero_rom_is_an_emulator_error() {
        let (dir, rom) = temp_rom("zero-rom", &vec![0u8; Model::Hp48sx.rom_bytes()]);
        let result = SaturnusTransport::open(EmulatorModel::Hp48sx, &rom);
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
        let (dir, rom) = temp_rom("short-rom", &[0u8; 100]);
        for model in [
            EmulatorModel::Hp48sx,
            EmulatorModel::Hp48gx,
            EmulatorModel::Hp49g,
        ] {
            let result = SaturnusTransport::open(model, &rom);
            match result {
                Err(Error::Emulator(msg)) => assert!(msg.contains("100 bytes"), "{msg}"),
                other => panic!("expected Error::Emulator, got {other:?}"),
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn models_without_a_kermit_server_are_refused() {
        // Refused before the ROM is read: no ROM file needed.
        let rom = Path::new("/nonexistent/rom");
        for (model, why) in [
            (EmulatorModel::Hp42s, "the 42S has no serial port"),
            (EmulatorModel::Hp38g, "the 38G has no Kermit server"),
            (EmulatorModel::Hp39g, "the 39G has no Kermit server"),
            (EmulatorModel::Hp40g, "the 40G has no Kermit server"),
        ] {
            match SaturnusTransport::open(model, rom) {
                Err(Error::Emulator(msg)) => assert!(msg.starts_with(why), "{msg}"),
                other => panic!("expected Error::Emulator for {model:?}, got {other:?}"),
            }
        }
        for model in [
            EmulatorModel::Hp48sx,
            EmulatorModel::Hp48gx,
            EmulatorModel::Hp49g,
        ] {
            assert_eq!(boot_model(model).unwrap().name(), model.name());
        }
    }
}

//! End-to-end tests against the saturnng emulator in `emulator/`.
//!
//! Skipped unless `HPTX_E2E_ADDR` is set, e.g. `tcp://localhost:4848`. Must
//! pass on the 48SX, 48GX and 49G. Every scenario leaves the calculator in
//! HOME and in ASCII transfer mode, as a fresh one is.
//!
//! The XModem scenario also needs `HPTX_E2E_CONTAINER`, the name of the
//! emulator's docker container: XRECV/XSEND cannot be started through the
//! Kermit server, so it types them with `docker exec CONTAINER calc-keys`.
//! It runs only on a detected 49G or 48G/GX.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hptx_core::object::{self, Family, HEADER_LEN, ObjectType};
use hptx_core::reply::{Iopar, parse_real};
use hptx_core::xmodem::XmodemDirection;
use hptx_core::xmodem_proto::{Check, Event};
use hptx_core::{
    Calculator, Error, Model, Options, Session, TransferMode, XmodemOptions, XmodemSession,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// One emulator, one client at a time.
static LOCK: Mutex<()> = Mutex::new(());

/// Variables a scenario may create in HOME.
const LEFTOVERS: &[&str] = &[
    "HPTXE2E", "HPTXE2F", "HPTXRT", "HPTXTMP", "HPTXBK", "HPTXXM",
];

/// Run `body` against a fresh connection, then clean up whatever happened.
/// Does nothing when `HPTX_E2E_ADDR` is unset.
fn scenario(body: impl FnOnce(&mut Calculator) -> TestResult) -> TestResult {
    let Some(addr) = std::env::var("HPTX_E2E_ADDR")
        .ok()
        .filter(|a| !a.is_empty())
    else {
        return Ok(());
    };
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut calc = Calculator::open(&addr)?;
    let result = body(&mut calc);
    let cleanup = cleanup(&mut calc);
    result?;
    cleanup
}

/// Back to HOME and ASCII mode, remove leftovers.
fn cleanup(calc: &mut Calculator) -> TestResult {
    calc.cd(&["HOME"])?;
    let listing = calc.list()?;
    for name in LEFTOVERS {
        if listing.entries.iter().any(|e| e.name == *name) {
            eprintln!("cleanup: removing {name}");
            calc.remove(name)?;
        }
    }
    calc.set_transfer_mode(TransferMode::Ascii)?;
    Ok(())
}

fn names(calc: &mut Calculator) -> Result<Vec<String>, Error> {
    Ok(calc.list()?.entries.into_iter().map(|e| e.name).collect())
}

#[test]
fn ls() -> TestResult {
    scenario(|calc| {
        calc.mkdir("HPTXE2E")?;
        calc.cd(&["HOME", "HPTXE2E"])?;
        assert_eq!(calc.path()?, ["HOME", "HPTXE2E"]);
        assert!(calc.list()?.entries.is_empty());
        let stored = calc.put(
            "HPTXV",
            b"%%HP: T(3)A(D)F(.);\r\n42\r\n",
            TransferMode::Ascii,
        )?;
        assert_eq!(stored, "HPTXV");
        assert_eq!(names(calc)?, ["HPTXV"]);
        calc.cd(&["HOME"])?;
        calc.rename("HPTXE2E", "HPTXE2F")?;
        let listing = calc.list()?;
        assert!(
            listing
                .entries
                .iter()
                .any(|e| e.name == "HPTXE2F" && e.is_directory()),
            "{listing:?}"
        );
        assert!(!listing.entries.iter().any(|e| e.name == "HPTXE2E"));
        calc.remove("HPTXE2F")?;
        assert!(!names(calc)?.iter().any(|n| n == "HPTXE2F"));
        Ok(())
    })
}

#[test]
fn get() -> TestResult {
    scenario(|calc| {
        let ascii = calc.get("IOPAR", TransferMode::Ascii)?;
        assert!(
            ascii.starts_with(b"%%HP:"),
            "{:?}",
            String::from_utf8_lossy(&ascii)
        );
        let binary = calc.get("IOPAR", TransferMode::Binary)?;
        let info = object::inspect(&binary)?;
        assert_eq!(info.object_type, Some(ObjectType::List));
        assert!(matches!(info.header.family, Family::Hp48 | Family::Hp49));
        assert_eq!(calc.iopar()?, Iopar::default());
        Ok(())
    })
}

/// A String object holding all 256 byte values, as a binary transfer file
/// with `header`.
fn all_bytes_string(header: &[u8]) -> Vec<u8> {
    fn field(nibbles: &mut Vec<u8>, value: u32, width: usize) {
        for i in 0..width {
            nibbles.push(((value >> (4 * i)) & 0xF) as u8);
        }
    }
    let mut nibbles = Vec::new();
    field(&mut nibbles, 0x02A2C, 5);
    field(&mut nibbles, 5 + 2 * 256, 5);
    for b in 0..=255u8 {
        nibbles.extend([b & 0xF, b >> 4]);
    }
    let mut file = header.to_vec();
    file.extend(object::pack(&nibbles));
    file
}

/// Binary mode survives byte for byte (ASCII mode on the 48SX mangles
/// bytes 0-26).
#[test]
fn put_round_trip() -> TestResult {
    scenario(|calc| {
        let iopar = calc.get("IOPAR", TransferMode::Binary)?;
        let file = all_bytes_string(&iopar[..HEADER_LEN]);
        assert_eq!(
            object::inspect(&file)?.object_type,
            Some(ObjectType::String)
        );
        assert_eq!(calc.put("HPTXRT", &file, TransferMode::Binary)?, "HPTXRT");
        let back = calc.get("HPTXRT", TransferMode::Binary)?;
        calc.remove("HPTXRT")?;
        assert_eq!(back, file);
        Ok(())
    })
}

#[test]
fn run() -> TestResult {
    scenario(|calc| {
        let reply = calc.run("6 7 *")?;
        calc.run("DROP")?;
        assert_eq!(reply.error, None);
        assert_eq!(reply.level(1).and_then(parse_real), Some(42.0));

        let reply = calc.run("'HPTXNOSUCH' RCL")?;
        calc.run("DROP")?;
        assert_eq!(reply.error.as_deref(), Some("Undefined Name"));

        assert!(calc.mem()? > 0.0);
        if let Some(version) = calc.version()? {
            assert!(!version.is_empty());
            eprintln!("version: {version}");
        }

        let err = calc.run(&"1".repeat(80)).unwrap_err();
        assert!(matches!(err, Error::CommandTooLong { .. }), "{err:?}");
        let reply = calc.run("1 2 +")?;
        calc.run("DROP")?;
        assert_eq!(reply.level(1).and_then(parse_real), Some(3.0));
        Ok(())
    })
}

#[test]
fn pict() -> TestResult {
    scenario(|calc| {
        // A fresh PICT is 0x0; ERASE makes it 131x64. One pixel at (10, 10).
        let reply = calc.run("ERASE { # 10d # 10d } PIXON")?;
        assert_eq!(reply.error, None);
        let grob = calc.pict()?;
        assert_eq!((grob.width, grob.height), (131, 64));
        assert!(grob.pixel(10, 10));
        let lit = (0..64)
            .flat_map(|y| (0..131).map(move |x| (x, y)))
            .filter(|&(x, y)| grob.pixel(x, y))
            .count();
        assert_eq!(lit, 1);
        assert!(!names(calc)?.iter().any(|n| n == "HPTXTMP"));
        calc.run("ERASE")?;
        Ok(())
    })
}

#[test]
fn backup() -> TestResult {
    scenario(|calc| {
        let data = calc.backup()?;
        let info = object::inspect(&data)?;
        assert_eq!(info.object_type, Some(ObjectType::Directory));
        assert!(info.size_nibbles.is_some_and(|n| n > 0));
        assert!(!names(calc)?.iter().any(|n| n == "HPTXBK"));
        Ok(())
    })
}

// ---- XModem ----

/// Variable the XModem scenario transfers.
const XMODEM_VAR: &str = "HPTXXM";

/// Press `keys` on the emulated calculator (`calc-keys`, one tmux key name
/// each; `;` is ALPHA, `\` is ON).
fn press(container: &str, keys: &[&str]) -> TestResult {
    let status = Command::new("docker")
        .args(["exec", container, "calc-keys"])
        .args(keys)
        .status()?;
    if !status.success() {
        return Err(format!("calc-keys {keys:?}: {status}").into());
    }
    Ok(())
}

/// Press ON first (alpha mode can still be on after the Kermit server ended;
/// on the 49G a stale alpha lock turned `xsend` into `SIN(X)!`), then type
/// `word` in alpha-lock mode, then ENTER.
fn type_word(container: &str, word: &str) -> TestResult {
    let letters: Vec<String> = word.chars().map(|c| c.to_string()).collect();
    let mut keys = vec!["\\", ";", ";"];
    keys.extend(letters.iter().map(String::as_str));
    keys.push("Enter");
    press(container, &keys)
}

/// Type `word` after `delay`, in the background (the transfer is waiting
/// meanwhile).
fn type_later(container: &str, word: &str, delay: Duration) -> JoinHandle<Result<(), String>> {
    let (container, word) = (container.to_string(), word.to_string());
    thread::spawn(move || {
        thread::sleep(delay);
        type_word(&container, &word).map_err(|e| e.to_string())
    })
}

/// Cancel whatever runs (ON) and type SERVER, then reconnect Kermit on the
/// same link. The calculator is out of server mode after XRECV/XSEND.
fn restart_server(
    container: &str,
    xs: XmodemSession,
) -> Result<Calculator, Box<dyn std::error::Error>> {
    thread::sleep(Duration::from_secs(2));
    press(container, &["\\"])?;
    thread::sleep(Duration::from_secs(1));
    type_word(container, "server")?;
    thread::sleep(Duration::from_secs(2));
    Ok(Calculator::new(Session::new(
        xs.into_transport(),
        Options::default(),
    )?))
}

fn print_progress(event: &Event) {
    match event {
        Event::Started { check } => eprintln!("xmodem: started, check {check:?}"),
        Event::Done => eprintln!("xmodem: done"),
        Event::Error(e) => eprintln!("xmodem: error {e}"),
        _ => {}
    }
}

/// Put a 269-byte string holding all 256 byte values with XRECV, check it
/// with a Kermit GET, get it back with XSEND and compare byte for byte after
/// the padding cut. 49G: HP's CRC (`D`); 48G/GX: checksum.
#[test]
fn xmodem_round_trip() -> TestResult {
    let Some(addr) = std::env::var("HPTX_E2E_ADDR")
        .ok()
        .filter(|a| !a.is_empty())
    else {
        return Ok(());
    };
    let Some(container) = std::env::var("HPTX_E2E_CONTAINER")
        .ok()
        .filter(|c| !c.is_empty())
    else {
        eprintln!("xmodem: HPTX_E2E_CONTAINER not set, skipped");
        return Ok(());
    };
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut calc = Calculator::open(&addr)?;
    let model = calc.model()?;
    if !matches!(model, Model::Hp49G | Model::Hp48Gx) {
        eprintln!("xmodem: {} has no XModem, skipped", model.name());
        return Ok(());
    }
    cleanup(&mut calc)?;
    let iopar = calc.get("IOPAR", TransferMode::Binary)?;
    let file = all_bytes_string(&iopar[..HEADER_LEN]);
    let options = XmodemOptions::for_model(model)?;
    let want_check = match model {
        Model::Hp49G => Check::HpCrc,
        _ => Check::Checksum,
    };

    // XRECV: the name goes on the stack over Kermit (the same as typing
    // `'HPTXXM'`), XRECV is typed.
    calc.run(&format!("'{XMODEM_VAR}'"))?;
    let plan = calc.prepare_for_xmodem(XmodemDirection::ToCalculator, XMODEM_VAR)?;
    eprintln!("xmodem: {}", plan.instructions());
    let switched_to_rpn = plan.switched_to_rpn;
    let mut xs = XmodemSession::new(calc.into_transport(), options.clone());
    let typer = type_later(&container, "xrecv", Duration::from_secs(2));
    let start = Instant::now();
    let sent = xs.send_with(&file, &mut print_progress);
    eprintln!("xmodem: XRECV took {:?}", start.elapsed());
    let typed = typer.join().map_err(|_| "typing thread panicked")?;
    let mut calc = restart_server(&container, xs)?;
    let result = (|| -> TestResult {
        typed?;
        let report = sent?;
        assert_eq!(report.check, want_check);
        assert_eq!(report.bytes, file.len() as u64);
        let back = calc.get(XMODEM_VAR, TransferMode::Binary)?;
        assert_eq!(back, file, "XRECV stored something else");
        Ok(())
    })();
    if let Err(e) = result {
        let _ = cleanup(&mut calc);
        return Err(e);
    }

    // XSEND.
    calc.run(&format!("'{XMODEM_VAR}'"))?;
    let plan = calc.prepare_for_xmodem(XmodemDirection::FromCalculator, XMODEM_VAR)?;
    eprintln!("xmodem: {}", plan.instructions());
    let mut xs = XmodemSession::new(calc.into_transport(), options);
    let typer = type_later(&container, "xsend", Duration::from_secs(2));
    let start = Instant::now();
    let received = xs.receive_with(&mut print_progress);
    eprintln!("xmodem: XSEND took {:?}", start.elapsed());
    let typed = typer.join().map_err(|_| "typing thread panicked")?;
    let mut calc = restart_server(&container, xs)?;
    let result = (|| -> TestResult {
        typed?;
        let got = received?;
        eprintln!(
            "xmodem: received {} bytes, last block {}, cut {:?}",
            got.received, got.last_block, got.stripped
        );
        assert_eq!(got.check, want_check);
        assert_eq!(got.stripped, Some(got.received - file.len()));
        assert_eq!(got.data, file, "XSEND sent something else");
        Ok(())
    })();
    if switched_to_rpn {
        calc.run("-95 SF")?;
    }
    let cleaned = cleanup(&mut calc);
    result?;
    cleaned
}

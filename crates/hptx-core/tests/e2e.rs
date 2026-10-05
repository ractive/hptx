//! End-to-end tests against the saturnng emulator in `emulator/`.
//!
//! Skipped unless `HPTX_E2E_ADDR` is set, e.g. `tcp://localhost:4848`. Must
//! pass on the 48SX, 48GX and 49G. Every scenario leaves the calculator in
//! HOME and in ASCII transfer mode, as a fresh one is.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use hptx_core::object::{self, Family, HEADER_LEN, ObjectType};
use hptx_core::reply::{Iopar, parse_real};
use hptx_core::{Calculator, Error, TransferMode};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// One emulator, one client at a time.
static LOCK: Mutex<()> = Mutex::new(());

/// Variables a scenario may create in HOME.
const LEFTOVERS: &[&str] = &["HPTXE2E", "HPTXE2F", "HPTXRT", "HPTXTMP", "HPTXBK"];

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
fn screenshot() -> TestResult {
    scenario(|calc| {
        let grob = calc.screenshot()?;
        assert_eq!((grob.width, grob.height), (131, 64));
        let pixels: Vec<bool> = (0..64)
            .flat_map(|y| (0..131).map(move |x| (x, y)))
            .map(|(x, y)| grob.pixel(x, y))
            .collect();
        assert!(pixels.iter().any(|&p| p) && pixels.iter().any(|&p| !p));
        assert!(!names(calc)?.iter().any(|n| n == "HPTXTMP"));
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

//! Choosing the link: `--port` / `HPTX_PORT`, else the one USB serial port.
//!
//! `serialport` is built without libudev (decision log, iteration 3), so on
//! Linux the port type may be unknown. A port is a USB candidate when
//! `serialport` says it is USB or its name matches the usual USB serial
//! pattern of the OS: macOS `/dev/cu.usbserial*`, `/dev/cu.usbmodem*` (and
//! the CH340, CP210x and PL2303 driver names), Linux `/dev/ttyUSB*`,
//! `/dev/ttyACM*`. On Windows a single `COM*` port counts when nothing is
//! known to be USB.

use serde::Serialize;
use serialport::{SerialPortInfo, SerialPortType};

/// One serial port as `hptx ports` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortInfo {
    /// Device path or COM name.
    pub name: String,
    /// `usb`, `pci`, `bluetooth` or `unknown`.
    pub kind: &'static str,
    /// USB product text when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    /// USB vendor:product id, e.g. `0403:6001`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usb_id: Option<String>,
    /// Would be picked automatically if it were the only candidate.
    pub candidate: bool,
}

/// All serial ports, candidates first.
pub fn list() -> anyhow::Result<Vec<PortInfo>> {
    let ports = serialport::available_ports()?;
    Ok(describe(&ports, std::env::consts::OS))
}

fn describe(ports: &[SerialPortInfo], os: &str) -> Vec<PortInfo> {
    let mut out: Vec<PortInfo> = ports
        .iter()
        .filter(|p| !(os == "macos" && p.port_name.starts_with("/dev/tty.")))
        .map(|p| {
            let (kind, product, usb_id) = match &p.port_type {
                SerialPortType::UsbPort(usb) => (
                    "usb",
                    usb.product.clone(),
                    Some(format!("{:04x}:{:04x}", usb.vid, usb.pid)),
                ),
                SerialPortType::PciPort => ("pci", None, None),
                SerialPortType::BluetoothPort => ("bluetooth", None, None),
                SerialPortType::Unknown => ("unknown", None, None),
            };
            let candidate = kind == "usb" || usb_name(&p.port_name, os);
            PortInfo {
                name: p.port_name.clone(),
                kind,
                product,
                usb_id,
                candidate,
            }
        })
        .collect();
    if os == "windows" && !out.iter().any(|p| p.candidate) && out.len() == 1 {
        for p in &mut out {
            p.candidate = p.name.to_ascii_uppercase().starts_with("COM");
        }
    }
    out.sort_by(|a, b| b.candidate.cmp(&a.candidate).then(a.name.cmp(&b.name)));
    out
}

/// Name patterns of USB serial adapters per OS.
fn usb_name(name: &str, os: &str) -> bool {
    let prefixes: &[&str] = match os {
        "macos" => &[
            "/dev/cu.usbserial",
            "/dev/cu.usbmodem",
            "/dev/cu.wchusbserial",
            "/dev/cu.SLAB_USBtoUART",
            "/dev/cu.PL2303",
        ],
        "linux" => &["/dev/ttyUSB", "/dev/ttyACM"],
        _ => &[],
    };
    prefixes.iter().any(|p| name.starts_with(p))
}

/// The address to open: `explicit` (from `--port` or `HPTX_PORT`), else the
/// only USB serial port.
pub fn resolve(explicit: Option<&str>) -> anyhow::Result<String> {
    if let Some(addr) = explicit.filter(|a| !a.is_empty()) {
        return Ok(addr.to_string());
    }
    let ports = list().map_err(|e| {
        crate::error::Hinted::new(
            format!("cannot list serial ports: {e}"),
            "name the port: --port /dev/ttyUSB0, --port COM3 or --port tcp://localhost:4848",
        )
    })?;
    pick(&ports).map_err(Into::into)
}

fn pick(ports: &[PortInfo]) -> Result<String, crate::error::Hinted> {
    let candidates: Vec<&PortInfo> = ports.iter().filter(|p| p.candidate).collect();
    match candidates.as_slice() {
        [one] => Ok(one.name.clone()),
        [] => Err(crate::error::Hinted::new(
            "no USB serial port found",
            "connect the cable, or name the port with --port PATH (or HPTX_PORT); \
             `hptx ports` lists what is there; an emulator is --port tcp://localhost:4848",
        )),
        many => {
            let names: Vec<&str> = many.iter().map(|p| p.name.as_str()).collect();
            Err(crate::error::Hinted::new(
                format!("several USB serial ports: {}", names.join(", ")),
                format!(
                    "pick one with --port, e.g. `hptx --port {} info`, or set HPTX_PORT",
                    crate::output::shell_quote(names[0])
                ),
            ))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn info(name: &str, port_type: SerialPortType) -> SerialPortInfo {
        SerialPortInfo {
            port_name: name.into(),
            port_type,
        }
    }

    #[test]
    fn linux_names_without_libudev() {
        let ports = [
            info("/dev/ttyS0", SerialPortType::Unknown),
            info("/dev/ttyUSB0", SerialPortType::Unknown),
        ];
        let d = describe(&ports, "linux");
        assert_eq!(d[0].name, "/dev/ttyUSB0");
        assert!(d[0].candidate && !d[1].candidate);
        assert_eq!(pick(&d).unwrap(), "/dev/ttyUSB0");
    }

    #[test]
    fn macos_skips_tty_twins() {
        let ports = [
            info("/dev/tty.usbserial-A1", SerialPortType::Unknown),
            info("/dev/cu.usbserial-A1", SerialPortType::Unknown),
            info("/dev/cu.Bluetooth-Incoming-Port", SerialPortType::Unknown),
        ];
        let d = describe(&ports, "macos");
        assert_eq!(d.len(), 2);
        assert_eq!(pick(&d).unwrap(), "/dev/cu.usbserial-A1");
    }

    #[test]
    fn several_candidates_is_an_error_listing_them() {
        let ports = [
            info("/dev/ttyUSB0", SerialPortType::Unknown),
            info("/dev/ttyACM0", SerialPortType::Unknown),
        ];
        let err = pick(&describe(&ports, "linux")).unwrap_err();
        assert!(err.message.contains("/dev/ttyACM0, /dev/ttyUSB0"), "{err}");
        assert!(err.hint.contains("--port"));
    }

    #[test]
    fn none_is_an_error() {
        let ports = [info("/dev/ttyS0", SerialPortType::Unknown)];
        assert!(pick(&describe(&ports, "linux")).is_err());
    }

    #[test]
    fn windows_single_com_port() {
        let ports = [info("COM3", SerialPortType::Unknown)];
        assert_eq!(pick(&describe(&ports, "windows")).unwrap(), "COM3");
        let two = [
            info("COM1", SerialPortType::Unknown),
            info("COM3", SerialPortType::Unknown),
        ];
        assert!(pick(&describe(&two, "windows")).is_err());
    }

    #[test]
    fn explicit_wins() {
        assert_eq!(
            resolve(Some("tcp://localhost:4848")).unwrap(),
            "tcp://localhost:4848"
        );
    }
}

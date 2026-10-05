//! The error type of `hptx-core`.

/// Everything that can go wrong talking to a calculator.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The link failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The serial port could not be opened or configured.
    #[error("serial port: {0}")]
    Serial(#[from] serialport::Error),
    /// The link address is not `tcp://host:port`, `saturnus://[MODEL@]ROM`
    /// or a device path.
    #[error(
        "bad address {0:?}: expected a serial device path, tcp://host:port or saturnus://[MODEL@]ROM-PATH"
    )]
    Address(String),
    /// The in-process emulator could not be started (missing ROM, wrong
    /// size, no `saturnus` feature).
    #[error("emulator: {0}")]
    Emulator(String),
    /// The calculator sent a Kermit E packet (text translated from the HP
    /// character set), e.g. `Undefined Name` for a GET of a missing variable.
    #[error("calculator: {0}")]
    Remote(String),
    /// A Kermit failure other than an E packet: retries exhausted, protocol
    /// violation, cancelled.
    #[error("kermit: {0}")]
    Kermit(kermit_proto::Error),
    /// An XModem transfer failed: no start within the start window, retries
    /// exhausted, or cancelled by the calculator (two CANs).
    #[error("xmodem: {0}")]
    Xmodem(xmodem_proto::Error),
    /// The calculator cannot do what was asked, e.g. XModem on a 48S/SX.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// A host command ran but the calculator reported an error. `stack` is
    /// what the calculator returned with it, level 1 first.
    #[error("calculator error: {message}")]
    Calculator {
        /// The text after `Error: `, e.g. `Undefined Name`.
        message: String,
        /// Stack levels after the error, level 1 first.
        stack: Vec<String>,
    },
    /// A host command does not fit in one Kermit C packet.
    #[error(
        "host command too long: {len} encoded bytes, at most {max} fit in one packet: {command}"
    )]
    CommandTooLong {
        /// The command as given.
        command: String,
        /// Its encoded length.
        len: usize,
        /// The largest encoded length that fits.
        max: usize,
    },
    /// A character has no code in the HP character set.
    #[error("{0:?} has no code in the HP character set")]
    Charset(char),
    /// Not a valid calculator variable name.
    #[error("invalid variable name {0:?}")]
    Name(String),
    /// The calculator's reply could not be understood.
    #[error("unexpected reply: {0}")]
    Reply(String),
    /// A file is not a usable HP object.
    #[error("bad object: {0}")]
    Object(String),
}

/// `Result` with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

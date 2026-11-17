//! logid: control Logitech HID++ mice from Linux.

pub mod hidpp;
pub mod ipc;
pub mod log;
pub mod paths;
pub mod request;

/// The OS message of an I/O error without Rust's " (os error N)" suffix
/// (what Python calls `strerror`).
pub fn strerror(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

/// Error type shared by all modules. `Display` gives the user-facing message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Generic transport/device failure.
    Device(String),
    /// The receiver cannot reach the device (absent, off, asleep).
    Unreachable(String),
    /// The device or the receiver answered with a HID++ error.
    Hidpp(String),
    /// The device does not implement a feature.
    Unsupported(String),
    /// No reply within the deadline.
    Timeout(String),
    /// Bad request (business logic or IPC).
    Request(String),
    /// Another daemon already listens on the socket.
    AlreadyRunning(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Device(m)
            | Error::Unreachable(m)
            | Error::Hidpp(m)
            | Error::Unsupported(m)
            | Error::Timeout(m)
            | Error::Request(m)
            | Error::AlreadyRunning(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

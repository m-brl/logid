//! logid daemon: logid [--hidraw /dev/hidrawN] [--index N] [--socket PATH] [-v]

use std::process::exit;
use std::sync::{Arc, Mutex};

use logid::hidpp::Device;
use logid::paths::default_socket_path;
use logid::{info, ipc};

const USAGE: &str = "usage: logid [-h] [--hidraw PATH] [--index N] [--socket PATH] [-v]";

const HELP: &str = "Logitech HID++ mouse control daemon

options:
  -h, --help       show this help message and exit
  --hidraw PATH    HID++ hidraw node (default: first Logitech HID++ interface found)
  --index N        device index: 1-6 on a receiver, 0xff for Bluetooth (default: auto)
  --socket PATH    Unix socket path
  -v, --verbose    log exchanged reports";

#[derive(Debug, PartialEq)]
struct Args {
    hidraw: Option<String>,
    index: Option<u8>,
    socket: String,
    verbose: bool,
}

#[derive(Debug, PartialEq)]
enum Parsed {
    Run(Args),
    Help,
}

/// Decimal or 0x-prefixed hexadecimal, 0..=255.
fn parse_index(s: &str) -> Result<u8, String> {
    let bad = || format!("argument --index: invalid index value: '{s}'");
    let v = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16),
        None => s.parse(),
    }
    .map_err(|_| bad())?;
    u8::try_from(v).map_err(|_| bad())
}

fn parse_args(
    args: impl IntoIterator<Item = String>,
    default_socket: String,
) -> Result<Parsed, String> {
    let mut out = Args {
        hidraw: None,
        index: None,
        socket: default_socket,
        verbose: false,
    };
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let value = |it: &mut dyn Iterator<Item = String>| {
            inline
                .clone()
                .or_else(|| it.next())
                .ok_or(format!("argument {name}: expected one argument"))
        };
        match name.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-v" | "--verbose" => out.verbose = true,
            "--hidraw" => out.hidraw = Some(value(&mut it)?),
            "--socket" => out.socket = value(&mut it)?,
            "--index" => out.index = Some(parse_index(&value(&mut it)?)?),
            _ => return Err(format!("unrecognized arguments: {arg}")),
        }
    }
    Ok(Parsed::Run(out))
}

fn block_signals() -> libc::sigset_t {
    // SAFETY: plain libc signal-set manipulation on a zero-initialised set.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        set
    }
}

fn wait_signal(set: &libc::sigset_t) {
    let mut sig = 0;
    // SAFETY: set is a valid signal set; the signals are blocked in all threads.
    unsafe { libc::sigwait(set, &mut sig) };
}

fn main() {
    let args = match parse_args(std::env::args().skip(1), default_socket_path()) {
        Ok(Parsed::Run(a)) => a,
        Ok(Parsed::Help) => {
            println!("{USAGE}\n\n{HELP}");
            return;
        }
        Err(e) => {
            eprintln!("{USAGE}\nlogid: error: {e}");
            exit(2);
        }
    };
    logid::log::set_debug(args.verbose);

    // Block the signals before spawning any thread so that all inherit the mask.
    let set = block_signals();

    let device = Arc::new(Mutex::new(Device::new(args.hidraw, args.index)));
    let listener = match ipc::bind(&args.socket) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("logid: {e}");
            exit(1);
        }
    };

    let socket = args.socket.clone();
    let dev = device.clone();
    std::thread::spawn(move || {
        wait_signal(&set);
        info!("logid", "shutting down");
        let _ = std::fs::remove_file(&socket);
        if let Ok(mut d) = dev.try_lock() {
            d.disconnect();
        }
        exit(0);
    });

    ipc::serve(listener, device);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(a: &[&str]) -> Result<Parsed, String> {
        parse_args(a.iter().map(|s| s.to_string()), "/def.sock".into())
    }

    #[test]
    fn index_parsing() {
        assert_eq!(parse_index("3"), Ok(3));
        assert_eq!(parse_index("0xff"), Ok(255));
        assert_eq!(parse_index("0XFF"), Ok(255));
        assert!(parse_index("256").is_err());
        assert!(parse_index("0x").is_err());
        assert!(parse_index("-1").is_err());
        assert!(parse_index("abc").is_err());
    }

    #[test]
    fn options() {
        let Parsed::Run(a) = parse(&[
            "--hidraw",
            "/dev/hidraw3",
            "--index=0x01",
            "-v",
            "--socket",
            "/s",
        ])
        .unwrap() else {
            panic!()
        };
        assert_eq!(a.hidraw.as_deref(), Some("/dev/hidraw3"));
        assert_eq!(a.index, Some(1));
        assert_eq!(a.socket, "/s");
        assert!(a.verbose);
    }

    #[test]
    fn defaults_help_and_errors() {
        let Parsed::Run(a) = parse(&[]).unwrap() else {
            panic!()
        };
        assert_eq!(
            (a.hidraw, a.index, a.socket.as_str(), a.verbose),
            (None, None, "/def.sock", false)
        );
        assert_eq!(parse(&["--verbose", "-h"]), Ok(Parsed::Help));
        assert!(parse(&["--index"]).is_err());
        assert!(parse(&["--index", "x"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
    }
}

//! IPC: Unix socket, one text request per line, one response per line.
//!
//! ```text
//! > vibrate                < ok: played sharp-state-change
//! > vibrate happy-alert    < ok: played happy-alert
//! > dpi                     < ok: dpi 1600 (default 1000, range 200-8000 step 50)
//! > dpi 800                 < ok: dpi 800
//! > scroll auto 10          < ok: ratchet (auto-freespin above 10)
//! > wheel invert on         < ok: invert on, hires on
//! > battery                 < ok: 80% (discharging)
//! > foo                    < err: unknown command 'foo' — ...
//!
//! Commands: `vibrate [waveform]`, `dpi [value]`,
//! `scroll [ratchet|freespin|auto N]`, `wheel [invert on|off] [hires on|off]`,
//! `battery`, `help`.
//! ```

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::request::{self, Backend};
use crate::{debug, info, warn, Error};

const T: &str = "logid.ipc";

pub type SharedDevice = Arc<Mutex<dyn Backend + Send>>;

pub fn help() -> String {
    let names: Vec<&str> = request::WAVEFORMS.iter().map(|w| w.0).collect();
    format!(
        "commands: vibrate [{}], dpi [value], scroll [ratchet|freespin|auto N], \
         wheel [invert on|off] [hires on|off], battery",
        names.join("|")
    )
}

/// Translate a text line into a call to the request module.
pub fn dispatch(dev: &mut dyn Backend, line: &str) -> Result<String, Error> {
    let mut words = line.split_whitespace();
    let cmd = words.next().unwrap_or("");
    let args: Vec<&str> = words.collect();
    match cmd {
        "vibrate" => {
            if args.len() > 1 {
                return Err(Error::Request("usage: vibrate [waveform]".into()));
            }
            request::vibrate(
                dev,
                args.first().copied().unwrap_or(request::DEFAULT_WAVEFORM),
            )
        }
        "dpi" => match args[..] {
            [] => request::dpi(dev, None),
            [v] => request::dpi(dev, Some(v)),
            _ => Err(Error::Request("usage: dpi [value]".into())),
        },
        "scroll" => request::scroll(dev, &args),
        "wheel" => request::wheel(dev, &args),
        "battery" => {
            if !args.is_empty() {
                return Err(Error::Request("usage: battery".into()));
            }
            request::battery(dev)
        }
        "help" | "" => Ok(help()),
        _ => Err(Error::Request(format!(
            "unknown command '{cmd}' — {}",
            help()
        ))),
    }
}

/// Full response line (without newline) for one request line.
pub fn respond(dev: &mut dyn Backend, text: &str) -> String {
    match dispatch(dev, text) {
        Ok(m) => format!("ok: {m}"),
        Err(e) => {
            if !matches!(e, Error::Request(_)) {
                warn!(T, "'{text}' failed: {e}");
            }
            format!("err: {e}")
        }
    }
}

/// Remove a stale socket, or fail if a daemon is already listening.
pub fn remove_stale_socket(path: &str) -> Result<(), Error> {
    if !Path::new(path).exists() {
        return Ok(());
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(Error::AlreadyRunning(format!(
            "logid is already running on {path}"
        ))),
        Err(_) => {
            let _ = std::fs::remove_file(path);
            Ok(())
        }
    }
}

/// Bind the socket (mode 0600) after removing a stale one.
pub fn bind(path: &str) -> Result<UnixListener, Error> {
    remove_stale_socket(path)?;
    let listener = UnixListener::bind(path)
        .map_err(|e| Error::Device(format!("cannot bind {path}: {}", crate::strerror(&e))))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| Error::Device(format!("cannot chmod {path}: {}", crate::strerror(&e))))?;
    info!(T, "listening on {path}");
    Ok(listener)
}

pub fn handle_client(stream: UnixStream, dev: SharedDevice) {
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&buf).trim().to_string();
        debug!(T, "request: {text}");
        let response = {
            let mut d = dev.lock().unwrap_or_else(|e| e.into_inner());
            respond(&mut *d, &text)
        };
        if writer
            .write_all(format!("{response}\n").as_bytes())
            .is_err()
        {
            return;
        }
    }
}

/// Accept loop: one thread per connection.
pub fn serve(listener: UnixListener, dev: SharedDevice) {
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let dev = dev.clone();
                std::thread::spawn(move || handle_client(stream, dev));
            }
            Err(e) => warn!(T, "accept failed: {}", crate::strerror(&e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::tests::Fake;

    #[test]
    fn help_lists_commands() {
        let mut d = Fake::new(0);
        let h = dispatch(&mut d, "").unwrap();
        assert!(h.starts_with("commands: vibrate [sharp-state-change|damp-state-change|"));
        assert_eq!(dispatch(&mut d, "help").unwrap(), h);
        assert!(d.calls.is_empty());
    }

    #[test]
    fn vibrate_default_and_named() {
        let mut d = Fake::new(0xFFFF);
        assert_eq!(respond(&mut d, "vibrate"), "ok: played sharp-state-change");
        assert_eq!(
            respond(&mut d, "  vibrate   happy-alert "),
            "ok: played happy-alert"
        );
    }

    #[test]
    fn help_lists_new_commands() {
        let h = help();
        for c in [
            "dpi [value]",
            "scroll [ratchet|freespin|auto N]",
            "wheel [invert on|off] [hires on|off]",
            "battery",
        ] {
            assert!(h.contains(c), "{h}");
        }
    }

    #[test]
    fn new_commands_dispatch() {
        let mut d = Fake::new(0);
        d.script(request::SMART_SHIFT_ENHANCED, 1, &[1, 0]);
        d.script(request::HIRES_WHEEL, 1, &[0b100]);
        d.script(request::UNIFIED_BATTERY, 1, &[80, 0, 0]);
        d.script(request::ADJUSTABLE_DPI, 1, &[0, 0x01, 0x90, 0x03, 0x20, 0]);
        d.script(request::ADJUSTABLE_DPI, 2, &[0, 0x01, 0x90, 0x01, 0x90]);
        assert_eq!(respond(&mut d, "scroll"), "ok: freespin");
        assert_eq!(respond(&mut d, "wheel"), "ok: invert on, hires off");
        assert_eq!(respond(&mut d, "battery"), "ok: 80% (discharging)");
        assert_eq!(
            respond(&mut d, "dpi"),
            "ok: dpi 400 (default 400, values: 400, 800)"
        );
        assert_eq!(respond(&mut d, "dpi 400"), "ok: dpi 400");
        assert_eq!(respond(&mut d, "dpi 1 2"), "err: usage: dpi [value]");
        assert_eq!(respond(&mut d, "battery x"), "err: usage: battery");
        assert_eq!(
            respond(&mut d, "wheel invert"),
            "err: usage: wheel [invert on|off] [hires on|off]"
        );
        assert_eq!(
            respond(&mut d, "scroll auto"),
            "err: usage: scroll [ratchet|freespin|auto N]"
        );
    }

    #[test]
    fn errors() {
        let mut d = Fake::new(0);
        assert_eq!(
            respond(&mut d, "vibrate a b"),
            "err: usage: vibrate [waveform]"
        );
        let r = respond(&mut d, "foo");
        assert!(
            r.starts_with("err: unknown command 'foo' — commands: vibrate ["),
            "{r}"
        );
        d.fail = Some(Error::Unreachable(
            "receiver error: connection failed".into(),
        ));
        assert_eq!(
            respond(&mut d, "vibrate"),
            "err: receiver error: connection failed"
        );
    }

    #[test]
    fn stale_socket() {
        let dir = std::env::temp_dir().join(format!("logid-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.sock");
        let p = p.to_str().unwrap();
        let l = UnixListener::bind(p).unwrap();
        assert!(matches!(
            remove_stale_socket(p),
            Err(Error::AlreadyRunning(_))
        ));
        drop(l); // socket file remains but nobody listens
        assert!(remove_stale_socket(p).is_ok());
        assert!(!Path::new(p).exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}

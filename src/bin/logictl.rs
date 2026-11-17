//! logid client:
//! logictl [vibrate [waveform] | dpi [value] | scroll [ratchet|freespin|auto N]
//!          | wheel [invert on|off] [hires on|off] | battery | help]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::exit;

use logid::paths::default_socket_path;

fn query(line: &str) -> std::io::Result<String> {
    let mut s = UnixStream::connect(default_socket_path())?;
    s.write_all(format!("{line}\n").as_bytes())?;
    let mut response = String::new();
    BufReader::new(s).read_line(&mut response)?;
    Ok(response.trim().to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let line = if args.is_empty() {
        "help".to_string()
    } else {
        args.join(" ")
    };
    let response = match query(&line) {
        Ok(r) => r,
        Err(e) => {
            let code = e
                .raw_os_error()
                .map(|c| format!("[Errno {c}] "))
                .unwrap_or_default();
            eprintln!("cannot reach logid: {code}{}", logid::strerror(&e));
            exit(1);
        }
    };
    let (status, message) = match response.split_once(": ") {
        Some((s, m)) => (s, m),
        None => (response.as_str(), ""),
    };
    if status != "ok" {
        eprintln!(
            "{}",
            if message.is_empty() {
                &response
            } else {
                message
            }
        );
        exit(1);
    }
    println!("{message}");
}

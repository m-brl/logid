//! Tiny stderr logger: `LEVEL target: message` (journald adds timestamps).

use std::fmt::Arguments;
use std::sync::atomic::{AtomicBool, Ordering};

static DEBUG: AtomicBool = AtomicBool::new(false);

pub fn set_debug(on: bool) {
    DEBUG.store(on, Ordering::Relaxed);
}

pub fn emit(level: &str, target: &str, args: Arguments<'_>) {
    if level == "DEBUG" && !DEBUG.load(Ordering::Relaxed) {
        return;
    }
    eprintln!("{level} {target}: {args}");
}

/// Space-separated lowercase hex dump.
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[macro_export]
macro_rules! debug {
    ($t:expr, $($a:tt)*) => { $crate::log::emit("DEBUG", $t, format_args!($($a)*)) };
}
#[macro_export]
macro_rules! info {
    ($t:expr, $($a:tt)*) => { $crate::log::emit("INFO", $t, format_args!($($a)*)) };
}
#[macro_export]
macro_rules! warn {
    ($t:expr, $($a:tt)*) => { $crate::log::emit("WARNING", $t, format_args!($($a)*)) };
}

#[cfg(test)]
mod tests {
    #[test]
    fn hex_dump() {
        assert_eq!(super::hex(&[0x11, 0x01, 0xff]), "11 01 ff");
        assert_eq!(super::hex(&[]), "");
    }
}

//! HID++ 2.0 transport over hidraw (Bolt/Unifying receiver or Bluetooth).
//!
//! Handles opening the hidraw node, choosing the device, resolving feature
//! indexes and matching requests to replies. It does not interpret payloads
//! (see request.rs).
//!
//! Long report : 11 | device index | feature index | (function << 4) | swid | 16 bytes of params
//! 2.0 error   : 11 | device index | FF | feature index | function|swid | code
//! 1.0 error   : 10 | device index | 8F | feature index | function|swid | code  (from the receiver)

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

use crate::log::hex;
use crate::{debug, info, strerror, warn, Error};

const T: &str = "logid.hidpp";

pub const LOGITECH_VENDOR: u32 = 0x046D;
pub const BUS_BLUETOOTH: u32 = 0x0005;

pub const SHORT: u8 = 0x10;
pub const LONG: u8 = 0x11;
pub const LONG_LEN: usize = 20;
pub const ERROR_10: u8 = 0x8F;
pub const ERROR_20: u8 = 0xFF;
/// Software ID: distinguishes our replies from Solaar's/logiops'.
pub const SWID: u8 = 0x0A;

pub const ROOT: u16 = 0x0000;
pub const ROOT_GET_FEATURE: u8 = 0;

pub const DIRECT: u8 = 0xFF;

pub const TIMEOUT: Duration = Duration::from_secs(2);
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

const ERRORS_10: [(u8, &str); 12] = [
    (0x01, "invalid sub-id"),
    (0x02, "invalid address"),
    (0x03, "invalid value"),
    (0x04, "connection failed"),
    (0x05, "too many devices"),
    (0x06, "already exists"),
    (0x07, "busy"),
    (0x08, "unknown device"),
    (0x09, "resource error"),
    (0x0A, "request unavailable"),
    (0x0B, "invalid parameter value"),
    (0x0C, "wrong PIN code"),
];
/// Device absent, off or asleep.
const UNREACHABLE_10: [u8; 3] = [0x04, 0x08, 0x09];

const ERRORS_20: [(u8, &str); 9] = [
    (0x01, "unknown error"),
    (0x02, "invalid argument"),
    (0x03, "out of range"),
    (0x04, "hardware error"),
    (0x05, "Logitech internal error"),
    (0x06, "invalid feature index"),
    (0x07, "invalid function"),
    (0x08, "busy"),
    (0x09, "unsupported"),
];

fn error_name(table: &[(u8, &str)], code: u8) -> String {
    match table.iter().find(|e| e.0 == code) {
        Some(e) => e.1.to_string(),
        None => format!("0x{code:02x}"),
    }
}

// --- Pure helpers -----------------------------------------------------------

/// Build a long report, zero-padding the parameters.
pub fn build_report(
    index: u8,
    feature_index: u8,
    function: u8,
    params: &[u8],
) -> Result<[u8; LONG_LEN], Error> {
    if params.len() > LONG_LEN - 4 {
        return Err(Error::Request("too many parameters".into()));
    }
    let mut r = [0u8; LONG_LEN];
    r[0] = LONG;
    r[1] = index;
    r[2] = feature_index;
    r[3] = (function << 4) | SWID;
    r[4..4 + params.len()].copy_from_slice(params);
    Ok(r)
}

/// If `report` answers the request `(index, feature_index, fn_sw)`, return the
/// reply bytes or the HID++ error; `None` means it is a notification.
pub fn match_report(
    report: &[u8],
    index: u8,
    feature_index: u8,
    fn_sw: u8,
) -> Option<Result<[u8; 16], Error>> {
    if report.len() < 5 || report[1] != index {
        return None;
    }
    let ours = report[3] == feature_index && report.get(4) == Some(&fn_sw);
    if report[2] == ERROR_10 && ours {
        let code = report.get(5).copied().unwrap_or(0);
        let msg = format!("receiver error: {}", error_name(&ERRORS_10, code));
        return Some(Err(if UNREACHABLE_10.contains(&code) {
            Error::Unreachable(msg)
        } else {
            Error::Hidpp(msg)
        }));
    }
    if report[2] == ERROR_20 && ours {
        let code = report.get(5).copied().unwrap_or(0);
        return Some(Err(Error::Hidpp(format!(
            "device error: {}",
            error_name(&ERRORS_20, code)
        ))));
    }
    if report[2] == feature_index && report[3] == fn_sw {
        let mut reply = [0u8; 16];
        let data = &report[4..];
        let n = data.len().min(16);
        reply[..n].copy_from_slice(&data[..n]);
        return Some(Ok(reply));
    }
    None
}

/// `(bus, vendor, product)` from the contents of a sysfs `uevent` file.
pub fn parse_hid_id(uevent: &str) -> Option<(u32, u32, u32)> {
    let line = uevent.lines().find_map(|l| l.strip_prefix("HID_ID="))?;
    let mut parts = line.trim().split(':');
    let mut next = || u32::from_str_radix(parts.next()?, 16).ok();
    Some((next()?, next()?, next()?))
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// HID++ interface: vendor usage page 0xFF00 and long report 0x11.
pub fn is_hidpp_descriptor(desc: &[u8]) -> bool {
    contains(desc, &[0x06, 0x00, 0xff]) && contains(desc, &[0x85, LONG])
}

fn sys_dir(hidraw: &str) -> String {
    let base = hidraw.rsplit('/').next().unwrap_or(hidraw);
    format!("/sys/class/hidraw/{base}/device")
}

fn hid_id(hidraw: &str) -> Option<(u32, u32, u32)> {
    parse_hid_id(&std::fs::read_to_string(format!("{}/uevent", sys_dir(hidraw))).ok()?)
}

fn is_hidpp(hidraw: &str) -> bool {
    std::fs::read(format!("{}/report_descriptor", sys_dir(hidraw)))
        .map(|d| is_hidpp_descriptor(&d))
        .unwrap_or(false)
}

/// Device indexes to try: receiver slots over USB, 0xFF over Bluetooth.
pub fn candidates(hidraw: &str) -> Vec<u8> {
    match hid_id(hidraw) {
        Some((bus, _, _)) if bus == BUS_BLUETOOTH => vec![DIRECT],
        _ => (1..=6).chain([DIRECT]).collect(),
    }
}

pub fn find_hidraw() -> Result<String, Error> {
    let mut nodes: Vec<(u32, String)> = std::fs::read_dir("/sys/class/hidraw")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let n = name.strip_prefix("hidraw")?.parse().ok()?;
            Some((n, name))
        })
        .collect();
    nodes.sort();
    for (_, name) in nodes {
        if let Some((_, vendor, _)) = hid_id(&name) {
            if vendor == LOGITECH_VENDOR && is_hidpp(&name) {
                return Ok(format!("/dev/{name}"));
            }
        }
    }
    Err(Error::Device(
        "no Logitech HID++ interface found (is the receiver plugged in?)".into(),
    ))
}

// --- Connection -------------------------------------------------------------

pub struct Device {
    auto_path: bool,
    pub path: Option<String>,
    fixed_index: Option<u8>,
    /// Index of the chosen device (discovered on the first request).
    pub index: Option<u8>,
    file: Option<File>,
    /// feature id -> index
    features: HashMap<u16, u8>,
    /// Cache for request.rs, cleared with the feature indexes.
    pub haptic_waveforms: Option<u32>,
}

impl Device {
    pub fn new(path: Option<String>, index: Option<u8>) -> Self {
        Device {
            auto_path: path.is_none(),
            path,
            fixed_index: index,
            index,
            file: None,
            features: HashMap::new(),
            haptic_waveforms: None,
        }
    }

    /// Call `function` of feature `feature_id` and return the 16 reply bytes.
    /// Opens the hidraw and looks for the device carrying the feature if needed.
    pub fn request(
        &mut self,
        feature_id: u16,
        function: u8,
        params: &[u8],
    ) -> Result<[u8; 16], Error> {
        if self.file.is_none() {
            self.open()?;
        }
        let r = self.request_inner(feature_id, function, params);
        if matches!(r, Err(Error::Unreachable(_)) | Err(Error::Timeout(_))) {
            self.forget();
        }
        r
    }

    fn request_inner(
        &mut self,
        feature_id: u16,
        function: u8,
        params: &[u8],
    ) -> Result<[u8; 16], Error> {
        if self.index.is_none() {
            self.find_device(feature_id)?;
        }
        let feature_index = self.feature_index(feature_id)?;
        let index = self.index.unwrap_or(DIRECT);
        self.exchange(index, feature_index, function, params, TIMEOUT)
    }

    /// Close the hidraw (shutdown).
    pub fn disconnect(&mut self) {
        self.close();
    }

    fn open(&mut self) -> Result<(), Error> {
        if self.path.is_none() {
            self.path = Some(find_hidraw()?);
        }
        let path = self.path.clone().unwrap_or_default();
        match OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(f) => self.file = Some(f),
            Err(e) => {
                let hint = if e.raw_os_error() == Some(libc::EACCES) {
                    " (check the udev rules / permissions)"
                } else {
                    ""
                };
                return Err(Error::Device(format!(
                    "cannot open {path}: {}{hint}",
                    strerror(&e)
                )));
            }
        }
        info!(T, "opened {path}");
        Ok(())
    }

    fn close(&mut self) {
        if self.file.take().is_none() {
            return;
        }
        self.forget();
        info!(T, "closed {}", self.path.as_deref().unwrap_or("?"));
        if self.auto_path {
            self.path = None; // the receiver may come back under another node
        }
    }

    fn forget(&mut self) {
        self.features.clear();
        self.haptic_waveforms = None;
        self.index = self.fixed_index;
    }

    fn find_device(&mut self, feature_id: u16) -> Result<(), Error> {
        let path = self.path.clone().unwrap_or_default();
        for index in candidates(&path) {
            match self.exchange(
                index,
                0,
                ROOT_GET_FEATURE,
                &feature_id.to_be_bytes(),
                PROBE_TIMEOUT,
            ) {
                Err(e) => {
                    debug!(T, "index 0x{index:02x}: {e}");
                    if self.file.is_none() {
                        return Err(e); // the hidraw went away: stop probing
                    }
                }
                Ok(reply) if reply[0] != 0 => {
                    self.index = Some(index);
                    self.features.insert(feature_id, reply[0]);
                    info!(T, "using device at index 0x{index:02x}");
                    return Ok(());
                }
                Ok(_) => {}
            }
        }
        Err(Error::Unreachable(format!(
            "no reachable device supports feature 0x{feature_id:04X} (is the mouse awake?)"
        )))
    }

    fn feature_index(&mut self, feature_id: u16) -> Result<u8, Error> {
        if feature_id == ROOT {
            return Ok(0);
        }
        if let Some(&i) = self.features.get(&feature_id) {
            return Ok(i);
        }
        let index = self.index.unwrap_or(DIRECT);
        let reply = self.exchange(
            index,
            0,
            ROOT_GET_FEATURE,
            &feature_id.to_be_bytes(),
            TIMEOUT,
        )?;
        if reply[0] == 0 {
            return Err(Error::Unsupported(format!(
                "device does not support feature 0x{feature_id:04X}"
            )));
        }
        self.features.insert(feature_id, reply[0]);
        Ok(reply[0])
    }

    fn exchange(
        &mut self,
        index: u8,
        feature_index: u8,
        function: u8,
        params: &[u8],
        timeout: Duration,
    ) -> Result<[u8; 16], Error> {
        let report = build_report(index, feature_index, function, params)?;
        let fn_sw = report[3];
        let Some(file) = self.file.as_mut() else {
            return Err(Error::Device("hidraw closed".into()));
        };
        debug!(T, "> {}", hex(&report));
        if let Err(e) = file.write_all(&report) {
            let msg = format!(
                "write to {} failed: {}",
                self.path.as_deref().unwrap_or("?"),
                strerror(&e)
            );
            self.close();
            return Err(Error::Device(msg));
        }
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 64];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout(format!(
                    "no reply from device at index 0x{index:02x}"
                )));
            }
            let Some(file) = self.file.as_mut() else {
                return Err(Error::Device("hidraw closed".into()));
            };
            let mut pfd = libc::pollfd {
                fd: file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ms = (remaining.as_millis() + 1).min(i32::MAX as u128) as i32;
            // SAFETY: pfd is a valid pollfd and we pass nfds = 1.
            let n = unsafe { libc::poll(&mut pfd, 1, ms) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                warn!(
                    T,
                    "poll on {} failed: {}",
                    self.path.as_deref().unwrap_or("?"),
                    strerror(&e)
                );
                self.close();
                return Err(Error::Device("hidraw closed".into()));
            }
            if n == 0 {
                continue; // deadline checked at the top of the loop
            }
            let len = match file.read(&mut buf) {
                Ok(0) => {
                    warn!(
                        T,
                        "read from {} failed: end of file",
                        self.path.as_deref().unwrap_or("?")
                    );
                    self.close();
                    return Err(Error::Device("hidraw closed".into()));
                }
                Ok(len) => len,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(e) => {
                    warn!(
                        T,
                        "read from {} failed: {}",
                        self.path.as_deref().unwrap_or("?"),
                        strerror(&e)
                    );
                    self.close();
                    return Err(Error::Device("hidraw closed".into()));
                }
            };
            let report = &buf[..len];
            debug!(T, "< {}", hex(report));
            if len < 5 || (report[0] != SHORT && report[0] != LONG) {
                continue;
            }
            match match_report(report, index, feature_index, fn_sw) {
                Some(result) => return result,
                None => debug!(T, "notification {}", hex(report)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_building() {
        let r = build_report(0x01, 0x05, 4, &[0x07]).unwrap();
        assert_eq!(
            hex(&r),
            "11 01 05 4a 07 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00"
        );
        assert_eq!(r.len(), 20);
        assert!(build_report(1, 0, 0, &[0; 17]).is_err());
        assert!(build_report(1, 0, 0, &[0; 16]).is_ok());
    }

    #[test]
    fn matches_reply() {
        let mut rep = vec![0x11, 0x01, 0x05, 0x4a, 0xAA, 0xBB];
        rep.resize(20, 0);
        let r = match_report(&rep, 1, 5, 0x4a).unwrap().unwrap();
        assert_eq!(&r[..2], &[0xAA, 0xBB]);
        assert_eq!(r.len(), 16);
    }

    #[test]
    fn notifications_ignored() {
        let rep = [0x11, 0x01, 0x05, 0x4a, 0, 0];
        assert!(match_report(&rep, 2, 5, 0x4a).is_none()); // other device
        assert!(match_report(&rep, 1, 6, 0x4a).is_none()); // other feature
        assert!(match_report(&rep, 1, 5, 0x4b).is_none()); // other swid
        assert!(match_report(&[0x11, 0x01, 0x05, 0x40], 1, 5, 0x4a).is_none());
    }

    #[test]
    fn receiver_errors() {
        let rep = [0x10, 0x01, 0x8F, 0x05, 0x4a, 0x04, 0x00];
        let e = match_report(&rep, 1, 5, 0x4a).unwrap().unwrap_err();
        assert_eq!(
            e,
            Error::Unreachable("receiver error: connection failed".into())
        );
        let rep = [0x10, 0x01, 0x8F, 0x05, 0x4a, 0x07, 0x00];
        let e = match_report(&rep, 1, 5, 0x4a).unwrap().unwrap_err();
        assert_eq!(e, Error::Hidpp("receiver error: busy".into()));
        let rep = [0x10, 0x01, 0x8F, 0x05, 0x4a, 0x77, 0x00];
        assert_eq!(
            match_report(&rep, 1, 5, 0x4a)
                .unwrap()
                .unwrap_err()
                .to_string(),
            "receiver error: 0x77"
        );
        // error for another request
        assert!(match_report(&rep, 1, 6, 0x4a).is_none());
    }

    #[test]
    fn device_errors() {
        let rep = [0x11, 0x01, 0xFF, 0x05, 0x4a, 0x09];
        let e = match_report(&rep, 1, 5, 0x4a).unwrap().unwrap_err();
        assert_eq!(e, Error::Hidpp("device error: unsupported".into()));
    }

    #[test]
    fn hid_id_parsing() {
        let u = "DRIVER=hid-generic\nHID_ID=0003:0000046D:0000C548\nHID_NAME=x\n";
        assert_eq!(parse_hid_id(u), Some((3, 0x46D, 0xC548)));
        assert_eq!(parse_hid_id("HID_ID=zz\n"), None);
        assert_eq!(parse_hid_id(""), None);
    }

    #[test]
    fn descriptor_detection() {
        assert!(is_hidpp_descriptor(&[
            0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x11
        ]));
        assert!(!is_hidpp_descriptor(&[0x06, 0x00, 0xff, 0x85, 0x10]));
        assert!(!is_hidpp_descriptor(&[]));
    }
}

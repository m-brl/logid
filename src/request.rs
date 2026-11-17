//! Business logic: builds HID++ requests and interprets replies.
//!
//! Knows nothing about the transport: it talks to a [`Backend`]
//! (implemented by `hidpp::Device`, or by a fake in tests).
//!
//! Haptic feature 0x19B0 and waveforms follow Solaar. The feature IDs and
//! payloads of DPI, SmartShift, hi-res wheel and battery also follow Solaar
//! and are untested on MX Master 4 hardware.

use crate::hidpp::Device;
use crate::Error;

pub const HAPTIC: u16 = 0x19B0;
pub const HAPTIC_GET_CAPABILITIES: u8 = 0; // bytes 4-7: mask of supported waveforms
pub const HAPTIC_PLAY_WAVEFORM: u8 = 4;

pub const WAVEFORMS: [(&str, u8); 16] = [
    ("sharp-state-change", 0x00),
    ("damp-state-change", 0x01),
    ("sharp-collision", 0x02),
    ("damp-collision", 0x03),
    ("subtle-collision", 0x04),
    ("happy-alert", 0x05),
    ("angry-alert", 0x06),
    ("completed", 0x07),
    ("square", 0x08),
    ("wave", 0x09),
    ("firework", 0x0A),
    ("mad", 0x0B),
    ("knock", 0x0C),
    ("jingle", 0x0D),
    ("ringing", 0x0E),
    ("whisper-collision", 0x1B),
];
pub const DEFAULT_WAVEFORM: &str = "sharp-state-change";

/// What the business logic needs from a device.
pub trait Backend {
    /// Call `function` of `feature_id`; returns the 16 reply bytes.
    fn request(&mut self, feature_id: u16, function: u8, params: &[u8]) -> Result<[u8; 16], Error>;
    /// Cached haptic waveform mask (cleared when the device is forgotten).
    fn haptic_waveforms(&self) -> Option<u32>;
    fn set_haptic_waveforms(&mut self, mask: u32);
}

impl Backend for Device {
    fn request(&mut self, feature_id: u16, function: u8, params: &[u8]) -> Result<[u8; 16], Error> {
        Device::request(self, feature_id, function, params)
    }
    fn haptic_waveforms(&self) -> Option<u32> {
        self.haptic_waveforms
    }
    fn set_haptic_waveforms(&mut self, mask: u32) {
        self.haptic_waveforms = Some(mask);
    }
}

pub fn waveform_names() -> String {
    WAVEFORMS.iter().map(|w| w.0).collect::<Vec<_>>().join(", ")
}

/// Names of the known waveforms whose id is set in `mask`.
pub fn mask_names(mask: u32) -> Vec<&'static str> {
    WAVEFORMS
        .iter()
        .filter(|(_, id)| mask >> id & 1 == 1)
        .map(|w| w.0)
        .collect()
}

/// Waveform mask advertised by the device (0 if absent), cached on the device.
pub fn supported_waveforms(dev: &mut dyn Backend) -> Result<u32, Error> {
    if let Some(mask) = dev.haptic_waveforms() {
        return Ok(mask);
    }
    let caps = dev.request(HAPTIC, HAPTIC_GET_CAPABILITIES, &[])?;
    let mask = u32::from_be_bytes([caps[4], caps[5], caps[6], caps[7]]);
    dev.set_haptic_waveforms(mask);
    Ok(mask)
}

pub fn vibrate(dev: &mut dyn Backend, name: &str) -> Result<String, Error> {
    let Some(&(_, waveform)) = WAVEFORMS.iter().find(|w| w.0 == name) else {
        return Err(Error::Request(format!(
            "unknown waveform '{name}' (expected: {})",
            waveform_names()
        )));
    };
    let mask = supported_waveforms(dev)?;
    if mask != 0 && mask >> waveform & 1 == 0 {
        return Err(Error::Request(format!(
            "waveform '{name}' not supported by this device (supported: {})",
            mask_names(mask).join(", ")
        )));
    }
    dev.request(HAPTIC, HAPTIC_PLAY_WAVEFORM, &[waveform])?;
    Ok(format!("played {name}"))
}

// --- Shared helpers -----------------------------------------------------------

/// Try each `(feature, function)` in order with the same `params`; move on to
/// the next one only when the device lacks the feature (`Unsupported`) or no
/// device with it is reachable (`Unreachable`). Returns the feature that
/// answered with its reply, or the last error.
pub fn request_first(
    dev: &mut dyn Backend,
    candidates: &[(u16, u8)],
    params: &[u8],
) -> Result<(u16, [u8; 16]), Error> {
    let mut last = Error::Request("no feature to try".into());
    for &(feature, function) in candidates {
        match dev.request(feature, function, params) {
            Ok(reply) => return Ok((feature, reply)),
            Err(e @ (Error::Unsupported(_) | Error::Unreachable(_))) => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

// --- DPI ------------------------------------------------------------------------

pub const ADJUSTABLE_DPI: u16 = 0x2201;
pub const DPI_GET_LIST: u8 = 1;
pub const DPI_GET: u8 = 2;
pub const DPI_SET: u8 = 3;
const DPI_SENSOR: u8 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpiEntry {
    Value(u16),
    Range { min: u16, max: u16, step: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpiList(pub Vec<DpiEntry>);

/// Parse the big-endian words of a getSensorDpiList reply (after the sensor
/// byte). A word >= 0xE000 is a step marker: the range runs from the previous
/// word to the next one. 0 terminates the list.
pub fn parse_dpi_list(bytes: &[u8]) -> DpiList {
    let words: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes(*c))
        .collect();
    let mut entries: Vec<DpiEntry> = vec![];
    let mut i = 0;
    while i < words.len() && words[i] != 0 {
        let w = words[i];
        if w >= 0xE000 {
            let step = w & 0x1FFF;
            let min = match entries.pop() {
                Some(DpiEntry::Value(v)) => Some(v),
                other => {
                    entries.extend(other);
                    None
                }
            };
            let max = words.get(i + 1).copied().filter(|&m| m != 0);
            if let (Some(min), Some(max)) = (min, max) {
                if step > 0 {
                    entries.push(DpiEntry::Range { min, max, step });
                    i += 2;
                    continue;
                }
            }
            // malformed range: keep what we have and stop
            entries.extend(min.map(DpiEntry::Value));
            break;
        }
        entries.push(DpiEntry::Value(w));
        i += 1;
    }
    DpiList(entries)
}

impl DpiList {
    pub fn accepts(&self, dpi: u16) -> bool {
        self.0.iter().any(|e| match *e {
            DpiEntry::Value(v) => v == dpi,
            DpiEntry::Range { min, max, step } => {
                dpi >= min && dpi <= max && (dpi - min).is_multiple_of(step)
            }
        })
    }
}

impl std::fmt::Display for DpiList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let [DpiEntry::Range { min, max, step }] = self.0[..] {
            return write!(f, "range {min}-{max} step {step}");
        }
        let parts: Vec<String> = self
            .0
            .iter()
            .map(|e| match *e {
                DpiEntry::Value(v) => v.to_string(),
                DpiEntry::Range { min, max, step } => format!("{min}-{max} step {step}"),
            })
            .collect();
        write!(f, "values: {}", parts.join(", "))
    }
}

fn dpi_list(dev: &mut dyn Backend) -> Result<DpiList, Error> {
    let r = dev.request(ADJUSTABLE_DPI, DPI_GET_LIST, &[DPI_SENSOR])?;
    Ok(parse_dpi_list(&r[1..]))
}

fn dpi_current(dev: &mut dyn Backend) -> Result<(u16, u16), Error> {
    let r = dev.request(ADJUSTABLE_DPI, DPI_GET, &[DPI_SENSOR])?;
    Ok((
        u16::from_be_bytes([r[1], r[2]]),
        u16::from_be_bytes([r[3], r[4]]),
    ))
}

pub fn dpi(dev: &mut dyn Backend, value: Option<&str>) -> Result<String, Error> {
    let Some(value) = value else {
        let (cur, default) = dpi_current(dev)?;
        let list = dpi_list(dev)?;
        return Ok(format!("dpi {cur} (default {default}, {list})"));
    };
    let Ok(wanted) = value.parse::<u16>() else {
        return Err(Error::Request("usage: dpi [value]".into()));
    };
    let list = dpi_list(dev)?;
    if !list.accepts(wanted) {
        return Err(Error::Request(format!(
            "dpi {wanted} not supported by this device ({list})"
        )));
    }
    let [hi, lo] = wanted.to_be_bytes();
    dev.request(ADJUSTABLE_DPI, DPI_SET, &[DPI_SENSOR, hi, lo])?;
    let (cur, _) = dpi_current(dev)?;
    Ok(format!("dpi {cur}"))
}

// --- SmartShift -------------------------------------------------------------------

pub const SMART_SHIFT_ENHANCED: u16 = 0x2111;
pub const SMART_SHIFT: u16 = 0x2110;
const SCROLL_GET: [(u16, u8); 2] = [(SMART_SHIFT_ENHANCED, 1), (SMART_SHIFT, 0)];
const SCROLL_SET: [(u16, u8); 2] = [(SMART_SHIFT_ENHANCED, 2), (SMART_SHIFT, 1)];
const SCROLL_USAGE: &str = "usage: scroll [ratchet|freespin|auto N]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollMode {
    Ratchet,
    Freespin,
    /// Ratchet that frees the wheel above this speed (1..=50).
    Auto(u8),
}

impl ScrollMode {
    /// Request parameters `[mode, autodisengage]`.
    pub fn encode(self) -> [u8; 2] {
        match self {
            ScrollMode::Freespin => [1, 0],
            ScrollMode::Ratchet => [2, 0xFF],
            ScrollMode::Auto(n) => [2, n],
        }
    }

    /// Decode a reply: mode 1 = freespin, 2 = ratchet (0xFF = never frees).
    pub fn decode(mode: u8, threshold: u8) -> Option<Self> {
        match (mode, threshold) {
            (1, _) => Some(ScrollMode::Freespin),
            (2, 0 | 0xFF) => Some(ScrollMode::Ratchet),
            (2, n) => Some(ScrollMode::Auto(n)),
            _ => None,
        }
    }
}

impl std::fmt::Display for ScrollMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScrollMode::Ratchet => f.write_str("ratchet"),
            ScrollMode::Freespin => f.write_str("freespin"),
            ScrollMode::Auto(n) => write!(f, "ratchet (auto-freespin above {n})"),
        }
    }
}

pub fn scroll(dev: &mut dyn Backend, args: &[&str]) -> Result<String, Error> {
    let usage = || Error::Request(SCROLL_USAGE.into());
    let mode = match args {
        [] => {
            let (_, r) = request_first(dev, &SCROLL_GET, &[])?;
            return match ScrollMode::decode(r[0], r[1]) {
                Some(m) => Ok(m.to_string()),
                None => Err(Error::Device(format!("unexpected scroll mode {}", r[0]))),
            };
        }
        ["ratchet"] => ScrollMode::Ratchet,
        ["freespin"] => ScrollMode::Freespin,
        ["auto", n] => match n.parse::<u8>() {
            Ok(n @ 1..=50) => ScrollMode::Auto(n),
            _ => {
                return Err(Error::Request(
                    "auto threshold must be between 1 and 50".into(),
                ))
            }
        },
        _ => return Err(usage()),
    };
    request_first(dev, &SCROLL_SET, &mode.encode())?;
    Ok(mode.to_string())
}

// --- Hi-res wheel -------------------------------------------------------------------

pub const HIRES_WHEEL: u16 = 0x2121;
pub const WHEEL_GET_MODE: u8 = 1;
pub const WHEEL_SET_MODE: u8 = 2;
const WHEEL_HIRES: u8 = 1 << 1;
const WHEEL_INVERT: u8 = 1 << 2;
const WHEEL_USAGE: &str = "usage: wheel [invert on|off] [hires on|off]";

fn wheel_show(mode: u8) -> String {
    let s = |bit| if mode & bit != 0 { "on" } else { "off" };
    format!("invert {}, hires {}", s(WHEEL_INVERT), s(WHEEL_HIRES))
}

pub fn wheel(dev: &mut dyn Backend, args: &[&str]) -> Result<String, Error> {
    let mut changes: Vec<(u8, bool)> = vec![];
    if !args.len().is_multiple_of(2) {
        return Err(Error::Request(WHEEL_USAGE.into()));
    }
    for pair in args.chunks(2) {
        let bit = match pair[0] {
            "invert" => WHEEL_INVERT,
            "hires" => WHEEL_HIRES,
            _ => return Err(Error::Request(WHEEL_USAGE.into())),
        };
        let on = match pair[1] {
            "on" => true,
            "off" => false,
            _ => return Err(Error::Request(WHEEL_USAGE.into())),
        };
        changes.push((bit, on));
    }
    let mut mode = dev.request(HIRES_WHEEL, WHEEL_GET_MODE, &[])?[0];
    if changes.is_empty() {
        return Ok(wheel_show(mode));
    }
    for (bit, on) in changes {
        if on {
            mode |= bit;
        } else {
            mode &= !bit;
        }
    }
    dev.request(HIRES_WHEEL, WHEEL_SET_MODE, &[mode])?;
    Ok(wheel_show(mode))
}

// --- Battery ------------------------------------------------------------------------

pub const UNIFIED_BATTERY: u16 = 0x1004;
pub const BATTERY_STATUS: u16 = 0x1000;
const BATTERY_GET: [(u16, u8); 2] = [(UNIFIED_BATTERY, 1), (BATTERY_STATUS, 0)];

/// Charging status text; the codes differ between the two features.
pub fn battery_status(feature: u16, code: u8) -> &'static str {
    match (feature, code) {
        (UNIFIED_BATTERY, 0) => "discharging",
        (UNIFIED_BATTERY, 1) => "charging",
        (UNIFIED_BATTERY, 2) => "charging (slow)",
        (UNIFIED_BATTERY, 3) => "full",
        (UNIFIED_BATTERY, 4) => "error",
        (BATTERY_STATUS, 0) => "discharging",
        (BATTERY_STATUS, 1) => "recharging",
        (BATTERY_STATUS, 2) => "almost full",
        (BATTERY_STATUS, 3) => "full",
        _ => "unknown",
    }
}

pub fn battery(dev: &mut dyn Backend) -> Result<String, Error> {
    let (feature, r) = request_first(dev, &BATTERY_GET, &[])?;
    Ok(format!("{}% ({})", r[0], battery_status(feature, r[2])))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Fake device recording calls; `mask` is what it advertises.
    pub struct Fake {
        pub mask: u32,
        pub cache: Option<u32>,
        pub calls: Vec<(u16, u8, Vec<u8>)>,
        pub fail: Option<Error>,
        /// Scripted replies per (feature, function), consumed in order; the
        /// last one is repeated.
        pub replies: HashMap<(u16, u8), Vec<[u8; 16]>>,
        /// Features that answer `Error::Unsupported`.
        pub unsupported: Vec<u16>,
    }

    impl Fake {
        pub fn new(mask: u32) -> Self {
            Fake {
                mask,
                cache: None,
                calls: vec![],
                fail: None,
                replies: HashMap::new(),
                unsupported: vec![],
            }
        }

        /// Script a reply (`data` goes at reply[0..]) for (feature, function).
        pub fn script(&mut self, feature: u16, function: u8, data: &[u8]) {
            let mut r = [0u8; 16];
            r[..data.len()].copy_from_slice(data);
            self.replies.entry((feature, function)).or_default().push(r);
        }
    }

    impl Backend for Fake {
        fn request(&mut self, f: u16, func: u8, p: &[u8]) -> Result<[u8; 16], Error> {
            self.calls.push((f, func, p.to_vec()));
            if let Some(e) = &self.fail {
                return Err(e.clone());
            }
            if self.unsupported.contains(&f) {
                return Err(Error::Unsupported(format!("feature 0x{f:04X}")));
            }
            if let Some(q) = self.replies.get_mut(&(f, func)) {
                return Ok(if q.len() > 1 { q.remove(0) } else { q[0] });
            }
            let mut r = [0u8; 16];
            r[4..8].copy_from_slice(&self.mask.to_be_bytes());
            Ok(r)
        }
        fn haptic_waveforms(&self) -> Option<u32> {
            self.cache
        }
        fn set_haptic_waveforms(&mut self, mask: u32) {
            self.cache = Some(mask);
        }
    }

    #[test]
    fn mask_to_names() {
        assert_eq!(
            mask_names(0b101),
            vec!["sharp-state-change", "sharp-collision"]
        );
        assert_eq!(mask_names(1 << 0x1B), vec!["whisper-collision"]);
        assert!(mask_names(0).is_empty());
    }

    #[test]
    fn vibrate_plays_and_caches() {
        let mut d = Fake::new(0xFFFF);
        assert_eq!(
            vibrate(&mut d, "happy-alert").unwrap(),
            "played happy-alert"
        );
        assert_eq!(d.calls[1], (HAPTIC, HAPTIC_PLAY_WAVEFORM, vec![5]));
        vibrate(&mut d, "completed").unwrap();
        assert_eq!(d.calls.len(), 3); // capabilities requested once
    }

    #[test]
    fn unknown_waveform() {
        let mut d = Fake::new(0xFFFF);
        let e = vibrate(&mut d, "x").unwrap_err().to_string();
        assert!(
            e.starts_with("unknown waveform 'x' (expected: sharp-state-change, damp-state-change")
        );
        assert!(e.ends_with("whisper-collision)"));
        assert!(d.calls.is_empty());
    }

    #[test]
    fn unsupported_waveform() {
        let mut d = Fake::new(0b11);
        let e = vibrate(&mut d, "mad").unwrap_err().to_string();
        assert_eq!(
            e,
            "waveform 'mad' not supported by this device (supported: sharp-state-change, damp-state-change)"
        );
    }

    #[test]
    fn empty_mask_allows_all() {
        let mut d = Fake::new(0);
        assert_eq!(vibrate(&mut d, "mad").unwrap(), "played mad");
    }

    #[test]
    fn transport_error_propagates() {
        let mut d = Fake::new(1);
        d.fail = Some(Error::Hidpp("device error: unsupported".into()));
        assert_eq!(
            vibrate(&mut d, "mad").unwrap_err().to_string(),
            "device error: unsupported"
        );
    }

    fn dpi_words(words: &[u16]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_be_bytes()).collect()
    }

    #[test]
    fn dpi_list_range_and_values() {
        let l = parse_dpi_list(&dpi_words(&[200, 0xE032, 8000, 0]));
        assert_eq!(
            l,
            DpiList(vec![DpiEntry::Range {
                min: 200,
                max: 8000,
                step: 50
            }])
        );
        assert_eq!(l.to_string(), "range 200-8000 step 50");
        assert!(l.accepts(1600) && l.accepts(200) && l.accepts(8000));
        assert!(!l.accepts(1625) && !l.accepts(100) && !l.accepts(8050));

        let l = parse_dpi_list(&dpi_words(&[400, 800, 1600, 0, 999]));
        assert_eq!(l.to_string(), "values: 400, 800, 1600");
        assert!(l.accepts(800) && !l.accepts(999) && !l.accepts(1000));
        // malformed trailing step marker does not panic
        let l = parse_dpi_list(&dpi_words(&[400, 0xE032]));
        assert_eq!(l.0, vec![DpiEntry::Value(400)]);
        assert!(parse_dpi_list(&[]).0.is_empty());
    }

    fn dpi_fake() -> Fake {
        let mut d = Fake::new(0);
        d.script(
            ADJUSTABLE_DPI,
            DPI_GET_LIST,
            &[0, 0, 200, 0xE0, 0x32, 0x1F, 0x40, 0, 0],
        );
        d.script(ADJUSTABLE_DPI, DPI_GET, &[0, 0x06, 0x40, 0x03, 0xE8]);
        d
    }

    #[test]
    fn dpi_show() {
        let mut d = dpi_fake();
        assert_eq!(
            dpi(&mut d, None).unwrap(),
            "dpi 1600 (default 1000, range 200-8000 step 50)"
        );
    }

    #[test]
    fn dpi_set_validates_sends_and_rereads() {
        let mut d = dpi_fake();
        d.replies
            .get_mut(&(ADJUSTABLE_DPI, DPI_GET))
            .unwrap()
            .clear();
        d.script(ADJUSTABLE_DPI, DPI_GET, &[0, 0x0C, 0x80, 0x03, 0xE8]); // 3200 after set
        assert_eq!(dpi(&mut d, Some("3200")).unwrap(), "dpi 3200");
        assert!(d
            .calls
            .contains(&(ADJUSTABLE_DPI, DPI_SET, vec![0, 0x0C, 0x80])));
        let mut d = dpi_fake();
        let e = dpi(&mut d, Some("1625")).unwrap_err().to_string();
        assert_eq!(
            e,
            "dpi 1625 not supported by this device (range 200-8000 step 50)"
        );
        assert!(!d.calls.iter().any(|c| c.1 == DPI_SET));
        assert_eq!(
            dpi(&mut d, Some("abc")).unwrap_err().to_string(),
            "usage: dpi [value]"
        );
    }

    #[test]
    fn scroll_encode_decode() {
        for m in [
            ScrollMode::Ratchet,
            ScrollMode::Freespin,
            ScrollMode::Auto(7),
        ] {
            let [a, b] = m.encode();
            assert_eq!(ScrollMode::decode(a, b), Some(m));
        }
        assert_eq!(ScrollMode::Freespin.encode(), [1, 0]);
        assert_eq!(ScrollMode::Ratchet.encode(), [2, 0xFF]);
        assert_eq!(ScrollMode::Auto(10).encode(), [2, 10]);
        assert_eq!(ScrollMode::decode(3, 0), None);
        assert_eq!(
            ScrollMode::Auto(10).to_string(),
            "ratchet (auto-freespin above 10)"
        );
    }

    #[test]
    fn scroll_get_set_and_fallback() {
        let mut d = Fake::new(0);
        d.script(SMART_SHIFT_ENHANCED, 1, &[2, 0xFF]);
        assert_eq!(scroll(&mut d, &[]).unwrap(), "ratchet");
        assert_eq!(
            scroll(&mut d, &["auto", "12"]).unwrap(),
            "ratchet (auto-freespin above 12)"
        );
        assert_eq!(d.calls[1], (SMART_SHIFT_ENHANCED, 2, vec![2, 12]));

        let mut d = Fake::new(0);
        d.unsupported.push(SMART_SHIFT_ENHANCED);
        d.script(SMART_SHIFT, 0, &[1, 0]);
        assert_eq!(scroll(&mut d, &[]).unwrap(), "freespin");
        assert_eq!(scroll(&mut d, &["freespin"]).unwrap(), "freespin");
        assert_eq!(d.calls.last().unwrap(), &(SMART_SHIFT, 1, vec![1, 0]));

        d.unsupported.push(SMART_SHIFT);
        assert!(matches!(scroll(&mut d, &[]), Err(Error::Unsupported(_))));
    }

    #[test]
    fn scroll_bad_args() {
        let mut d = Fake::new(0);
        for a in [
            &["auto"][..],
            &["auto", "0"],
            &["auto", "51"],
            &["auto", "x"],
            &["spin"],
            &["ratchet", "1"],
        ] {
            assert!(matches!(scroll(&mut d, a), Err(Error::Request(_))), "{a:?}");
        }
        assert_eq!(
            scroll(&mut d, &["spin"]).unwrap_err().to_string(),
            SCROLL_USAGE
        );
        assert!(d.calls.is_empty());
    }

    #[test]
    fn fallback_stops_on_other_errors() {
        let mut d = Fake::new(0);
        d.fail = Some(Error::Timeout("t".into()));
        assert!(matches!(scroll(&mut d, &[]), Err(Error::Timeout(_))));
        assert_eq!(d.calls.len(), 1);
        let mut d = Fake::new(0);
        d.fail = Some(Error::Unreachable("u".into()));
        assert!(matches!(battery(&mut d), Err(Error::Unreachable(_))));
        assert_eq!(d.calls.len(), 2);
    }

    #[test]
    fn wheel_show_and_modify() {
        let mut d = Fake::new(0);
        d.script(HIRES_WHEEL, 1, &[0b011]); // divert + hires
        assert_eq!(wheel(&mut d, &[]).unwrap(), "invert off, hires on");
        assert_eq!(
            wheel(&mut d, &["invert", "on", "hires", "off"]).unwrap(),
            "invert on, hires off"
        );
        assert_eq!(d.calls.last().unwrap(), &(HIRES_WHEEL, 2, vec![0b101]));
        assert_eq!(
            wheel(&mut d, &["hires", "off"]).unwrap(),
            "invert off, hires off"
        );
        assert_eq!(d.calls.last().unwrap(), &(HIRES_WHEEL, 2, vec![0b001]));
    }

    #[test]
    fn wheel_bad_args() {
        let mut d = Fake::new(0);
        for a in [
            &["invert"][..],
            &["invert", "maybe"],
            &["spin", "on"],
            &["on"],
        ] {
            assert_eq!(
                wheel(&mut d, a).unwrap_err().to_string(),
                WHEEL_USAGE,
                "{a:?}"
            );
        }
        assert!(d.calls.is_empty());
    }

    #[test]
    fn battery_unified_and_legacy() {
        let mut d = Fake::new(0);
        d.script(UNIFIED_BATTERY, 1, &[80, 0, 0]);
        assert_eq!(battery(&mut d).unwrap(), "80% (discharging)");
        let mut d = Fake::new(0);
        d.script(UNIFIED_BATTERY, 1, &[100, 0, 3]);
        assert_eq!(battery(&mut d).unwrap(), "100% (full)");

        let mut d = Fake::new(0);
        d.unsupported.push(UNIFIED_BATTERY);
        d.script(BATTERY_STATUS, 0, &[55, 0, 1]);
        assert_eq!(battery(&mut d).unwrap(), "55% (recharging)");
        let mut d = Fake::new(0);
        d.unsupported.push(UNIFIED_BATTERY);
        d.script(BATTERY_STATUS, 0, &[90, 0, 9]);
        assert_eq!(battery(&mut d).unwrap(), "90% (unknown)");
        assert_eq!(battery_status(UNIFIED_BATTERY, 2), "charging (slow)");
        assert_eq!(battery_status(UNIFIED_BATTERY, 4), "error");
    }
}

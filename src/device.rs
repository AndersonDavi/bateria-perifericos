//! What every reader (Logitech, Corsair, controllers) hands back, and the round
//! that asks all of them.

use hidapi::{HidApi, HidDevice};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Charge {
    Discharging,
    Charging,
    Full,
}

/// Shape of the tray icon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Mouse,
    Keyboard,
    Headset,
    Controller,
}

#[derive(Clone, Debug)]
pub struct Reading {
    /// Stable id: names the history file and the icon.
    pub key: String,
    pub name: String,
    pub kind: Kind,
    pub percent: u8,
    pub charge: Charge,
    /// How it was read: "receptor", "cable", "Windows"...
    pub link: &'static str,
    /// The device only reports coarse levels; the % is approximate.
    pub approx: bool,
}

/// Asks every reader once. `log` collects diagnostic lines (raw frames, ids).
/// `diag`: also listen to vendor channels and retry everything (slower).
pub fn read_all(api: &mut HidApi, log: &mut Vec<String>, diag: bool) -> Vec<Reading> {
    if let Err(e) = api.refresh_devices() {
        log.push(format!("hidapi refresh: {e}"));
    }
    let mut out = crate::hidpp::read_all(api, log);
    out.extend(crate::corsair::read_all(api, log, diag));
    out.extend(crate::gamepad::read_all(log, diag));
    if diag {
        // Controller receivers' private channels (GameSir): only listened to, no writes.
        for d in api.device_list().filter(|d| d.vendor_id() == 0x3537 && d.usage_page() >= 0xFF00) {
            if let Ok(dev) = api.open_path(d.path()) {
                let label = format!("[Mando HID] {:04x} {:04x}:{:04x}", d.product_id(), d.usage_page(), d.usage());
                listen(&dev, &label, Duration::from_secs(3), log);
            }
        }
    }
    // One icon per device even if it answered on two paths (receiver + cable).
    let mut seen = std::collections::HashSet::new();
    out.retain(|r| seen.insert(r.key.clone()));
    out
}

/// "G305 X SUPERLIGHT" -> "g305-x-superlight".
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Logs whatever arrives on a collection during `window` (no writes).
pub fn listen(dev: &HidDevice, label: &str, window: Duration, log: &mut Vec<String>) {
    let start = Instant::now();
    let mut buf = [0u8; 65];
    let mut frames = 0;
    while start.elapsed() < window && frames < 15 {
        let n = dev.read_timeout(&mut buf, 200).unwrap_or(0);
        if n > 0 {
            frames += 1;
            log.push(format!("{label} <- {}", hex(&buf[..n.min(24)])));
        }
    }
    if frames == 0 {
        log.push(format!("{label}: nada en {} s", window.as_secs()));
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

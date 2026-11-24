//! AULA F108 Pro keyboard on its 2.4 GHz dongle.
//!
//! Taken from the official AULA driver (DeviceDriver.exe 1.0.0.1, the function at
//! 0x4357d0 and its send/receive helper at 0x44f4e0): every 15 s, and only in
//! wireless mode, it writes a 33-byte output report to the dongle
//! `[00, 20, 01, 00 ..., byte 32 = sum of the bytes before]` and expects an answer
//! starting `20 01 00` with the battery % in byte 3. The dongle (05AC:024F, it
//! borrows Apple's vendor id) takes it on its FF60 collection.
//!
//! On the cable (0C45:800A) the keyboard charges and the driver asks nothing: the
//! last % read wirelessly is shown as charging.

use crate::device::{hex, Charge, Kind, Reading};
use hidapi::HidApi;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const NAME: &str = "AULA F108 Pro";
const KEY: &str = "aula-f108-pro";
const DONGLE: (u16, u16) = (0x05AC, 0x024F);
const WIRED: (u16, u16) = (0x0C45, 0x800A);

/// Last % read over the dongle.
static LAST: Mutex<Option<u8>> = Mutex::new(None);

pub fn read_all(api: &HidApi, log: &mut Vec<String>) -> Vec<Reading> {
    let wired = api.device_list().any(|d| (d.vendor_id(), d.product_id()) == WIRED);
    if wired {
        // Charging over the cable; the keyboard doesn't report a level here.
        let Some(last) = *LAST.lock().unwrap() else {
            log.push("[AULA] por cable, sin lectura previa por el dongle".into());
            return Vec::new();
        };
        return vec![reading(last, Charge::Charging, "cable", true)];
    }
    let Some(d) = api
        .device_list()
        .find(|d| (d.vendor_id(), d.product_id()) == DONGLE && d.usage_page() == 0xFF60)
    else {
        return Vec::new();
    };
    let Ok(dev) = api.open_path(d.path()) else {
        log.push("[AULA] no se pudo abrir el dongle".into());
        return Vec::new();
    };
    let mut out = [0u8; 33];
    out[1] = 0x20;
    out[2] = 0x01;
    out[32] = out.iter().fold(0u8, |a, b| a.wrapping_add(*b));
    if let Err(e) = dev.write(&out) {
        log.push(format!("[AULA] pedido rechazado ({e})"));
        return Vec::new();
    }
    let start = Instant::now();
    let mut buf = [0u8; 64];
    while start.elapsed() < Duration::from_millis(600) {
        let n = dev.read_timeout(&mut buf, 50).unwrap_or(0);
        if n < 4 || buf[..3] != [0x20, 0x01, 0x00] {
            continue;
        }
        log.push(format!("[AULA] <- {}", hex(&buf[..n.min(16)])));
        // 0 = no answer from the keyboard (off, asleep or out of range).
        let level = buf[3];
        if level == 0 || level > 100 {
            return Vec::new();
        }
        *LAST.lock().unwrap() = Some(level);
        return vec![reading(level, Charge::Discharging, "receptor", false)];
    }
    log.push("[AULA] el dongle no contestó".into());
    Vec::new()
}

fn reading(percent: u8, charge: Charge, link: &'static str, approx: bool) -> Reading {
    Reading { key: KEY.into(), name: NAME.into(), kind: Kind::Keyboard, percent, charge, link, approx }
}

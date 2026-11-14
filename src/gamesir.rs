//! GameSir Cyclone 2: real battery from the receiver's private channel.
//!
//! Windows' battery report for this pad is made up (always "full"). The receiver's
//! vendor collection (usage page 0xFFF0, usage 0x40) streams report 0x12 with the
//! pad's state once it gets a heartbeat (`0f f2 00`), the same "still here" message
//! GameSir Connect sends. In that report byte 35 is the charge state (1 = charging)
//! and byte 36 the battery in percent, read the same way GameSir Connect does.
//!
//! While the pad sits on its charging base it drops off the receiver: the receiver
//! changes to 3537:0575 and streams 0x12 frames full of zeros, so nothing is read.

use crate::device::{hex, slug, Charge, Kind, Reading};
use hidapi::HidApi;
use std::time::{Duration, Instant};

const GAMESIR: u16 = 0x3537;
/// Receivers whose 0x12 report carries the pad's battery: (pid, name).
const RECEIVERS: &[(u16, &str)] = &[(0x100B, "GameSir Cyclone 2")];

const STATE_REPORT: u8 = 0x12;
const CHARGE_BYTE: usize = 35;
const LEVEL_BYTE: usize = 36;

/// Replaces the Windows reading of a known GameSir pad with the receiver's own, or
/// adds it when Windows didn't list the pad yet.
pub fn apply(api: &HidApi, out: &mut Vec<Reading>, log: &mut Vec<String>) {
    for &(pid, name) in RECEIVERS {
        let Some(d) = api
            .device_list()
            .find(|d| d.vendor_id() == GAMESIR && d.product_id() == pid && d.usage_page() == 0xFFF0 && d.usage() == 0x0040)
        else {
            continue;
        };
        let Ok(dev) = api.open_path(d.path()) else {
            log.push(format!("[GameSir] {pid:04x}: no se pudo abrir el canal privado"));
            continue;
        };
        let mut hb = [0u8; 64];
        hb[..3].copy_from_slice(&[0x0F, 0xF2, 0x00]);
        if let Err(e) = dev.write(&hb) {
            log.push(format!("[GameSir] {pid:04x}: heartbeat rechazado ({e})"));
            continue;
        }
        let start = Instant::now();
        let mut buf = [0u8; 65];
        let mut got = None;
        while start.elapsed() < Duration::from_millis(1500) {
            let n = dev.read_timeout(&mut buf, 200).unwrap_or(0);
            if n <= LEVEL_BYTE || buf[0] != STATE_REPORT {
                continue;
            }
            // All zeros = receiver without a pad (pad on the base or off).
            if buf[1..CHARGE_BYTE].iter().all(|&b| b == 0) {
                log.push(format!("[GameSir] {pid:04x}: receptor sin mando"));
                break;
            }
            log.push(format!("[GameSir] {pid:04x} <- {}", hex(&buf[..n.min(40)])));
            got = Some((buf[LEVEL_BYTE], buf[CHARGE_BYTE]));
            break;
        }
        let Some((level, state)) = got else { continue };
        if level > 100 {
            log.push(format!("[GameSir] {pid:04x}: nivel fuera de rango ({level})"));
            continue;
        }
        let charge = match state {
            1 if level >= 100 => Charge::Full,
            1 => Charge::Charging,
            _ => Charge::Discharging,
        };
        log.push(format!("[GameSir] {name}: {level} % estado={state}"));
        let key = slug(name);
        match out.iter_mut().find(|r| r.key == key) {
            Some(r) => {
                r.percent = level;
                r.charge = charge;
                r.link = "receptor";
                r.approx = false;
            }
            None => out.push(Reading {
                key,
                name: name.to_string(),
                kind: Kind::Controller,
                percent: level,
                charge,
                link: "receptor",
                approx: false,
            }),
        }
    }
}

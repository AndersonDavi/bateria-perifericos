//! Logitech over HID++ 2.0, without G HUB.
//!
//! Talks to the "long" vendor collection (usage page 0xFF00 usage 0x0002, or
//! 0xFF43/0x0302 on newer devices) of a receiver (LIGHTSPEED / Unifying /
//! Bolt; device index 1..6) or of a device plugged in by cable (index 0xFF).
//! Battery from feature 0x1004 Unified Battery, falling back to 0x1000 and
//! 0x1001 (voltage). Name and type from feature 0x0005.

use crate::device::{hex, slug, Charge, Kind, Reading};
use hidapi::{HidApi, HidDevice};
use std::time::{Duration, Instant};

const LOGITECH: u16 = 0x046D;
const LONG_ID: u8 = 0x11;
const LONG_LEN: usize = 20;
const SW_ID: u8 = 0x0B;
const TIMEOUT: Duration = Duration::from_millis(1500);

const F_DEVICE_NAME: u16 = 0x0005;
const F_BATTERY_STATUS: u16 = 0x1000;
const F_BATTERY_VOLTAGE: u16 = 0x1001;
const F_UNIFIED_BATTERY: u16 = 0x1004;

/// The receiver answered "no device at this index" (asleep, off or unpaired).
struct Unreachable;

struct Link<'a> {
    dev: &'a HidDevice,
    /// Short-report collection of the same interface. Receivers send
    /// HID++ 1.0 errors (e.g. "device not connected") there.
    short: Option<&'a HidDevice>,
    index: u8,
}

impl Link<'_> {
    /// Long HID++ 2.0 request; the 16 parameter bytes of the answer, or None on
    /// a HID++ error / timeout.
    fn call(&self, feature: u8, function: u8, params: &[u8], log: &mut Vec<String>) -> Result<Option<[u8; 16]>, Unreachable> {
        let mut msg = [0u8; LONG_LEN];
        msg[0] = LONG_ID;
        msg[1] = self.index;
        msg[2] = feature;
        msg[3] = (function << 4) | SW_ID;
        msg[4..4 + params.len()].copy_from_slice(params);
        if self.dev.write(&msg).is_err() {
            return Ok(None);
        }
        let start = Instant::now();
        let mut buf = [0u8; 64];
        while start.elapsed() < TIMEOUT {
            let mut n = self.dev.read_timeout(&mut buf, 25).unwrap_or(0);
            if n == 0 {
                if let Some(short) = self.short {
                    n = short.read_timeout(&mut buf, 0).unwrap_or(0);
                }
            }
            if n < 7 || buf[1] != self.index {
                continue;
            }
            // HID++ 2.0 error: [id, idx, 0xFF, feature, fn|sw, code]
            if buf[2] == 0xFF && buf[3] == feature && buf[4] == msg[3] {
                return Ok(None);
            }
            // HID++ 1.0 error from the receiver: [0x10, idx, 0x8F, ...]
            if buf[2] == 0x8F && buf[3] == feature {
                return Err(Unreachable);
            }
            if buf[0] == LONG_ID && buf[2] == feature && buf[3] == msg[3] {
                if log.len() < 400 {
                    log.push(format!("    idx {:02x} f{feature:02x}.{function} -> {}", self.index, hex(&buf[4..12])));
                }
                let mut out = [0u8; 16];
                out.copy_from_slice(&buf[4..20]);
                return Ok(Some(out));
            }
        }
        Ok(None)
    }

    /// IRoot.GetFeature: feature id -> feature index (None if absent).
    fn feature(&self, id: u16, log: &mut Vec<String>) -> Result<Option<u8>, Unreachable> {
        let r = self.call(0x00, 0, &id.to_be_bytes(), log)?;
        Ok(r.map(|p| p[0]).filter(|&i| i != 0))
    }

    /// (name, kind) from feature 0x0005.
    fn identity(&self, log: &mut Vec<String>) -> Result<(String, Kind), Unreachable> {
        let fallback = || ("Dispositivo Logitech".to_string(), Kind::Mouse);
        let Some(fi) = self.feature(F_DEVICE_NAME, log)? else { return Ok(fallback()) };
        let Some(len) = self.call(fi, 0, &[], log)? else { return Ok(fallback()) };
        let len = len[0] as usize;
        let mut name = Vec::new();
        while name.len() < len {
            match self.call(fi, 1, &[name.len() as u8], log)? {
                Some(chunk) => name.extend(chunk.iter().take(len - name.len()).copied()),
                None => break,
            }
        }
        let kind = match self.call(fi, 2, &[], log)?.map(|p| p[0]) {
            Some(0) | Some(2) => Kind::Keyboard,
            Some(8) => Kind::Headset,
            Some(0x0B) | Some(0x0C) => Kind::Controller,
            _ => Kind::Mouse,
        };
        let name = String::from_utf8_lossy(&name).trim_end_matches('\0').trim().to_string();
        Ok((if name.is_empty() { fallback().0 } else { name }, kind))
    }

    /// (percent, charge, approximate).
    fn battery(&self, log: &mut Vec<String>) -> Result<Option<(u8, Charge, bool)>, Unreachable> {
        if let Some(fi) = self.feature(F_UNIFIED_BATTERY, log)? {
            // Asked twice: a device waking up has answered garbage once
            // (15 % "charging" between two 93 % readings). Only matching answers count.
            let first = self.call(fi, 1, &[], log)?;
            let second = self.call(fi, 1, &[], log)?;
            if first.is_some() && (first.map(|p| (p[0], p[2])) != second.map(|p| (p[0], p[2]))) {
                log.push("    dos respuestas distintas; lectura descartada".into());
                return Ok(None);
            }
            if let Some(p) = second {
                let (pct, approx) = match p[0] {
                    0 => (
                        // Only coarse levels reported.
                        match p[1] {
                            l if l & 8 != 0 => 100,
                            l if l & 4 != 0 => 60,
                            l if l & 2 != 0 => 20,
                            _ => 5,
                        },
                        true,
                    ),
                    v => (v.min(100), false),
                };
                let charge = match p[2] {
                    1 | 2 => Charge::Charging,
                    3 => Charge::Full,
                    _ => Charge::Discharging,
                };
                return Ok(Some((pct, charge, approx)));
            }
        }
        if let Some(fi) = self.feature(F_BATTERY_STATUS, log)? {
            if let Some(p) = self.call(fi, 0, &[], log)? {
                let charge = match p[2] {
                    1 | 2 | 4 => Charge::Charging,
                    3 => Charge::Full,
                    _ => Charge::Discharging,
                };
                return Ok(Some((p[0].min(100), charge, false)));
            }
        }
        if let Some(fi) = self.feature(F_BATTERY_VOLTAGE, log)? {
            if let Some(p) = self.call(fi, 0, &[], log)? {
                let mv = u16::from_be_bytes([p[0], p[1]]);
                let flags = p[2];
                let charge = if flags & 0x80 != 0 {
                    if flags & 0x03 == 1 { Charge::Full } else { Charge::Charging }
                } else {
                    Charge::Discharging
                };
                return Ok(Some((volts_to_percent(mv), charge, true)));
            }
        }
        Ok(None)
    }
}

/// Li-ion discharge curve (same points Solaar uses).
fn volts_to_percent(mv: u16) -> u8 {
    const T: [(u16, u8); 13] = [
        (4186, 100), (4067, 90), (3989, 80), (3922, 70), (3859, 60), (3811, 50),
        (3778, 40), (3751, 30), (3717, 20), (3671, 10), (3646, 5), (3579, 2), (3500, 0),
    ];
    if mv >= T[0].0 {
        return 100;
    }
    for w in T.windows(2) {
        let (hi_v, hi_p) = w[0];
        let (lo_v, lo_p) = w[1];
        if mv >= lo_v {
            let f = (mv - lo_v) as f32 / (hi_v - lo_v) as f32;
            return (lo_p as f32 + f * (hi_p - lo_p) as f32).round() as u8;
        }
    }
    0
}

fn is_receiver(pid: u16) -> bool {
    // Unifying / Nano / LIGHTSPEED / Bolt receivers.
    matches!(pid, 0xC52B | 0xC532 | 0xC534 | 0xC539 | 0xC53A | 0xC53D | 0xC53F
        | 0xC541 | 0xC545 | 0xC547 | 0xC548 | 0xC54D)
}

/// Every Logitech device reachable now: all receiver slots first, then cables.
pub fn read_all(api: &HidApi, log: &mut Vec<String>) -> Vec<Reading> {
    let list: Vec<_> = api.device_list().filter(|d| d.vendor_id() == LOGITECH).collect();
    let mut cands: Vec<_> = list
        .iter()
        .filter(|d| matches!((d.usage_page(), d.usage()), (0xFF00, 0x0002) | (0xFF43, 0x0302)))
        .map(|d| {
            let short = list
                .iter()
                .find(|s| {
                    s.product_id() == d.product_id()
                        && s.interface_number() == d.interface_number()
                        && s.usage_page() == d.usage_page()
                        && s.usage() == d.usage() - 1
                })
                .map(|s| s.path().to_owned());
            (d.path().to_owned(), short, d.product_id())
        })
        .collect();
    cands.sort_by_key(|c| !is_receiver(c.2));

    let mut out = Vec::new();
    for (path, short_path, pid) in cands {
        let rx = is_receiver(pid);
        log.push(format!("[Logitech] {pid:04x} {}", if rx { "receptor" } else { "cable" }));
        let Ok(dev) = api.open_path(&path) else {
            log.push("  no se pudo abrir".into());
            continue;
        };
        let short = short_path.and_then(|p| api.open_path(&p).ok());
        let indexes: &[u8] = if rx { &[1, 2, 3, 4, 5, 6] } else { &[0xFF] };
        for &index in indexes {
            let link = Link { dev: &dev, short: short.as_ref(), index };
            let got = link.battery(log).and_then(|b| Ok(b.map(|b| (b, link.identity(log)))));
            match got {
                Ok(Some(((percent, charge, approx), Ok((name, kind))))) => {
                    log.push(format!("  idx {index:02x}: {name} {percent} % {charge:?}"));
                    out.push(Reading {
                        key: slug(&name),
                        name,
                        kind,
                        percent,
                        charge,
                        link: if rx { "receptor" } else { "cable" },
                        approx,
                    });
                }
                Ok(_) => {}
                Err(Unreachable) => {
                    if rx {
                        log.push(format!("  idx {index:02x}: no conectado (apagado, dormido o vacío)"));
                    }
                }
            }
        }
    }
    out
}

/// `--probe`: one line per device.
pub fn probe(api: &mut HidApi, log: &mut Vec<String>) -> Vec<String> {
    crate::device::read_all(api, log)
        .into_iter()
        .map(|r| format!("{} · {} % · {:?} ({}){}", r.name, r.percent, r.charge, r.link, if r.approx { " ~" } else { "" }))
        .collect()
}

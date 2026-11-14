//! GameSir Cyclone 2: real battery from the receiver's private channel.
//!
//! Windows' battery report for this pad is made up (always "full"). The receiver's
//! vendor collection (usage page 0xFFF0, usage 0x40) streams report 0x12 with the
//! pad's state once it gets a heartbeat (`0f f2 00`), the same "still here" message
//! GameSir Connect sends. In that report byte 35 is the charge state (1 = charging)
//! and byte 36 the battery in percent, read the same way GameSir Connect does.
//!
//! While the pad sits on its charging base it is off: the receiver changes to
//! 3537:0575 and streams 0x12 frames full of zeros, the same as with the pad off
//! anywhere else. But a pad docked while on reports "charging" for under a second
//! before it goes off, so a watcher thread keeps listening while the pad is
//! connected: "last said charging, then vanished" means it sits on the base.

use crate::device::{hex, slug, Charge, Kind, Reading};
use hidapi::{HidApi, HidDevice};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const GAMESIR: u16 = 0x3537;
/// Receivers whose 0x12 report carries the pad's battery: (pid, name).
const RECEIVERS: &[(u16, &str)] = &[(0x100B, "GameSir Cyclone 2")];

/// The receiver's id while no pad is connected to it.
const IDLE_RECEIVER: u16 = 0x0575;
/// Battery % the pad had when it went onto its base (see the module notes).
static DOCKED: Mutex<Option<u8>> = Mutex::new(None);
/// Bumped on every USB arrival/removal; tells the watcher to look again.
pub static USB_GEN: AtomicU64 = AtomicU64::new(1);
/// Set by the watcher when the pad starts or stops charging, or goes onto its base:
/// the tray should read again now instead of waiting for the next round.
pub static CHANGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_docked(percent: u8) {
    *DOCKED.lock().unwrap() = Some(percent);
}

fn is_channel(d: &hidapi::DeviceInfo, pid: u16) -> bool {
    d.vendor_id() == GAMESIR && d.product_id() == pid && d.usage_page() == 0xFFF0 && d.usage() == 0x0040
}

fn heartbeat(dev: &HidDevice) -> bool {
    let mut hb = [0u8; 64];
    hb[..3].copy_from_slice(&[0x0F, 0xF2, 0x00]);
    dev.write(&hb).is_ok()
}

/// (charge state, percent) of a live state frame; None for anything else.
fn parse(buf: &[u8]) -> Option<(u8, u8)> {
    if buf.len() <= LEVEL_BYTE || buf[0] != STATE_REPORT || buf[1..CHARGE_BYTE].iter().all(|&b| b == 0) {
        return None;
    }
    (buf[LEVEL_BYTE] <= 100).then_some((buf[CHARGE_BYTE], buf[LEVEL_BYTE]))
}

/// Listens to the pad for as long as it is connected, to catch the moment it is
/// put on its base. Idle (no reads, no scans) while no pad is connected.
pub fn spawn_watcher() {
    std::thread::spawn(|| {
        let Ok(mut api) = HidApi::new() else { return };
        let mut seen_gen = 0;
        let mut rescans = 0u8;
        loop {
            // Devices take a moment to settle after an arrival: look a few times.
            let g = USB_GEN.load(Ordering::Relaxed);
            if g != seen_gen {
                seen_gen = g;
                rescans = 4;
            }
            if rescans == 0 {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            rescans -= 1;
            let _ = api.refresh_devices();
            let dev = RECEIVERS
                .iter()
                .find_map(|&(pid, _)| api.device_list().find(|d| is_channel(d, pid)))
                .and_then(|d| api.open_path(d.path()).ok());
            let Some(dev) = dev else {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            };
            let mut last = None;
            let mut last_hb = Instant::now() - Duration::from_secs(2);
            let mut last_frame = Instant::now();
            let mut buf = [0u8; 65];
            loop {
                if last_hb.elapsed() >= Duration::from_millis(900) {
                    last_hb = Instant::now();
                    if !heartbeat(&dev) {
                        break;
                    }
                }
                match dev.read_timeout(&mut buf, 250) {
                    Ok(n) => {
                        if let Some(state) = parse(&buf[..n]) {
                            if last.is_some_and(|l: (u8, u8)| l.0 != state.0) {
                                CHANGED.store(true, Ordering::Relaxed);
                            }
                            last = Some(state);
                            last_frame = Instant::now();
                        }
                    }
                    Err(_) => break,
                }
                if last_frame.elapsed() > Duration::from_secs(3) {
                    break;
                }
            }
            if let Some((1, percent)) = last {
                set_docked(percent);
                CHANGED.store(true, Ordering::Relaxed);
            }
            rescans = 4;
        }
    });
}

const STATE_REPORT: u8 = 0x12;
const CHARGE_BYTE: usize = 35;
const LEVEL_BYTE: usize = 36;

/// Replaces the Windows reading of a known GameSir pad with the receiver's own, or
/// adds it when Windows didn't list the pad yet.
pub fn apply(api: &HidApi, out: &mut Vec<Reading>, log: &mut Vec<String>) {
    for &(pid, name) in RECEIVERS {
        let Some(d) = api.device_list().find(|d| is_channel(d, pid)) else {
            // No pad on the receiver. Still on its base if that is where it went;
            // with the receiver itself unplugged, the base has no power either.
            if !api.device_list().any(|d| is_channel(d, IDLE_RECEIVER)) {
                *DOCKED.lock().unwrap() = None;
            } else if let Some(percent) = *DOCKED.lock().unwrap() {
                log.push(format!("[GameSir] {name}: en la base (tenía {percent} % al dejarlo)"));
                out.retain(|r| r.key != slug(name));
                out.push(Reading {
                    key: slug(name),
                    name: name.to_string(),
                    kind: Kind::Controller,
                    percent,
                    charge: Charge::Charging,
                    link: "base",
                    approx: true,
                });
            }
            continue;
        };
        let Ok(dev) = api.open_path(d.path()) else {
            log.push(format!("[GameSir] {pid:04x}: no se pudo abrir el canal privado"));
            continue;
        };
        if !heartbeat(&dev) {
            log.push(format!("[GameSir] {pid:04x}: heartbeat rechazado"));
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
        *DOCKED.lock().unwrap() = None;
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

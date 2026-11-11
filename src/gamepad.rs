//! Controllers (GameSir, Xbox and other Xbox-protocol pads).
//!
//! 1. Windows.Gaming.Input battery report: real percentage and charging state.
//!    Third-party pads on their own 2.4 GHz receiver (e.g. GameSir) often report
//!    nothing through XInput but do report here (finding from HaloBattery).
//!    Called directly through WinRT, no PowerShell.
//! 2. XInputGetBatteryInformation, only when (1) found nothing: four levels
//!    (empty / low / medium / full), so the % is approximate.

use crate::device::{Charge, Kind, Reading};
use std::cell::Cell;
use windows::Foundation::EventHandler;
use windows::Gaming::Input::{Gamepad, RawGameController};
use windows::System::Power::BatteryStatus;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::Input::XboxController::{
    XInputGetBatteryInformation, BATTERY_DEVTYPE_GAMEPAD, BATTERY_LEVEL_EMPTY, BATTERY_LEVEL_FULL,
    BATTERY_LEVEL_LOW, BATTERY_LEVEL_MEDIUM, BATTERY_TYPE_ALKALINE, BATTERY_TYPE_DISCONNECTED, BATTERY_TYPE_NIMH,
    BATTERY_TYPE_WIRED, XINPUT_BATTERY_INFORMATION,
};

thread_local! {
    static WINRT: Cell<bool> = const { Cell::new(false) };
}

fn ensure_winrt() {
    WINRT.with(|w| {
        if !w.get() {
            unsafe {
                let _ = RoInitialize(RO_INIT_MULTITHREADED);
            }
            // The controller list fills asynchronously, and only once someone
            // listens for arrivals: subscribe (no-op handler, kept for the
            // process lifetime) and touch the list so later polls see the pads.
            let _ = RawGameController::RawGameControllerAdded(&EventHandler::new(|_, _| Ok(())));
            let _ = RawGameController::RawGameControllers();
            w.set(true);
        }
    });
}

fn vendor_name(vid: u16) -> &'static str {
    match vid {
        0x3537 => "Mando GameSir",
        0x045E => "Mando Xbox",
        0x2DC8 => "Mando 8BitDo",
        0x0E6F | 0x24C6 => "Mando PDP / PowerA",
        _ => "Mando",
    }
}

pub fn read_all(log: &mut Vec<String>, diag: bool) -> Vec<Reading> {
    ensure_winrt();
    if diag {
        // A fresh process (--probe, first poll) may still be filling the list.
        for _ in 0..10 {
            if RawGameController::RawGameControllers().and_then(|l| l.Size()).unwrap_or(0) > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
    let mut out = Vec::new();
    let mut pads: Vec<(Reading, bool)> = Vec::new();
    match RawGameController::RawGameControllers() {
        Ok(list) => {
            if diag {
                log.push(format!("[Mando WGI] {} controles vistos por Windows", list.Size().unwrap_or(0)));
            }
            for c in list {
                let vid = c.HardwareVendorId().unwrap_or(0);
                let pid = c.HardwareProductId().unwrap_or(0);
                let display = c.DisplayName().map(|s| s.to_string()).unwrap_or_default();
                // Only what Windows also exposes as a gamepad (not wheels, sticks...).
                if Gamepad::FromGameController(&c).is_err() {
                    log.push(format!("[Mando WGI] {vid:04x}:{pid:04x} '{display}': no es gamepad, se ignora"));
                    continue;
                }
                let wireless = c.IsWireless().unwrap_or(false);
                let report = c.TryGetBatteryReport().ok();
                let remain = report.as_ref().and_then(|r| r.RemainingCapacityInMilliwattHours().ok()).and_then(|v| v.Value().ok());
                let full = report.as_ref().and_then(|r| r.FullChargeCapacityInMilliwattHours().ok()).and_then(|v| v.Value().ok());
                let status = report.as_ref().and_then(|r| r.Status().ok());
                let rate = report.as_ref().and_then(|r| r.ChargeRateInMilliwatts().ok()).and_then(|v| v.Value().ok());
                log.push(format!(
                    "[Mando WGI] {vid:04x}:{pid:04x} '{display}' inalámbrico={wireless} estado={status:?} restante={remain:?} total={full:?} ritmo_mW={rate:?}"
                ));
                let (Some(remain), Some(full)) = (remain, full) else { continue };
                if full <= 0 {
                    continue;
                }
                let percent = (remain as f64 * 100.0 / full as f64).round().clamp(0.0, 100.0) as u8;
                let charge = match status {
                    Some(BatteryStatus::Charging) => Charge::Charging,
                    Some(BatteryStatus::Idle) if percent >= 95 => Charge::Full,
                    Some(BatteryStatus::Idle) => Charge::Charging,
                    _ => Charge::Discharging,
                };
                let generic = display.is_empty() || display.to_lowercase().contains("xbox");
                let name = if generic || vid == 0x3537 { vendor_name(vid).to_string() } else { display };
                // GameSir pads report one id on the cable and another through their
                // receiver: one key so they keep a single icon and history.
                let key = if vid == 0x3537 { "mando-gamesir".to_string() } else { format!("mando-{vid:04x}-{pid:04x}") };
                pads.push((
                    Reading {
                        key,
                        name,
                        kind: Kind::Controller,
                        percent,
                        charge,
                        // IsWireless is false for pads on a USB receiver too, so it can't tell.
                        link: if wireless { "inalámbrico" } else { "receptor" },
                        approx: false,
                    },
                    wireless,
                ));
            }
        }
        Err(e) => log.push(format!("[Mando WGI] error: {e}")),
    }

    // The battery report says "discharging" for a pad on a USB cable even while it
    // charges (seen on the GameSir Cyclone 2). XInput's battery type does say
    // "wired": a pad on the cable is getting power.
    let types = xinput_types();
    let wired = types.iter().filter(|&&t| t == BATTERY_TYPE_WIRED.0).count();
    let unsure_pads = pads.iter().filter(|p| !p.1).count();
    if wired > 0 && (pads.len() == 1 || wired == types.len()) {
        for (r, wireless) in pads.iter_mut() {
            if !*wireless {
                r.link = "cable";
                if r.charge == Charge::Discharging {
                    r.charge = if r.percent >= 100 { Charge::Full } else { Charge::Charging };
                }
            }
        }
    } else if diag && wired > 0 {
        log.push(format!("[Mando] {wired} ranura(s) XInput por cable y {unsure_pads} mando(s) sin asignar; no se marca carga"));
    }
    out.extend(pads.into_iter().map(|p| p.0));

    if out.is_empty() {
        out.extend(xinput(log, diag));
    } else if diag {
        let _ = xinput(log, diag);
    }
    out
}

/// Battery type of every connected XInput slot.
fn xinput_types() -> Vec<u8> {
    (0..4u32)
        .filter_map(|slot| {
            let mut info = XINPUT_BATTERY_INFORMATION::default();
            let rc = unsafe { XInputGetBatteryInformation(slot, BATTERY_DEVTYPE_GAMEPAD, &mut info) };
            (rc == 0 && info.BatteryType != BATTERY_TYPE_DISCONNECTED).then_some(info.BatteryType.0)
        })
        .collect()
}

fn xinput(log: &mut Vec<String>, diag: bool) -> Vec<Reading> {
    let mut out = Vec::new();
    for slot in 0..4u32 {
        let mut info = XINPUT_BATTERY_INFORMATION::default();
        let rc = unsafe { XInputGetBatteryInformation(slot, BATTERY_DEVTYPE_GAMEPAD, &mut info) };
        if rc != 0 {
            if diag {
                // 1167 = ERROR_DEVICE_NOT_CONNECTED
                log.push(format!("[Mando XInput] ranura {slot}: código {rc}{}", if rc == 1167 { " (vacía)" } else { "" }));
            }
            continue;
        }
        log.push(format!("[Mando XInput] ranura {slot}: tipo={:?} nivel={:?}", info.BatteryType, info.BatteryLevel));
        // Only real batteries; "wired" and "unknown" carry no level.
        if info.BatteryType != BATTERY_TYPE_ALKALINE && info.BatteryType != BATTERY_TYPE_NIMH && info.BatteryType.0 != 0x02 && info.BatteryType.0 != 0x03 {
            continue;
        }
        let percent = match info.BatteryLevel {
            BATTERY_LEVEL_EMPTY => 5,
            BATTERY_LEVEL_LOW => 25,
            BATTERY_LEVEL_MEDIUM => 60,
            BATTERY_LEVEL_FULL => 100,
            _ => continue,
        };
        out.push(Reading {
            key: format!("mando-xinput-{slot}"),
            name: format!("Mando {}", slot + 1),
            kind: Kind::Controller,
            percent,
            charge: Charge::Discharging,
            link: "XInput",
            approx: true,
        });
    }
    out
}

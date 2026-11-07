//! Windows notifications sent from the existing tray icon (Shell_NotifyIcon
//! NIF_INFO): no extra library, thread or window. Windows 10/11 shows them as toasts.

use crate::device::Charge;
use windows_sys::Win32::Foundation::HWND;
use std::sync::atomic::{AtomicPtr, Ordering};
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIcon, DestroyIcon, HICON};

/// Charge levels that trigger a notice while discharging.
pub const THRESHOLDS: [u8; 6] = [50, 25, 15, 10, 7, 5];

/// Balloon icon of the last notice; Windows may still be showing it, so it's
/// only destroyed when the next one replaces it.
static LAST_ICON: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

/// `image`: straight RGBA + side, shown as the notice's picture (the battery
/// icon) instead of Windows' generic info/warning glyph.
/// Returns the icon id Windows accepted, or None.
pub fn show(hwnd: HWND, title: &str, body: &str, warning: bool, image: Option<(&[u8], u32)>) -> Option<u32> {
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO };
        if let Some(hicon) = image.and_then(|(rgba, side)| make_icon(rgba, side)) {
            nid.hBalloonIcon = hicon;
            nid.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON;
            let old = LAST_ICON.swap(hicon, Ordering::Relaxed);
            if !old.is_null() {
                DestroyIcon(old);
            }
        }
        copy(&mut nid.szInfoTitle, title);
        copy(&mut nid.szInfo, body);
        // tray-icon doesn't expose its icon id; it's a small counter, and the
        // wrong ids are rejected, so the first accepted one is ours.
        for id in 1..=16 {
            nid.uID = id;
            if Shell_NotifyIconW(NIM_MODIFY, &nid) != 0 {
                return Some(id);
            }
        }
    }
    None
}

/// 32-bit HICON from straight RGBA (Windows wants BGRA; an all-zero AND mask
/// lets the alpha channel do the transparency).
fn make_icon(rgba: &[u8], side: u32) -> Option<HICON> {
    let bgra: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
    let mask = vec![0u8; (side.div_ceil(16) * 2 * side) as usize];
    let h = unsafe {
        CreateIcon(std::ptr::null_mut(), side as i32, side as i32, 1, 32, mask.as_ptr(), bgra.as_ptr())
    };
    (!h.is_null()).then_some(h)
}

fn copy(dst: &mut [u16], s: &str) {
    let max = dst.len() - 1;
    for (d, c) in dst.iter_mut().zip(s.encode_utf16().take(max)) {
        *d = c;
    }
}

/// Decides which notices are due. Lives in the UI thread, fed with every reading.
#[derive(Default)]
pub struct Alerts {
    /// Thresholds already announced in the current discharge.
    done: Vec<u8>,
    primed: bool,
    prev: Option<Charge>,
    /// 90 % "unplug it" reminder enabled / already sent in this charge.
    pub unplug_reminder: bool,
    unplug_sent: bool,
}

/// Level at which the "you can unplug it" reminder fires while charging.
pub const UNPLUG_AT: u8 = 90;

pub enum Notice {
    Low(u8),
    Unplug(u8),
    Full,
}

impl Alerts {
    pub fn new(unplug_reminder: bool) -> Self {
        Alerts { unplug_reminder, ..Default::default() }
    }

    /// `min_since_charge`: lowest level already logged in this discharge before this
    /// reading (so a restart doesn't repeat notices that were already shown).
    pub fn check(&mut self, pct: u8, charge: Charge, min_since_charge: Option<u8>) -> Option<Notice> {
        if !self.primed {
            self.primed = true;
            if let Some(m) = min_since_charge {
                self.done.extend(THRESHOLDS.iter().filter(|&&t| t >= m));
            }
            // Already plugged in above the reminder level at start: don't remind.
            self.unplug_sent = charge != Charge::Discharging && pct >= UNPLUG_AT;
            self.prev = Some(charge);
        }
        let prev = self.prev.replace(charge);

        if charge != Charge::Discharging {
            self.done.clear();
            let finished = charge == Charge::Full && prev == Some(Charge::Charging);
            if finished {
                return Some(Notice::Full);
            }
            if self.unplug_reminder && !self.unplug_sent && charge == Charge::Charging && pct >= UNPLUG_AT {
                self.unplug_sent = true;
                return Some(Notice::Unplug(pct));
            }
            return None;
        }
        self.unplug_sent = false;
        let due: Vec<u8> = THRESHOLDS
            .iter()
            .copied()
            .filter(|&t| pct <= t && !self.done.contains(&t))
            .collect();
        if due.is_empty() {
            return None;
        }
        self.done.extend(&due);
        Some(Notice::Low(pct))
    }
}

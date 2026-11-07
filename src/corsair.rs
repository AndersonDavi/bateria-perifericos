//! Corsair wireless devices over their USB receiver, without iCUE.
//!
//! Protocol notes taken from HaloBattery (MIT, providers/corsair.py), which in
//! turn documents what HeadsetControl, ckb-next and OpenLinkHub found:
//!
//! * "bragi" / Slipstream (newer receivers, vendor page 0xFF42 usage 1 on
//!   interface 4; usage 2 is a notice channel): get-property frames to a route
//!   (0x08 = the receiver, 0x09 = the paired device). Three framings exist and
//!   which one Windows accepts depends on the receiver's report ids:
//!     framed   `00 <route> 02 <prop>` (65 bytes, report id 0; Dark Core)
//!     bare     `02 <route> 02 <prop>` (64 bytes; Virtuoso SE)
//!     unframed `<route> 02 <prop>`    (64 bytes)
//!   Session first (firmware 0x13 to the receiver, heartbeat 0x12 to receiver and
//!   device). Answers `01 02 00 <v LE>` or `01 01 02 00 <v LE>`; property 0x0F =
//!   battery in tenths of %, 0x10 = charge state (1 charging, 2 on battery).
//!   The answer can take ~5 s.
//! * "Void v2" family (HeadsetControl): 65-byte writes `00 02 <endpoint> 02
//!   <command>` on interface 4; handshake receiver firmware (0x13) + heartbeat
//!   (0x12), headset heartbeat; battery 0x0F -> reply[4] | reply[5] << 8.
//!
//! The method and framing that answer are remembered per receiver.

use crate::device::{hex, slug, Charge, Kind, Reading};
use hidapi::{DeviceInfo, HidApi, HidDevice};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CORSAIR: u16 = 0x1B1C;

/// Known receivers. Others with a 0xFF42 collection are tried too, named after
/// the product string.
const KNOWN: &[(u16, &str, Kind)] = &[
    (0x2A0F, "Corsair HS80 MAX Wireless", Kind::Headset),
    (0x0A97, "Corsair HS80 MAX Wireless", Kind::Headset),
    (0x2A08, "Corsair Void v2 Wireless", Kind::Headset),
    (0x2A02, "Corsair Virtuoso Max Wireless", Kind::Headset),
    (0x0A40, "Corsair Virtuoso RGB Wireless SE", Kind::Headset),
    (0x0A3E, "Corsair Virtuoso RGB Wireless SE", Kind::Headset),
    (0x0A64, "Corsair Virtuoso RGB Wireless XT", Kind::Headset),
    (0x1B7F, "Corsair Dark Core RGB Pro SE", Kind::Mouse),
];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Framing {
    Framed,
    Bare,
    Unframed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Method {
    Bragi(Framing),
    Void,
}

struct Memo {
    works: Option<Method>,
    /// Last time every method was tried and none answered.
    failed_at: Option<Instant>,
}

static MEMO: Mutex<Option<HashMap<u16, Memo>>> = Mutex::new(None);
/// While nothing has worked, retry the full set at most this often.
const RETRY_ALL: Duration = Duration::from_secs(10 * 60);

fn name_and_kind(pid: u16, product: &str) -> (String, Kind) {
    if let Some(&(_, n, k)) = KNOWN.iter().find(|k| k.0 == pid) {
        return (n.to_string(), k);
    }
    let name = product
        .replace("Gaming Receiver", "")
        .replace("Wireless Receiver", "Wireless")
        .replace("Receiver", "")
        .trim()
        .to_string();
    let upper = name.to_uppercase();
    let kind = if ["HS", "VOID", "VIRTUOSO", "HEADSET"].iter().any(|k| upper.contains(k)) {
        Kind::Headset
    } else if upper.starts_with('K') || upper.contains("KEYBOARD") {
        Kind::Keyboard
    } else {
        Kind::Mouse
    };
    let name = if upper.starts_with("CORSAIR") { name } else { format!("Corsair {name}") };
    (name, kind)
}

/// `diag`: try every method regardless of what was remembered, and listen to the
/// notice channel.
pub fn read_all(api: &HidApi, log: &mut Vec<String>, diag: bool) -> Vec<Reading> {
    let list: Vec<&DeviceInfo> = api.device_list().filter(|d| d.vendor_id() == CORSAIR).collect();
    if list.is_empty() {
        return Vec::new();
    }
    for d in &list {
        log.push(format!(
            "[Corsair] {:04x} if={} up={:04x} u={:04x} {:?}",
            d.product_id(), d.interface_number(), d.usage_page(), d.usage(), d.product_string()
        ));
    }
    let mut pids: Vec<u16> = list
        .iter()
        .filter(|d| KNOWN.iter().any(|k| k.0 == d.product_id()) || d.usage_page() == 0xFF42)
        .map(|d| d.product_id())
        .collect();
    pids.sort();
    pids.dedup();

    let mut out = Vec::new();
    for pid in pids {
        let mine: Vec<&&DeviceInfo> = list.iter().filter(|d| d.product_id() == pid).collect();
        let product = mine.iter().find_map(|d| d.product_string()).unwrap_or("");
        let (name, kind) = name_and_kind(pid, product);
        let bragi_paths: Vec<_> = mine
            .iter()
            .filter(|d| d.usage_page() == 0xFF42 && d.usage() == 0x0001)
            .map(|d| d.path().to_owned())
            .collect();
        let void_paths: Vec<_> = mine.iter().filter(|d| d.interface_number() == 4).map(|d| d.path().to_owned()).collect();

        if diag {
            for d in mine.iter().filter(|d| d.usage_page() == 0xFF42 && d.usage() == 0x0002) {
                if let Ok(dev) = api.open_path(d.path()) {
                    crate::device::listen(&dev, "  canal de avisos ff42:0002", Duration::from_secs(2), log);
                }
            }
        }

        let mut memo = MEMO.lock().unwrap();
        let memo = memo.get_or_insert_with(HashMap::new).entry(pid).or_insert(Memo { works: None, failed_at: None });
        let all = || {
            let mut v = Vec::new();
            if !bragi_paths.is_empty() {
                v.extend([Method::Bragi(Framing::Bare), Method::Bragi(Framing::Framed), Method::Bragi(Framing::Unframed)]);
            }
            if !void_paths.is_empty() {
                v.push(Method::Void);
            }
            v
        };
        let methods: Vec<Method> = match memo.works {
            Some(m) if !diag => vec![m],
            None if !diag && memo.failed_at.is_some_and(|t| t.elapsed() < RETRY_ALL) => {
                log.push("  (sin respuesta hace poco; se reintenta en unos minutos)".into());
                vec![]
            }
            _ => all(),
        };

        let mut got = None;
        'methods: for m in &methods {
            let paths = if matches!(m, Method::Void) { &void_paths } else { &bragi_paths };
            for path in paths {
                let Ok(dev) = api.open_path(path) else {
                    log.push(format!("  {m:?}: no se pudo abrir"));
                    continue;
                };
                log.push(format!("  probando {m:?}"));
                got = match m {
                    Method::Void => void_query(&dev, log).map(|p| (p, Charge::Discharging)),
                    Method::Bragi(f) => bragi_query(&dev, *f, log),
                };
                if got.is_some() {
                    memo.works = Some(*m);
                    break 'methods;
                }
            }
        }
        if got.is_none() && !methods.is_empty() {
            memo.failed_at = Some(Instant::now());
            memo.works = None;
        }
        if let Some((percent, charge)) = got {
            log.push(format!("  -> {percent} % {charge:?}"));
            out.push(Reading { key: slug(&name), name, kind, percent, charge, link: "receptor", approx: false });
        }
    }
    out
}

// ---------- bragi / Slipstream ----------

fn bragi_frame(f: Framing, route: u8, prop: u8) -> Vec<u8> {
    match f {
        Framing::Framed => {
            let mut v = vec![0u8; 65];
            v[1..4].copy_from_slice(&[route, 0x02, prop]);
            v
        }
        Framing::Bare => {
            let mut v = vec![0u8; 64];
            v[..4].copy_from_slice(&[0x02, route, 0x02, prop]);
            v
        }
        Framing::Unframed => {
            let mut v = vec![0u8; 64];
            v[..3].copy_from_slice(&[route, 0x02, prop]);
            v
        }
    }
}

/// Raw 16-bit value of a get-property answer, or None if the frame isn't one.
fn bragi_value(reply: &[u8]) -> Option<u16> {
    let d = if reply.len() >= 65 { &reply[1..] } else { reply };
    if d.len() < 6 || d[0] != 0x01 {
        return None;
    }
    if d[1] == 0x02 && d[2] == 0x00 {
        return Some(d[3] as u16 | (d[4] as u16) << 8);
    }
    if d[1] == 0x01 && d[2] == 0x02 && d[3] == 0x00 {
        return Some(d[4] as u16 | (d[5] as u16) << 8);
    }
    None
}

fn bragi_write(dev: &HidDevice, f: Framing, route: u8, prop: u8, log: &mut Vec<String>) -> bool {
    match dev.write(&bragi_frame(f, route, prop)) {
        Ok(n) if n > 0 => true,
        Ok(_) => {
            log.push(format!("    {f:?} r{route:02x} p{prop:02x}: escritura de 0 bytes"));
            false
        }
        Err(e) => {
            log.push(format!("    {f:?} r{route:02x} p{prop:02x}: escritura rechazada ({e})"));
            false
        }
    }
}

/// Waits up to `window` for a get-property answer; logs every frame.
fn bragi_answer(dev: &HidDevice, window: Duration, log: &mut Vec<String>) -> Option<u16> {
    let start = Instant::now();
    let mut buf = [0u8; 65];
    let mut reads = 0;
    while start.elapsed() < window && reads < 96 {
        reads += 1;
        let n = dev.read_timeout(&mut buf, 300).unwrap_or(0);
        if n == 0 {
            continue;
        }
        if log.len() < 800 {
            log.push(format!("    <- {}", hex(&buf[..n.min(16)])));
        }
        if let Some(v) = bragi_value(&buf[..n]) {
            return Some(v);
        }
    }
    None
}

fn bragi_query(dev: &HidDevice, f: Framing, log: &mut Vec<String>) -> Option<(u8, Charge)> {
    // Session: firmware query and heartbeat to the receiver, heartbeat to the device.
    for (route, prop) in [(0x08, 0x13), (0x08, 0x12), (0x09, 0x12)] {
        if !bragi_write(dev, f, route, prop, log) {
            return None; // this framing is refused: try the next one
        }
        let _ = bragi_answer(dev, Duration::from_millis(800), log);
    }
    let level = |v: Option<u16>| v.filter(|v| (1..=1000).contains(v));
    let mut first = None;
    let mut route = 0x09;
    for r in [0x09, 0x08] {
        if !bragi_write(dev, f, r, 0x0F, log) {
            return None;
        }
        first = level(bragi_answer(dev, Duration::from_secs(6), log));
        if first.is_some() {
            route = r;
            break;
        }
        log.push(format!("    sin respuesta de batería en la ruta {r:02x}"));
    }
    let first = first?;
    // Confirm: another app on the dongle can put other values on the channel.
    if bragi_write(dev, f, route, 0x0F, log) {
        match level(bragi_answer(dev, Duration::from_secs(3), log)) {
            Some(second) if second / 10 != first / 10 => {
                log.push(format!("    lecturas distintas ({first} / {second}); descartadas"));
                return None;
            }
            Some(_) => {}
            None => log.push("    (la confirmación no respondió; se toma la primera lectura)".into()),
        }
    }
    let charge = if bragi_write(dev, f, route, 0x10, log) {
        match bragi_answer(dev, Duration::from_secs(3), log) {
            Some(1) => Charge::Charging,
            _ => Charge::Discharging,
        }
    } else {
        Charge::Discharging
    };
    Some(((first / 10) as u8, charge))
}

// ---------- Void v2 ----------

fn void_frame(endpoint: u8, command: u8) -> [u8; 65] {
    let mut f = [0u8; 65];
    f[1] = 0x02;
    f[2] = endpoint;
    f[3] = 0x02;
    f[4] = command;
    f
}

fn drain(dev: &HidDevice) {
    let mut buf = [0u8; 65];
    for _ in 0..4 {
        if dev.read_timeout(&mut buf, 30).unwrap_or(0) == 0 {
            return;
        }
    }
}

fn void_query(dev: &HidDevice, log: &mut Vec<String>) -> Option<u8> {
    if dev.write(&void_frame(0x08, 0x13)).is_err() {
        log.push("    escritura rechazada".into());
        return None;
    }
    let _ = dev.write(&void_frame(0x08, 0x12));
    drain(dev);
    let _ = dev.write(&void_frame(0x09, 0x12));
    let mut buf = [0u8; 65];
    if dev.read_timeout(&mut buf, 500).unwrap_or(0) == 0 {
        log.push("    heartbeat del audífono: sin respuesta".into());
        return None;
    }
    drain(dev);
    for attempt in 1..=3 {
        let _ = dev.write(&void_frame(0x09, 0x0F));
        let n = dev.read_timeout(&mut buf, 500).unwrap_or(0);
        if n == 0 {
            log.push(format!("    intento {attempt}: sin respuesta"));
            continue;
        }
        log.push(format!("    intento {attempt}: {}", hex(&buf[..n.min(16)])));
        if n > 5 {
            let v = buf[4] as u16 | (buf[5] as u16) << 8;
            if (1..=1000).contains(&v) {
                return Some((v / 10) as u8);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bragi_answers() {
        // Dark Core shape: 01 02 00 <v LE> (0x0226 = 550 -> 55 %)
        assert_eq!(bragi_value(&[0x01, 0x02, 0x00, 0x26, 0x02, 0x00]), Some(550));
        // Virtuoso shape: 01 01 02 00 <v LE>
        assert_eq!(bragi_value(&[0x01, 0x01, 0x02, 0x00, 0xFD, 0x02]), Some(765));
        // Other traffic is ignored.
        assert_eq!(bragi_value(&[0x02, 0x02, 0x00, 0x26, 0x02, 0x00]), None);
    }

    #[test]
    fn frames() {
        assert_eq!(&bragi_frame(Framing::Framed, 0x09, 0x0F)[..4], &[0x00, 0x09, 0x02, 0x0F]);
        assert_eq!(&bragi_frame(Framing::Bare, 0x09, 0x0F)[..4], &[0x02, 0x09, 0x02, 0x0F]);
        assert_eq!(&bragi_frame(Framing::Unframed, 0x08, 0x13)[..3], &[0x08, 0x02, 0x13]);
        assert_eq!(&void_frame(0x09, 0x0F)[..5], &[0x00, 0x02, 0x09, 0x02, 0x0F]);
    }

    #[test]
    fn names() {
        assert_eq!(name_and_kind(0x2A0F, "x").0, "Corsair HS80 MAX Wireless");
        let (n, k) = name_and_kind(0x2AFF, "CORSAIR VOID RGB ELITE Gaming Receiver");
        assert_eq!((n.as_str(), k), ("CORSAIR VOID RGB ELITE", Kind::Headset));
    }
}

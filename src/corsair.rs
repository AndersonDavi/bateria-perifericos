//! Corsair wireless headsets over their USB receiver, without iCUE.
//!
//! Protocol notes taken from HaloBattery (MIT, providers/corsair.py), which in
//! turn documents what HeadsetControl and ckb-next / OpenLinkHub found. Neither
//! project has confirmed the HS80 Max on real hardware, so both known exchanges
//! are tried and the one that answers is remembered per receiver:
//!
//! * "Void v2" family (HeadsetControl): collection on interface 4, 65-byte
//!   writes `00 02 <endpoint> 02 <command>`; endpoint 0x08 = receiver, 0x09 =
//!   headset. Handshake: receiver firmware (0x13) + heartbeat (0x12), headset
//!   heartbeat; then battery (0x0F) -> reply[4] | reply[5] << 8 = 0..1000
//!   (tenths of %). No charging flag.
//! * "bragi" / Slipstream (ckb-next, OpenLinkHub): vendor page 0xFF42 usage 1,
//!   get-property frames to route 0x09 (the headset). Framed `00 09 02 <prop>`
//!   (65 bytes) or bare `02 09 02 <prop>` (64 bytes); answers `01 02 00 <v LE>`
//!   or `01 01 02 00 <v LE>`. Property 0x0F battery (tenths of %), 0x10
//!   charge state (1 charging, 2 on battery).

use crate::device::{hex, slug, Charge, Kind, Reading};
use hidapi::{HidApi, HidDevice};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CORSAIR: u16 = 0x1B1C;

/// Receivers (and the HS80 Max cable id) worth asking.
const PIDS: &[(u16, &str)] = &[
    (0x0A97, "Corsair HS80 Max Wireless"),
    (0x0A96, "Corsair HS80 Max Wireless"),
    (0x2A08, "Corsair Void v2 Wireless"),
    (0x2A02, "Corsair Virtuoso Max Wireless"),
    (0x0A40, "Corsair Virtuoso RGB Wireless SE"),
    (0x0A64, "Corsair Virtuoso RGB Wireless XT"),
];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Method {
    Void,
    BragiFramed,
    BragiBare,
}

struct Memo {
    works: Option<Method>,
    /// Last time every method was tried and none answered.
    failed_at: Option<Instant>,
}

/// Which exchange each receiver answers; avoids retrying all of them every poll.
static MEMO: Mutex<Option<HashMap<u16, Memo>>> = Mutex::new(None);
/// While nothing has worked, retry the full set at most this often.
const RETRY_ALL: Duration = Duration::from_secs(10 * 60);

pub fn read_all(api: &HidApi, log: &mut Vec<String>) -> Vec<Reading> {
    let list: Vec<_> = api.device_list().filter(|d| d.vendor_id() == CORSAIR).collect();
    if list.is_empty() {
        return Vec::new();
    }
    for d in &list {
        log.push(format!(
            "[Corsair] {:04x} if={} up={:04x} u={:04x} {:?}",
            d.product_id(), d.interface_number(), d.usage_page(), d.usage(), d.product_string()
        ));
    }
    let mut out = Vec::new();
    for &(pid, name) in PIDS {
        let mine: Vec<_> = list.iter().filter(|d| d.product_id() == pid).collect();
        if mine.is_empty() {
            continue;
        }
        let mut memo = MEMO.lock().unwrap();
        let memo = memo.get_or_insert_with(HashMap::new).entry(pid).or_insert(Memo { works: None, failed_at: None });
        let methods: Vec<Method> = match memo.works {
            Some(m) => vec![m],
            None if memo.failed_at.is_some_and(|t| t.elapsed() < RETRY_ALL) => vec![],
            None => vec![Method::Void, Method::BragiFramed, Method::BragiBare],
        };
        let mut got = None;
        for m in &methods {
            let paths: Vec<_> = mine
                .iter()
                .filter(|d| match m {
                    Method::Void => d.interface_number() == 4,
                    _ => d.usage_page() == 0xFF42 && d.usage() == 0x0001,
                })
                .map(|d| d.path().to_owned())
                .collect();
            for path in paths {
                let Ok(dev) = api.open_path(&path) else {
                    log.push(format!("  {m:?}: no se pudo abrir"));
                    continue;
                };
                log.push(format!("  probando {m:?}"));
                got = match m {
                    Method::Void => void_query(&dev, log).map(|p| (p, Charge::Discharging)),
                    Method::BragiFramed => bragi_query(&dev, true, log),
                    Method::BragiBare => bragi_query(&dev, false, log),
                };
                if got.is_some() {
                    memo.works = Some(*m);
                    break;
                }
            }
            if got.is_some() {
                break;
            }
        }
        if got.is_none() && memo.works.is_none() && !methods.is_empty() {
            memo.failed_at = Some(Instant::now());
        }
        if let Some((percent, charge)) = got {
            log.push(format!("  -> {percent} % {charge:?}"));
            out.push(Reading {
                key: slug(name),
                name: name.to_string(),
                kind: Kind::Headset,
                percent,
                charge,
                link: "receptor",
                approx: false,
            });
        }
    }
    out
}

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
    // Handshake that wakes a sleeping headset without switching it to software mode.
    let _ = dev.write(&void_frame(0x08, 0x13));
    let _ = dev.write(&void_frame(0x08, 0x12));
    drain(dev);
    let _ = dev.write(&void_frame(0x09, 0x12));
    let mut buf = [0u8; 65];
    if dev.read_timeout(&mut buf, 500).unwrap_or(0) == 0 {
        log.push("    heartbeat del audífono: sin respuesta (apagado o dormido)".into());
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

fn bragi_frame(framed: bool, prop: u8) -> Vec<u8> {
    if framed {
        let mut f = vec![0u8; 65];
        f[1] = 0x09;
        f[2] = 0x02;
        f[3] = prop;
        f
    } else {
        let mut f = vec![0u8; 64];
        f[0] = 0x02;
        f[1] = 0x09;
        f[2] = 0x02;
        f[3] = prop;
        f
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

/// Asks one property and waits up to `window` for a matching answer.
fn bragi_ask(dev: &HidDevice, framed: bool, prop: u8, window: Duration, log: &mut Vec<String>) -> Option<u16> {
    if dev.write(&bragi_frame(framed, prop)).is_err() {
        log.push(format!("    prop {prop:02x}: escritura rechazada"));
        return None;
    }
    let start = Instant::now();
    let mut buf = [0u8; 65];
    let mut reads = 0;
    while start.elapsed() < window && reads < 96 {
        reads += 1;
        let n = dev.read_timeout(&mut buf, 300).unwrap_or(0);
        if n == 0 {
            continue;
        }
        if log.len() < 600 {
            log.push(format!("    prop {prop:02x} <- {}", hex(&buf[..n.min(12)])));
        }
        if let Some(v) = bragi_value(&buf[..n]) {
            return Some(v);
        }
    }
    None
}

fn bragi_query(dev: &HidDevice, framed: bool, log: &mut Vec<String>) -> Option<(u8, Charge)> {
    // Session the Virtuoso SE receiver needed: firmware + heartbeat properties.
    let _ = bragi_ask(dev, framed, 0x13, Duration::from_millis(600), log);
    let _ = bragi_ask(dev, framed, 0x12, Duration::from_millis(600), log);
    // Two equal answers: another app on the dongle can put other values on the channel.
    let a = bragi_ask(dev, framed, 0x0F, Duration::from_secs(6), log).filter(|v| (1..=1000).contains(v))?;
    let b = bragi_ask(dev, framed, 0x0F, Duration::from_secs(3), log).filter(|v| (1..=1000).contains(v))?;
    if a / 10 != b / 10 {
        log.push(format!("    lecturas distintas ({a} / {b}); descartadas"));
        return None;
    }
    let charge = match bragi_ask(dev, framed, 0x10, Duration::from_secs(2), log) {
        Some(1) => Charge::Charging,
        _ => Charge::Discharging,
    };
    Some(((b / 10) as u8, charge))
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
        assert_eq!(&void_frame(0x09, 0x0F)[..5], &[0x00, 0x02, 0x09, 0x02, 0x0F]);
        assert_eq!(&bragi_frame(true, 0x0F)[..4], &[0x00, 0x09, 0x02, 0x0F]);
        assert_eq!(bragi_frame(false, 0x0F).len(), 64);
    }
}

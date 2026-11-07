//! Battery history per device: one CSV row each time the percentage or the charge
//! state changes, and the estimates computed from it.
//!
//! File: %LOCALAPPDATA%\BateriaPerifericos\historial-<device>.csv
//! Columns: unix,fecha,porcentaje,estado,via,uso_s
//!   estado: bateria|cargando|completa
//!   uso_s:  seconds the device has been awake and on battery, cumulative.
//!
//! Time left is estimated from *use*, not the clock (approach from HaloBattery):
//! only the time between consecutive readings with the device awake counts, a
//! least-squares line is fitted to level vs. use since the last charge, and the
//! result is also given in calendar time using the recent ratio of use to clock.

use crate::device::{Charge, Reading};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// The most one gap between two readings may add to the use time.
const MAX_GAP_S: u64 = 300;
/// Use time and drop needed since the charge before the use-based estimate shows.
const MIN_USE_S: f64 = 1800.0;
const MIN_USE_DROP: f64 = 3.0;
/// Clock-based fallback: window and minimum drop.
const RATE_WINDOW_S: u64 = 30 * 24 * 3600;
const MIN_DROP: u32 = 4;
/// Window for the use-to-clock ratio.
const RATIO_WINDOW_S: u64 = 7 * 24 * 3600;
/// Finished discharges needed before judging battery health.
const HEALTH_MIN: usize = 6;

#[derive(Clone, Copy)]
struct Row {
    t: u64,
    pct: u8,
    st: Charge,
    uso: Option<f64>,
}

pub struct History {
    rows: Vec<Row>,
    path: Option<PathBuf>,
    /// Copy of the CSV in a synced folder (e.g. Google Drive), from `respaldo.txt`.
    backup: Option<PathBuf>,
    /// Result of the last backup copy: Ok(time) / Err(reason).
    backup_state: Option<Result<String, String>>,
    /// Cumulative use time (s) and the last awake on-battery reading.
    usage: f64,
    last_awake: Option<u64>,
}

#[derive(Clone, Copy)]
pub struct Session {
    pub t0: u64,
    pub t1: u64,
    pub p0: u8,
    pub p1: u8,
    pub done: bool,
}

pub enum Health {
    Measuring { have: usize, need: usize },
    /// change_pct < 0 = lasts less than at the start. Hours per full charge.
    Known { change_pct: f64, first_h: f64, last_h: f64, of_use: bool },
}

pub struct LastCharge {
    pub ago_s: u64,
    pub from: u8,
    pub to: u8,
    pub dur_s: u64,
    pub ongoing: bool,
}

pub struct Stats {
    /// Hours of use a full charge lasts, and left now (use-based).
    pub use_full_h: Option<f64>,
    pub use_left_h: Option<f64>,
    /// Calendar hours left at the recent pace of use.
    pub cal_left_h: Option<f64>,
    /// Clock-based fallback (when there's no use data yet).
    pub full_h: Option<f64>,
    pub left_h: Option<f64>,
    /// Equivalent full cycles discharged since tracking began.
    pub cycles: f64,
    pub since_s: Option<u64>,
    pub last_charge: Option<LastCharge>,
}

impl Stats {
    /// " · quedan ~14 h de uso (≈ 3 días)" or "" when unknown.
    pub fn left_text(&self) -> String {
        match (self.use_left_h, self.cal_left_h, self.left_h) {
            (Some(u), Some(c), _) if c > u * 1.3 => format!(" · quedan ~{} de uso (≈ {})", dur_h(u), dur_h(c)),
            (Some(u), _, _) => format!(" · quedan ~{} de uso", dur_h(u)),
            (None, _, Some(w)) => format!(" · quedan ~{}", dur_h(w)),
            _ => String::new(),
        }
    }

    pub fn life_text(&self) -> Option<String> {
        match (self.use_full_h, self.full_h) {
            (Some(u), _) => Some(format!("~{} de uso por carga", dur_h(u))),
            (None, Some(w)) => Some(format!("~{} por carga", dur_h(w))),
            _ => None,
        }
    }
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("BateriaPerifericos"))
}

fn state_str(c: Charge) -> &'static str {
    match c {
        Charge::Discharging => "bateria",
        Charge::Charging => "cargando",
        Charge::Full => "completa",
    }
}

impl History {
    /// History of one device. `adopt_legacy`: take over `historial-legado.csv`
    /// (the single-mouse history of earlier versions) if this device has none.
    pub fn load(key: &str, adopt_legacy: bool) -> Self {
        let dir = data_dir();
        let file = format!("historial-{key}.csv");
        let path = dir.as_ref().map(|d| d.join(&file));
        if let (Some(d), Some(p)) = (&dir, &path) {
            let legacy = d.join("historial-legado.csv");
            if adopt_legacy && !p.exists() && legacy.exists() {
                let _ = std::fs::rename(&legacy, p);
            }
        }
        // respaldo.txt: first line = folder where the CSVs are mirrored.
        let backup = dir
            .as_ref()
            .and_then(|d| std::fs::read_to_string(d.join("respaldo.txt")).ok())
            .and_then(|t| {
                t.lines()
                    .map(|l| l.trim().trim_start_matches('\u{feff}'))
                    .find(|l| !l.is_empty())
                    .map(PathBuf::from)
            })
            .map(|d| d.join(&file));

        // Restore: the backup holds more history than the local file (reinstall, new PC...).
        if let (Some(local), Some(bk)) = (&path, &backup) {
            if size(bk) > size(local) {
                let _ = std::fs::create_dir_all(local.parent().unwrap());
                let _ = std::fs::copy(bk, local);
            }
        }
        let rows = path.as_ref().map(parse).unwrap_or_default();
        let usage = rows.iter().rev().find_map(|r| r.uso).unwrap_or(0.0);
        History { rows, path, backup, backup_state: None, usage, last_awake: None }
    }

    /// Read-only history from any CSV (no backup); used by `--grafico`.
    pub fn from_file(path: PathBuf) -> Self {
        let rows = parse(&path);
        let usage = rows.iter().rev().find_map(|r| r.uso).unwrap_or(0.0);
        History { rows, path: Some(path), backup: None, backup_state: None, usage, last_awake: None }
    }

    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    /// Adds use time: called with every successful reading.
    pub fn tick(&mut self, charge: Charge) {
        let now = now();
        if charge == Charge::Discharging {
            if let Some(prev) = self.last_awake {
                self.usage += now.saturating_sub(prev).min(MAX_GAP_S) as f64;
            }
            self.last_awake = Some(now);
        } else {
            self.last_awake = None;
        }
    }

    /// The device didn't answer (asleep, off): the next reading starts a new stretch.
    pub fn asleep(&mut self) {
        self.last_awake = None;
    }

    /// Copies the local CSV to the backup folder. `hhmm` is the local time.
    pub fn sync(&mut self, hhmm: &str) {
        let (Some(local), Some(bk)) = (self.path.clone(), self.backup.clone()) else { return };
        if !local.exists() {
            return;
        }
        // The backup showed up after start with more history than here: merge instead
        // of overwriting (backup rows + local rows newer than the backup's last one).
        if size(&bk) > size(&local) {
            if let (Ok(b), Ok(l)) = (std::fs::read_to_string(&bk), std::fs::read_to_string(&local)) {
                let ts = |line: &str| line.split(',').next().and_then(|t| t.parse::<u64>().ok());
                let last_b = b.lines().filter_map(ts).max().unwrap_or(0);
                let mut merged = b.trim_end().to_string();
                for line in l.lines().filter(|x| ts(x).is_some_and(|t| t > last_b)) {
                    merged.push('\n');
                    merged.push_str(line);
                }
                merged.push('\n');
                if std::fs::write(&local, merged).is_ok() {
                    self.rows = parse(&local);
                    self.usage = self.rows.iter().rev().find_map(|r| r.uso).unwrap_or(self.usage);
                }
            }
        }
        let r = bk
            .parent()
            .ok_or_else(|| "ruta inválida".to_string())
            .and_then(|d| {
                if !d.parent().map(|p| p.exists()).unwrap_or(false) {
                    return Err("carpeta de respaldo no disponible".into());
                }
                std::fs::create_dir_all(d).map_err(|e| e.to_string())
            })
            .and_then(|_| std::fs::copy(&local, &bk).map_err(|e| e.to_string()))
            .map(|_| hhmm.to_string());
        self.backup_state = Some(r);
    }

    pub fn backup_ok(&self) -> bool {
        matches!(self.backup_state, Some(Ok(_)))
    }

    /// Menu text for the backup line.
    pub fn backup_text(&self) -> String {
        let Some(bk) = &self.backup else {
            return "Respaldo: no configurado".into();
        };
        let dir = bk.parent().map(|p| p.display().to_string()).unwrap_or_default();
        match &self.backup_state {
            Some(Ok(t)) => format!("Respaldo en {dir} ({t})"),
            Some(Err(e)) => format!("Respaldo pendiente: {e}"),
            None => format!("Respaldo en {dir}"),
        }
    }

    /// Records a reading if it differs from the last one. `fecha` is local time text.
    /// Returns true when a row was written.
    pub fn record(&mut self, r: &Reading, fecha: &str) -> bool {
        if let Some(last) = self.rows.last() {
            if last.pct == r.percent && last.st == r.charge {
                return false;
            }
        }
        let row = Row { t: now(), pct: r.percent, st: r.charge, uso: Some(self.usage) };
        self.rows.push(row);
        let Some(path) = &self.path else { return false };
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let new = !path.exists();
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            if new {
                let _ = writeln!(f, "unix,fecha,porcentaje,estado,via,uso_s");
            }
            let _ = writeln!(f, "{},{},{},{},{},{:.0}", row.t, fecha, row.pct, state_str(row.st), r.link, self.usage);
        }
        true
    }

    /// Discharges: runs of on-battery rows between charges. `done` = a charge followed.
    pub fn discharges(&self) -> Vec<Session> {
        self.runs(|r| r.st == Charge::Discharging)
            .into_iter()
            .map(|(a, b)| Session {
                t0: self.rows[a].t,
                t1: self.rows[b].t,
                p0: self.rows[a].pct,
                p1: self.rows[b].pct,
                done: b + 1 < self.rows.len(),
            })
            .collect()
    }

    /// Charges: runs of plugged-in rows. Starts at the level before plugging in and
    /// ends when it reports full (or when it was unplugged).
    pub fn charges(&self) -> Vec<Session> {
        self.runs(|r| r.st != Charge::Discharging)
            .into_iter()
            .map(|(a, b)| {
                let run = &self.rows[a..=b];
                let full = run.iter().find(|r| r.st == Charge::Full);
                let t1 = match full {
                    Some(f) => f.t,
                    None if b + 1 < self.rows.len() => self.rows[b + 1].t,
                    None => self.rows[b].t,
                };
                Session {
                    t0: self.rows[a].t,
                    t1,
                    p0: if a > 0 { self.rows[a - 1].pct } else { self.rows[a].pct },
                    p1: run.iter().map(|r| r.pct).max().unwrap_or(0),
                    done: full.is_some() || b + 1 < self.rows.len(),
                }
            })
            .collect()
    }

    fn runs(&self, keep: impl Fn(&Row) -> bool) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.rows.len() {
            if !keep(&self.rows[i]) {
                i += 1;
                continue;
            }
            let start = i;
            while i + 1 < self.rows.len() && keep(&self.rows[i + 1]) {
                i += 1;
            }
            out.push((start, i));
            i += 1;
        }
        out
    }

    /// Battery health: life per charge of the latest finished discharges vs the
    /// first ones (median of 3 each, discharges of at least 15 %). Uses hours of use
    /// when every compared discharge has them, clock hours otherwise.
    pub fn health(&self) -> Health {
        let mut by_use = Vec::new();
        let mut by_clock = Vec::new();
        for (a, b) in self.runs(|r| r.st == Charge::Discharging) {
            let (ra, rb) = (self.rows[a], self.rows[b]);
            if b + 1 >= self.rows.len() || ra.pct < rb.pct + 15 || rb.t <= ra.t {
                continue;
            }
            let drop = (ra.pct - rb.pct) as f64;
            by_clock.push(100.0 * (rb.t - ra.t) as f64 / 3600.0 / drop);
            if let (Some(ua), Some(ub)) = (ra.uso, rb.uso) {
                if ub > ua {
                    by_use.push(100.0 * (ub - ua) / 3600.0 / drop);
                }
            }
        }
        let (lives, of_use) = if by_use.len() >= HEALTH_MIN { (by_use, true) } else { (by_clock, false) };
        if lives.len() < HEALTH_MIN {
            return Health::Measuring { have: lives.len(), need: HEALTH_MIN };
        }
        let median = |v: &[f64]| {
            let mut v = v.to_vec();
            v.sort_by(|a, b| a.total_cmp(b));
            v[v.len() / 2]
        };
        let first = median(&lives[..3]);
        let last = median(&lives[lives.len() - 3..]);
        Health::Known { change_pct: (last / first - 1.0) * 100.0, first_h: first, last_h: last, of_use }
    }

    /// Rows as JSON `[[unix, pct, state], ...]` (state 0 battery, 1 charging, 2 full).
    pub fn rows_json(&self) -> String {
        let items: Vec<String> = self
            .rows
            .iter()
            .map(|r| {
                let s = match r.st {
                    Charge::Discharging => 0,
                    Charge::Charging => 1,
                    Charge::Full => 2,
                };
                format!("[{},{},{s}]", r.t, r.pct)
            })
            .collect();
        format!("[{}]", items.join(","))
    }

    /// Lowest level logged since the last charge (None if charging or no data).
    pub fn min_since_charge(&self) -> Option<u8> {
        if self.rows.last()?.st != Charge::Discharging {
            return None;
        }
        self.rows.iter().rev().take_while(|r| r.st == Charge::Discharging).map(|r| r.pct).min()
    }

    /// Use-based drain rate (% per second of use): least-squares fit since the last
    /// charge, or the last finished discharge when the current one is too short.
    fn use_rate(&self, current: Option<&Reading>) -> Option<f64> {
        let mut pts: Vec<(f64, f64)> = self
            .rows
            .iter()
            .rev()
            .take_while(|r| r.st == Charge::Discharging)
            .filter_map(|r| r.uso.map(|u| (u, r.pct as f64)))
            .collect();
        pts.reverse();
        if let Some(c) = current.filter(|c| c.charge == Charge::Discharging) {
            pts.push((self.usage, c.percent as f64));
        }
        if let Some(rate) = fit(&pts) {
            return Some(rate);
        }
        // Previous finished discharge with use data.
        for (a, b) in self.runs(|r| r.st == Charge::Discharging).into_iter().rev() {
            if b + 1 >= self.rows.len() {
                continue;
            }
            let pts: Vec<(f64, f64)> =
                self.rows[a..=b].iter().filter_map(|r| r.uso.map(|u| (u, r.pct as f64))).collect();
            if let Some(rate) = fit(&pts) {
                return Some(rate);
            }
        }
        None
    }

    pub fn stats(&self, current: Option<&Reading>) -> Stats {
        let now = now();
        let mut drop = 0u32;
        let mut secs = 0u64;
        let mut total_drop = 0u32;
        for w in self.rows.windows(2) {
            let (a, b) = (w[0], w[1]);
            if a.st == Charge::Discharging && b.st == Charge::Discharging && b.pct <= a.pct {
                total_drop += (a.pct - b.pct) as u32;
                if now.saturating_sub(b.t) < RATE_WINDOW_S {
                    drop += (a.pct - b.pct) as u32;
                    secs += b.t.saturating_sub(a.t);
                }
            }
        }
        if let (Some(last), Some(cur)) = (self.rows.last(), current) {
            if last.st == Charge::Discharging && cur.charge == Charge::Discharging {
                secs += now.saturating_sub(last.t);
            }
        }
        let on_battery = current.filter(|c| c.charge == Charge::Discharging);
        let rate = (drop >= MIN_DROP && secs > 0).then(|| drop as f64 / (secs as f64 / 3600.0));

        let use_rate = self.use_rate(current);
        let use_left_h = match (use_rate, on_battery) {
            (Some(r), Some(c)) => Some(c.percent as f64 / r / 3600.0),
            _ => None,
        };
        // Use per clock time over the last week: turns hours of use into calendar time.
        let ratio = self
            .rows
            .iter()
            .find(|r| r.uso.is_some() && now.saturating_sub(r.t) <= RATIO_WINDOW_S)
            .and_then(|r| {
                let clock = now.saturating_sub(r.t) as f64;
                (clock >= 86400.0).then(|| (self.usage - r.uso.unwrap()) / clock)
            })
            .filter(|&x| x > 0.005);

        Stats {
            use_full_h: use_rate.map(|r| 100.0 / r / 3600.0),
            use_left_h,
            cal_left_h: use_left_h.zip(ratio).map(|(h, k)| h / k),
            full_h: rate.map(|r| 100.0 / r),
            left_h: rate.zip(on_battery).map(|(r, c)| c.percent as f64 / r),
            cycles: total_drop as f64 / 100.0,
            since_s: self.rows.first().map(|r| now.saturating_sub(r.t)),
            last_charge: self.last_charge(now),
        }
    }

    fn last_charge(&self, now: u64) -> Option<LastCharge> {
        let end_idx = self.rows.iter().rposition(|r| r.st != Charge::Discharging)?;
        let mut start_idx = end_idx;
        while start_idx > 0 && self.rows[start_idx - 1].st != Charge::Discharging {
            start_idx -= 1;
        }
        let run = &self.rows[start_idx..=end_idx];
        let start = run[0];
        let from = if start_idx > 0 { self.rows[start_idx - 1].pct } else { start.pct };
        let ongoing = end_idx == self.rows.len() - 1;
        let end_t = match run.iter().find(|r| r.st == Charge::Full) {
            Some(full) => full.t,
            None if ongoing => now,
            None => self.rows[end_idx + 1].t,
        };
        Some(LastCharge {
            ago_s: now.saturating_sub(start.t),
            from,
            to: run.iter().map(|r| r.pct).max().unwrap_or(start.pct),
            dur_s: end_t.saturating_sub(start.t),
            ongoing: ongoing && !run.iter().any(|r| r.st == Charge::Full),
        })
    }
}

/// Drain rate (% per second of use) by least squares over (use_s, level) points,
/// when they span enough use and drop.
fn fit(pts: &[(f64, f64)]) -> Option<f64> {
    if pts.len() < 2 {
        return None;
    }
    let span = pts.last()?.0 - pts[0].0;
    let fall = pts[0].1 - pts.iter().map(|p| p.1).fold(f64::MAX, f64::min);
    if span < MIN_USE_S || fall < MIN_USE_DROP {
        return None;
    }
    let n = pts.len() as f64;
    let (sx, sy) = pts.iter().fold((0.0, 0.0), |(a, b), p| (a + p.0, b + p.1));
    let (mx, my) = (sx / n, sy / n);
    let (num, den) = pts.iter().fold((0.0, 0.0), |(a, b), p| (a + (p.0 - mx) * (p.1 - my), b + (p.0 - mx).powi(2)));
    let slope = num / den;
    (den > 0.0 && slope < 0.0).then_some(-slope)
}

fn size(p: &PathBuf) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

fn parse(path: &PathBuf) -> Vec<Row> {
    let mut rows = Vec::new();
    let Ok(text) = std::fs::read_to_string(path) else { return rows };
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 4 {
            continue;
        }
        let (Ok(t), Ok(pct)) = (f[0].parse(), f[2].parse()) else { continue };
        let st = match f[3] {
            "cargando" => Charge::Charging,
            "completa" => Charge::Full,
            _ => Charge::Discharging,
        };
        let uso = f.get(5).and_then(|u| u.trim().parse().ok());
        rows.push(Row { t, pct, st, uso });
    }
    rows
}

/// "3 días", "5 h 20 min", "45 min".
pub fn dur(secs: u64) -> String {
    let m = secs / 60;
    let (d, h, m) = (m / 1440, (m / 60) % 24, m % 60);
    if d >= 2 {
        format!("{d} días")
    } else if d == 1 {
        format!("1 día {h} h")
    } else if h > 0 {
        format!("{h} h {m} min")
    } else {
        format!("{m} min")
    }
}

pub fn dur_h(hours: f64) -> String {
    dur((hours * 3600.0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_needs_span_and_drop() {
        assert_eq!(fit(&[(0.0, 100.0), (600.0, 99.0)]), None);
        // 1 % per 3600 s of use.
        let pts: Vec<(f64, f64)> = (0..10).map(|i| (i as f64 * 3600.0, 100.0 - i as f64)).collect();
        let r = fit(&pts).unwrap();
        assert!((r * 3600.0 - 1.0).abs() < 1e-9);
    }
}

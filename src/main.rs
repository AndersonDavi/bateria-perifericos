#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod corsair;
mod device;
mod diag;
mod gamepad;
mod graph;
mod hidpp;
mod icon;
mod notify;
mod stats;

use device::{Charge, Kind, Reading};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const APP: &str = "BateriaPerifericos";
/// Name used by the single-mouse versions (autostart value, data folder).
const OLD_APP: &str = "MouseBateria";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Poll interval while something charges / otherwise.
const POLL_CHARGING: Duration = Duration::from_secs(30);
const POLL_BATTERY: Duration = Duration::from_secs(90);
/// A device that stops answering (asleep, off) keeps its dimmed icon with the last
/// value this long. Mice sleep whenever idle, so this has to be long.
const KEEP_MISSING: Duration = Duration::from_secs(24 * 3600);

enum Cmd {
    Refresh,
    Diag,
}

enum Msg {
    Round(Vec<Reading>),
    Diag(Option<PathBuf>),
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--probe") => return probe(),
        Some("--preview") => return preview(args.get(1).map(String::as_str).unwrap_or(".")),
        Some("--render") => {
            // --render <file.svg> <size>: writes <file>.<size>.rgba (icon design work)
            let svg = std::fs::read_to_string(&args[1]).expect("svg");
            let size: u32 = args[2].parse().expect("size");
            let (rgba, _) = icon::render(&svg, size);
            std::fs::write(format!("{}.{size}.rgba", args[1].trim_end_matches(".svg")), rgba).expect("rgba");
            return;
        }
        Some("--grafico") => {
            // --grafico <historial.csv>: writes grafico.html next to that CSV (no tray)
            let history = stats::History::from_file(PathBuf::from(&args[1]));
            println!("{}", health_text(&history.health()));
            println!("{:?}", write_graph("Dispositivo", None, "", false, &history));
            return;
        }
        _ => {}
    }

    unsafe {
        CreateMutexW(std::ptr::null(), 0, w(&format!("Local\\{APP}")).as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return; // already running
        }
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    migrate_legacy();
    watch_devices();
    let mut settings = Settings::load();

    // Worker thread: asks every reader, then waits (or refreshes on demand).
    let (msg_tx, msg_rx) = mpsc::channel::<Msg>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    std::thread::spawn(move || {
        let mut api = None;
        let mut cmd = None;
        loop {
            if api.is_none() {
                api = hidapi::HidApi::new().ok();
            }
            let mut log = Vec::new();
            let readings = match api.as_mut() {
                Some(a) => device::read_all(a, &mut log, matches!(cmd, Some(Cmd::Diag))),
                None => Vec::new(),
            };
            if matches!(cmd, Some(Cmd::Diag)) {
                let path = diag::write(api.as_ref(), &readings, &log);
                if msg_tx.send(Msg::Diag(path)).is_err() {
                    return;
                }
            }
            let wait = if readings.iter().any(|r| r.charge != Charge::Discharging) { POLL_CHARGING } else { POLL_BATTERY };
            if msg_tx.send(Msg::Round(readings)).is_err() {
                return;
            }
            cmd = match cmd_rx.recv_timeout(wait) {
                Ok(c) => {
                    let mut c = c;
                    while let Ok(more) = cmd_rx.try_recv() {
                        if matches!(more, Cmd::Diag) {
                            c = more;
                        }
                    }
                    Some(c)
                }
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            };
        }
    });

    let mut devs: Vec<Dev> = Vec::new();
    let mut idle: Option<Idle> = Some(Idle::new(&settings));

    unsafe {
        SetTimer(std::ptr::null_mut(), 0, 400, None);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if !(msg.message == WM_TIMER && msg.hwnd.is_null()) {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
                continue;
            }
            // A USB device came or went (cable, dongle): read once it has settled.
            let changed = DEVICE_CHANGED_AT.load(Ordering::Relaxed);
            if changed != 0 && now_ms().saturating_sub(changed) >= 1500 {
                DEVICE_CHANGED_AT.store(0, Ordering::Relaxed);
                let _ = cmd_tx.send(Cmd::Refresh);
            }

            while let Ok(m) = msg_rx.try_recv() {
                match m {
                    Msg::Diag(p) => {
                        if let Some(p) = p {
                            open_path(&p);
                        }
                    }
                    Msg::Round(readings) => {
                        let seen: Vec<String> = readings.iter().map(|r| r.key.clone()).collect();
                        for r in readings {
                            let pos = devs.iter().position(|d| d.last.key == r.key);
                            let i = match pos {
                                Some(i) => i,
                                None => {
                                    let adopt = r.kind == Kind::Mouse && !devs.iter().any(|d| d.last.kind == Kind::Mouse);
                                    match Dev::new(r.clone(), adopt, &settings) {
                                        Some(d) => {
                                            devs.push(d);
                                            devs.len() - 1
                                        }
                                        None => continue,
                                    }
                                }
                            };
                            if let Some(n) = devs[i].apply(r) {
                                devs[i].notify(n, false);
                            }
                            devs[i].show(&settings);
                        }
                        for d in devs.iter_mut().filter(|d| !seen.contains(&d.last.key)) {
                            d.history.asleep();
                            if d.missing_since.is_none() {
                                d.missing_since = Some(Instant::now());
                                d.show(&settings);
                            }
                        }
                        devs.retain(|d| d.missing_since.map_or(true, |t| t.elapsed() < KEEP_MISSING));
                        match (devs.is_empty(), idle.is_some()) {
                            (true, false) => idle = Some(Idle::new(&settings)),
                            (false, true) => idle = None,
                            _ => {}
                        }
                    }
                }
            }

            while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                    let _ = cmd_tx.send(Cmd::Refresh);
                }
            }

            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                let id = ev.id;
                // Options shared by every menu.
                let globals: Vec<&Globals> = devs.iter().map(|d| &d.menu.g).chain(idle.iter().map(|i| &i.g)).collect();
                if let Some(g) = globals.iter().find(|g| g.has(&id)) {
                    if id == *g.quit.id() {
                        PostQuitMessage(0);
                    } else if id == *g.diag.id() {
                        let _ = cmd_tx.send(Cmd::Diag);
                    } else if id == *g.number.id() {
                        settings.number = g.number.is_checked();
                    } else if id == *g.unplug.id() {
                        settings.unplug90 = g.unplug.is_checked();
                    } else if id == *g.autostart.id() {
                        set_autostart(g.autostart.is_checked());
                    }
                    settings.save();
                    let auto = autostart_enabled();
                    for g in &globals {
                        g.number.set_checked(settings.number);
                        g.unplug.set_checked(settings.unplug90);
                        g.autostart.set_checked(auto);
                    }
                    for d in devs.iter_mut() {
                        d.alerts.unplug_reminder = settings.unplug90;
                        d.show(&settings);
                    }
                    continue;
                }
                let Some(d) = devs.iter_mut().find(|d| d.menu.has(&id)) else { continue };
                if id == *d.menu.refresh.id() {
                    let _ = cmd_tx.send(Cmd::Refresh);
                } else if id == *d.menu.test.id() {
                    d.notify(notify::Notice::Low(d.last.percent), true);
                } else if id == *d.menu.graph.id() {
                    let stale = d.missing_since.is_some();
                    if let Some(p) = write_graph(&d.last.name, Some(&d.last), &d.last_time, stale, &d.history) {
                        open_path(&p);
                    }
                } else if id == *d.menu.open_log.id() {
                    if let Some(p) = d.history.path().filter(|p| p.exists()) {
                        open_path(p);
                    }
                }
            }
        }
    }
}

/// Items present in every menu.
struct Globals {
    number: CheckMenuItem,
    unplug: CheckMenuItem,
    autostart: CheckMenuItem,
    diag: MenuItem,
    quit: MenuItem,
}

impl Globals {
    fn new(s: &Settings) -> Self {
        Globals {
            number: CheckMenuItem::new("Mostrar número en el ícono", true, s.number, None),
            unplug: CheckMenuItem::new(format!("Avisar al {} % para desconectar", notify::UNPLUG_AT), true, s.unplug90, None),
            autostart: CheckMenuItem::new("Iniciar con Windows", true, autostart_enabled(), None),
            diag: MenuItem::new("Generar diagnóstico", true, None),
            quit: MenuItem::new("Salir", true, None),
        }
    }

    fn has(&self, id: &MenuId) -> bool {
        [self.number.id(), self.unplug.id(), self.autostart.id(), self.diag.id(), self.quit.id()].contains(&id)
    }

    fn append(&self, m: &Menu) {
        let _ = m.append_items(&[
            &PredefinedMenuItem::separator(),
            &self.number,
            &self.unplug,
            &self.autostart,
            &self.diag,
            &PredefinedMenuItem::separator(),
            &self.quit,
        ]);
    }
}

struct DevMenu {
    status: MenuItem,
    life: MenuItem,
    charge: MenuItem,
    cycles: MenuItem,
    health: MenuItem,
    backup: MenuItem,
    refresh: MenuItem,
    graph: MenuItem,
    open_log: MenuItem,
    test: MenuItem,
    g: Globals,
}

impl DevMenu {
    fn has(&self, id: &MenuId) -> bool {
        [self.refresh.id(), self.graph.id(), self.open_log.id(), self.test.id()].contains(&id)
    }
}

/// Icon shown while no device has been found.
struct Idle {
    _tray: Option<TrayIcon>,
    g: Globals,
}

impl Idle {
    fn new(s: &Settings) -> Self {
        let g = Globals::new(s);
        let menu = Menu::new();
        let _ = menu.append(&MenuItem::new("Buscando dispositivos compatibles…", false, None));
        g.append(&menu);
        let look = icon::Look { kind: Kind::Mouse, percent: None, charge: Charge::Discharging, stale: true, number: false };
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("Batería de periféricos: buscando dispositivos…")
            .with_icon(tray_icon_of(&look))
            .build()
            .ok();
        Idle { _tray: tray, g }
    }
}

/// One tray icon per device.
struct Dev {
    last: Reading,
    last_time: String,
    missing_since: Option<Instant>,
    history: stats::History,
    alerts: notify::Alerts,
    tray: TrayIcon,
    menu: DevMenu,
}

impl Dev {
    fn new(r: Reading, adopt_legacy: bool, s: &Settings) -> Option<Self> {
        let menu = DevMenu {
            status: MenuItem::new(&r.name, false, None),
            life: MenuItem::new("Duración: midiendo…", false, None),
            charge: MenuItem::new("Última carga: sin datos todavía", false, None),
            cycles: MenuItem::new("Ciclos: sin datos todavía", false, None),
            health: MenuItem::new("Salud: midiendo…", false, None),
            backup: MenuItem::new("Respaldo: no configurado", false, None),
            refresh: MenuItem::new("Actualizar ahora", true, None),
            graph: MenuItem::new("Ver gráfico", true, None),
            open_log: MenuItem::new("Abrir historial (CSV)", true, None),
            test: MenuItem::new("Probar aviso", true, None),
            g: Globals::new(s),
        };
        let m = Menu::new();
        let _ = m.append_items(&[
            &menu.status,
            &menu.life,
            &menu.charge,
            &menu.cycles,
            &menu.health,
            &menu.backup,
            &PredefinedMenuItem::separator(),
            &menu.refresh,
            &menu.graph,
            &menu.open_log,
            &menu.test,
        ]);
        menu.g.append(&m);
        let look = icon::Look { kind: r.kind, percent: None, charge: Charge::Discharging, stale: true, number: false };
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(m))
            .with_menu_on_left_click(false)
            .with_tooltip(&r.name)
            .with_icon(tray_icon_of(&look))
            .build()
            .ok()?;
        Some(Dev {
            history: stats::History::load(&r.key, adopt_legacy),
            alerts: notify::Alerts::new(s.unplug90),
            last: r,
            last_time: String::new(),
            missing_since: None,
            tray,
            menu,
        })
    }

    fn apply(&mut self, r: Reading) -> Option<notify::Notice> {
        let now = now_local();
        self.history.tick(r.charge);
        let notice = self.alerts.check(r.percent, r.charge, self.history.min_since_charge());
        let wrote = self.history.record(&r, &now);
        if wrote || !self.history.backup_ok() {
            self.history.sync(&now[11..]);
        }
        self.last = r;
        self.last_time = now[11..].to_string();
        self.missing_since = None;
        notice
    }

    fn look(&self, s: &Settings) -> icon::Look {
        icon::Look {
            kind: self.last.kind,
            percent: Some(self.last.percent),
            charge: self.last.charge,
            stale: self.missing_since.is_some(),
            number: s.number,
        }
    }

    fn show(&self, s: &Settings) {
        let r = &self.last;
        let stale = self.missing_since.is_some();
        let st = self.history.stats(if stale { None } else { Some(r) });
        let left = st.left_text();
        let pct = format!("{}{} %", if r.approx { "~" } else { "" }, r.percent);
        let tip = if stale {
            format!("{}\n{pct} (último dato {})\nNo responde: apagado o dormido", r.name, self.last_time)
        } else {
            format!("{}\n{pct} · {}{left}\nActualizado {} ({})", r.name, charge_text(r.charge), self.last_time, r.link)
        };
        let _ = self.tray.set_tooltip(Some(tip.chars().take(127).collect::<String>()));
        let _ = self.tray.set_icon(Some(tray_icon_of(&self.look(s))));
        let m = &self.menu;
        m.status.set_text(if stale {
            format!("{}: {pct} (sin respuesta)", r.name)
        } else {
            format!("{}: {pct} · {} ({})", r.name, charge_text(r.charge), r.link)
        });
        m.life.set_text(match st.life_text() {
            Some(t) => format!("Dura {t}{left}"),
            None => "Duración: midiendo (necesito ver bajar unos %)".into(),
        });
        m.charge.set_text(match st.last_charge {
            Some(c) if c.ongoing => format!("Cargando hace {} (desde {} %)", stats::dur(c.dur_s), c.from),
            Some(c) => format!("Última carga: {} % → {} % en {} (hace {})", c.from, c.to, stats::dur(c.dur_s), stats::dur(c.ago_s)),
            None => "Última carga: sin datos todavía".into(),
        });
        m.cycles.set_text(match st.since_s {
            Some(sec) => format!("Ciclos: {:.1} en {} de registro", st.cycles, stats::dur(sec)).replace('.', ","),
            None => "Ciclos: sin datos todavía".into(),
        });
        m.health.set_text(health_text(&self.history.health()));
        m.backup.set_text(self.history.backup_text());
    }

    fn notify(&self, n: notify::Notice, test: bool) {
        let name = &self.last.name;
        let st = self.history.stats(Some(&self.last));
        let (title, body, warn) = match n {
            notify::Notice::Low(p) => {
                let left = match (st.use_left_h, st.cal_left_h, st.left_h) {
                    (Some(u), Some(c), _) if c > u * 1.3 => format!("Quedan ~{} de uso (≈ {}).", stats::dur_h(u), stats::dur_h(c)),
                    (Some(u), _, _) => format!("Quedan ~{} de uso.", stats::dur_h(u)),
                    (None, _, Some(h)) => format!("Quedan ~{} de batería.", stats::dur_h(h)),
                    _ => "Todavía estoy midiendo cuánto dura una carga.".into(),
                };
                let tail = if p <= 10 { " Conéctalo pronto." } else { "" };
                (format!("{name}: {p} % de batería"), format!("{left}{tail}"), p <= 15)
            }
            notify::Notice::Unplug(p) => (
                format!("{name}: {p} % cargado"),
                "Ya puedes desconectarlo: dejarlo siempre hasta el 100 % desgasta más la batería.".into(),
                false,
            ),
            notify::Notice::Full => {
                let took = st
                    .last_charge
                    .map(|c| format!(" Cargó de {} % a 100 % en {}.", c.from, stats::dur(c.dur_s)))
                    .unwrap_or_default();
                (format!("{name}: carga completa"), format!("Ya puedes desconectarlo.{took}"), false)
            }
        };
        let body = if test { format!("{body} (prueba)") } else { body };
        let size = unsafe { GetSystemMetrics(SM_CXICON) }.clamp(32, 128) as u32;
        let mut look = self.look(&Settings { number: false, unplug90: false });
        look.stale = false;
        let (img, side) = icon::render(&icon::svg(&look), size);
        let _ = notify::show(self.tray.window_handle(), &title, &body, warn, Some((&img, side)));
    }
}

fn tray_icon_of(look: &icon::Look) -> Icon {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as u32;
    let (rgba, s) = icon::render(&icon::svg(look), size);
    Icon::from_rgba(rgba, s, s).expect("ícono")
}

/// Writes grafico.html for one device; returns its path.
fn write_graph(name: &str, last: Option<&Reading>, last_time: &str, stale: bool, h: &stats::History) -> Option<PathBuf> {
    let st = h.stats(if stale { None } else { last });
    let now_text = match last {
        Some(r) => format!("{} % · {}{} · actualizado {last_time}", r.percent, charge_text(r.charge), st.left_text()),
        None => "Historial".into(),
    };
    let summary = graph::Summary {
        name,
        now_text,
        life: st.life_text().unwrap_or_else(|| "Midiendo (necesito ver bajar unos %)".into()),
        cycles: match st.since_s {
            Some(s) => format!("{:.1} en {} de registro", st.cycles, stats::dur(s)).replace('.', ","),
            None => "Sin datos todavía".into(),
        },
        health: health_text(&h.health()).trim_start_matches("Salud: ").to_string(),
    };
    graph::write(h, &summary)
}

fn health_text(h: &stats::Health) -> String {
    match *h {
        stats::Health::Measuring { have, need } => format!("Salud: midiendo ({have} de {need} descargas completas)"),
        stats::Health::Known { change_pct, first_h, last_h, of_use } => {
            let how = if change_pct.abs() < 5.0 {
                "igual que al inicio".to_string()
            } else if change_pct < 0.0 {
                format!("dura {:.0} % menos que al inicio", -change_pct)
            } else {
                format!("dura {:.0} % más que al inicio", change_pct)
            };
            let span = |h: f64| {
                if h >= 48.0 {
                    format!("{:.1} días", h / 24.0).replace('.', ",")
                } else {
                    format!("{h:.0} h")
                }
            };
            let unit = if of_use { "de uso por carga" } else { "por carga" };
            format!("Salud: {how} ({} → {} {unit})", span(first_h), span(last_h))
        }
    }
}

fn charge_text(c: Charge) -> &'static str {
    match c {
        Charge::Discharging => "Con batería",
        Charge::Charging => "Cargando",
        Charge::Full => "Carga completa",
    }
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Local time as "YYYY-MM-DD HH:MM".
fn now_local() -> String {
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    format!("{:04}-{:02}-{:02} {:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute)
}

fn open_path(p: &std::path::Path) {
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            w("open").as_ptr(),
            w(&p.display().to_string()).as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// Menu options saved in %LOCALAPPDATA%\BateriaPerifericos\ajustes.txt (`clave=1/0`).
struct Settings {
    number: bool,
    unplug90: bool,
}

impl Settings {
    fn path() -> Option<PathBuf> {
        stats::data_dir().map(|d| d.join("ajustes.txt"))
    }

    fn load() -> Self {
        let mut s = Settings { number: false, unplug90: true };
        let text = Self::path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                let on = v.trim() == "1";
                match k.trim() {
                    "numero" => s.number = on,
                    "aviso90" => s.unplug90 = on,
                    _ => {}
                }
            }
        }
        s
    }

    fn save(&self) {
        if let Some(p) = Self::path() {
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            let _ = std::fs::write(p, format!("numero={}\naviso90={}\n", self.number as u8, self.unplug90 as u8));
        }
    }
}

/// From the single-mouse versions: data folder %LOCALAPPDATA%\MouseBateria (its
/// history becomes historial-legado.csv, adopted by the first mouse found) and
/// the autostart entry.
fn migrate_legacy() {
    let (Some(new), Some(old)) = (
        stats::data_dir(),
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join(OLD_APP)),
    ) else {
        return;
    };
    let has_history = std::fs::read_dir(&new)
        .map(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().starts_with("historial")))
        .unwrap_or(false);
    if old.exists() && !has_history {
        let _ = std::fs::create_dir_all(&new);
        for f in ["respaldo.txt", "ajustes.txt"] {
            if !new.join(f).exists() {
                let _ = std::fs::copy(old.join(f), new.join(f));
            }
        }
        let _ = std::fs::copy(old.join("historial.csv"), new.join("historial-legado.csv"));
    }
    if reg_value_exists(OLD_APP) {
        unsafe {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, w(RUN_KEY).as_ptr(), w(OLD_APP).as_ptr());
        }
        set_autostart(true);
    }
}

fn reg_value_exists(name: &str) -> bool {
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w(RUN_KEY).as_ptr(),
            w(name).as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    r == 0
}

fn autostart_enabled() -> bool {
    reg_value_exists(APP)
}

fn set_autostart(on: bool) {
    unsafe {
        if on {
            let exe = std::env::current_exe().unwrap_or_default();
            let cmd = w(&format!("\"{}\"", exe.display()));
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                w(RUN_KEY).as_ptr(),
                w(APP).as_ptr(),
                REG_SZ,
                cmd.as_ptr() as *const _,
                (cmd.len() * 2) as u32,
            );
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, w(RUN_KEY).as_ptr(), w(APP).as_ptr());
        }
    }
}

/// Time (ms) of the last USB HID arrival/removal; 0 = none pending.
static DEVICE_CHANGED_AT: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

unsafe extern "system" fn device_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_DEVICECHANGE && (wp == DBT_DEVICEARRIVAL as usize || wp == DBT_DEVICEREMOVECOMPLETE as usize) {
        DEVICE_CHANGED_AT.store(now_ms(), Ordering::Relaxed);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// Hidden message-only window subscribed to HID device arrivals/removals, so
/// plugging a cable or a dongle refreshes right away instead of at the next poll.
fn watch_devices() {
    unsafe {
        let class = w("BateriaPerifericosDispositivos");
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(device_proc);
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            std::ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }
        let mut filter: DEV_BROADCAST_DEVICEINTERFACE_W = std::mem::zeroed();
        filter.dbcc_size = std::mem::size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32;
        filter.dbcc_devicetype = DBT_DEVTYP_DEVICEINTERFACE;
        // GUID_DEVINTERFACE_HID
        filter.dbcc_classguid = windows_sys::core::GUID::from_u128(0x4d1e55b2_f16f_11cf_88cb_001111000030);
        RegisterDeviceNotificationW(hwnd, &filter as *const _ as *const _, DEVICE_NOTIFY_WINDOW_HANDLE);
    }
}

/// `--probe`: one reading of every device, printed to the console.
fn probe() {
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
    }
    let mut api = hidapi::HidApi::new().expect("hidapi");
    let mut log = Vec::new();
    let lines = hidpp::probe(&mut api, &mut log);
    if std::env::args().any(|a| a == "-v") {
        for l in &log {
            println!("{l}");
        }
    }
    if lines.is_empty() {
        println!("No encontré dispositivos compatibles.");
    }
    for l in lines {
        println!("{l}");
    }
}

/// `--preview <dir>`: writes icon SVGs (and 16/24/32 px RGBA) for several states.
fn preview(dir: &str) {
    let states = [
        ("100-cargando", Some(100), Charge::Charging, false),
        ("75", Some(75), Charge::Discharging, false),
        ("35", Some(35), Charge::Discharging, false),
        ("10", Some(10), Charge::Discharging, false),
        ("60-dormido", Some(60), Charge::Discharging, true),
    ];
    let kinds = [("mouse", Kind::Mouse), ("audifono", Kind::Headset), ("mando", Kind::Controller), ("teclado", Kind::Keyboard)];
    for (kname, kind) in kinds {
        for number in [false, true] {
            for (name, percent, charge, stale) in states {
                let name = format!("{}{kname}-{name}", if number { "num-" } else { "" });
                let svg = icon::svg(&icon::Look { kind, percent, charge, stale, number });
                for size in [16u32, 24, 32] {
                    let (rgba, _) = icon::render(&svg, size);
                    let _ = std::fs::write(format!("{dir}/{name}.{size}.rgba"), rgba);
                }
                std::fs::write(format!("{dir}/{name}.svg"), svg).expect("escribir svg");
            }
        }
    }
}

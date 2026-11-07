//! "Generar diagnóstico": a text report to attach when a device isn't detected or
//! reads wrong. Lists every HID collection (ids and usage pages, no serial
//! numbers), the raw exchanges of the last round and the vendor apps running
//! (they can hold a device and block the reading).

use crate::device::Reading;
use hidapi::HidApi;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;

/// Vendor apps known to keep their devices busy.
const VENDOR_APPS: &[&str] = &["icue", "lghub", "gamesir", "steelseries", "razer", "synapse", "signalrgb", "aula", "hyperx", "ngenuity"];

pub fn write(api: Option<&HidApi>, readings: &[Reading], log: &[String]) -> Option<PathBuf> {
    let mut out = String::new();
    out += &format!("Batería de periféricos {} - diagnóstico\n", env!("CARGO_PKG_VERSION"));
    out += &format!("Fecha (unix): {}\n\n", crate::stats::now());

    out += "== Dispositivos leídos ==\n";
    if readings.is_empty() {
        out += "(ninguno)\n";
    }
    for r in readings {
        out += &format!(
            "{} [{}] {:?}: {}{} % {:?} vía {}\n",
            r.name, r.key, r.kind, if r.approx { "~" } else { "" }, r.percent, r.charge, r.link
        );
    }

    out += "\n== Apps del fabricante abiertas ==\n";
    let running = running_vendor_apps();
    out += if running.is_empty() { "(ninguna)\n".to_string() } else { running.join("\n") + "\n" }.as_str();

    out += "\n== Colecciones HID ==\n";
    if let Some(api) = api {
        let mut lines: Vec<String> = api
            .device_list()
            .map(|d| {
                format!(
                    "{:04x}:{:04x} if={} up={:04x} u={:04x} {:?} / {:?}",
                    d.vendor_id(), d.product_id(), d.interface_number(), d.usage_page(), d.usage(),
                    d.manufacturer_string().unwrap_or(""), d.product_string().unwrap_or("")
                )
            })
            .collect();
        lines.sort();
        lines.dedup();
        out += &(lines.join("\n") + "\n");
    }

    out += "\n== Intercambios de la última lectura ==\n";
    out += &(log.join("\n") + "\n");

    let path = crate::stats::data_dir()?.join("diagnostico.txt");
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::write(&path, out).ok()?;
    Some(path)
}

fn running_vendor_apps() -> Vec<String> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(o) = std::process::Command::new("tasklist")
        .args(["/fo", "csv", "/nh"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&o.stdout).to_lowercase();
    let mut found: Vec<String> = text
        .lines()
        .filter_map(|l| l.split(',').next().map(|n| n.trim_matches('"').to_string()))
        .filter(|n| VENDOR_APPS.iter().any(|v| n.contains(v)))
        .collect();
    found.sort();
    found.dedup();
    found
}

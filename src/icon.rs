//! Tray icons: a silhouette of the device (mouse, headset, controller, keyboard)
//! that fills up with the charge level, or the % as digits. Built as an SVG
//! string on every change and rasterized with resvg.
//!
//! Outline and fill share the level color, the empty part is dark and a thin
//! dark rim keeps it readable on light, dark or tinted (mica) taskbars.

use crate::device::{Charge, Kind};
use resvg::{tiny_skia, usvg};

pub struct Look {
    pub kind: Kind,
    /// None = no reading yet.
    pub percent: Option<u8>,
    pub charge: Charge,
    /// Last value is stale (device asleep, off or out of range).
    pub stale: bool,
    /// Show the percentage as digits instead of the silhouette.
    pub number: bool,
}

/// Colored outline width (32-unit box): thin, but still visible at 16 px.
const RING: f32 = 1.9;
const RIM: &str = "#05090B";
const DETAIL: &str = "#0B1215";

pub fn level_color(p: u8) -> &'static str {
    match p {
        0..=15 => "#EF4444",
        16..=40 => "#F59E0B",
        _ => "#22C55E",
    }
}

/// Geometry of each silhouette in a 32x32 box.
struct Shape {
    /// Body element(s), unstyled: used for clip, rim, empty area and outline.
    body: &'static str,
    /// Extra stroke-only parts (headset band).
    band: &'static str,
    /// Dark details drawn over the fill (buttons, keys).
    details: &'static str,
    /// Fill direction and extent: vertical (bottom..top) or horizontal (left..right).
    vertical: bool,
    from: f32,
    to: f32,
}

fn shape(kind: Kind) -> Shape {
    match kind {
        Kind::Mouse => Shape {
            body: r#"<rect x="5" y="1" width="22" height="30" rx="11"/>"#,
            band: "",
            details: r#"<path d="M16 2 V12 M6 12.5 H26" stroke-width="1.8" opacity="0.85"/>"#,
            vertical: true,
            from: 31.0,
            to: 1.0,
        },
        Kind::Headset => Shape {
            body: r#"<rect x="2.5" y="15" width="9" height="15.5" rx="3.5"/><rect x="20.5" y="15" width="9" height="15.5" rx="3.5"/>"#,
            band: r#"<path d="M5.5 16 V14 A10.5 10.5 0 0 1 26.5 14 V16"/>"#,
            details: "",
            vertical: true,
            from: 30.5,
            to: 15.0,
        },
        Kind::Controller => Shape {
            body: r#"<path d="M9.5 7.5 H22.5 C26.5 7.5 29 10 30 14.5 L31.2 22 C31.8 26.5 28.5 28.6 26 26 L22.5 22 H9.5 L6 26 C3.5 28.6 0.2 26.5 0.8 22 L2 14.5 C3 10 5.5 7.5 9.5 7.5 Z"/>"#,
            band: "",
            details: r#"<path d="M8.5 12.5 V18.5 M5.5 15.5 H11.5" stroke-width="1.9"/><circle cx="23" cy="13.6" r="1.4" stroke="none"/><circle cx="23" cy="17.4" r="1.4" stroke="none"/>"#,
            vertical: false,
            from: 0.8,
            to: 31.2,
        },
        Kind::Keyboard => Shape {
            body: r#"<rect x="1" y="8.5" width="30" height="16" rx="3"/>"#,
            band: "",
            details: r#"<path d="M4.5 12.5 H27.5 M4.5 16.5 H27.5" stroke-width="1.7" stroke-dasharray="2.2 1.6"/><path d="M9 20.5 H23" stroke-width="1.7"/>"#,
            vertical: false,
            from: 1.0,
            to: 31.0,
        },
    }
}

const BOLT: &str = r##"<path d="M18.5 4 L9.5 18 H15 L13 28 L22.5 13.5 H17 Z" fill="#FFFFFF" stroke="#111827" stroke-width="1.6" stroke-linejoin="round"/>"##;

pub fn svg(l: &Look) -> String {
    if let (true, Some(p)) = (l.number, l.percent) {
        return number_svg(p, l.charge, l.stale);
    }
    let s = shape(l.kind);
    let color = l.percent.map(level_color).unwrap_or("#9CA3AF");

    let fill = match l.percent {
        Some(p) => {
            let len = (s.to - s.from).abs();
            // Keep a sliver visible even at 1-3 % so the icon never looks empty.
            let amount = (len * p as f32 / 100.0).max(if p > 0 { 3.0 } else { 0.0 });
            if s.vertical {
                format!(r#"<rect x="0" y="{}" width="32" height="{amount}" fill="{color}" clip-path="url(#b)"/>"#, s.from - amount)
            } else {
                format!(r#"<rect x="{}" y="0" width="{amount}" height="32" fill="{color}" clip-path="url(#b)"/>"#, s.from)
            }
        }
        None => String::new(),
    };
    let extra = match l.percent {
        Some(_) if l.charge != Charge::Discharging => BOLT.to_string(),
        Some(_) => String::new(),
        None => format!(
            r#"<path d="M5 28 L27 4" stroke="{RIM}" stroke-width="4.6" stroke-linecap="round"/><path d="M5 28 L27 4" stroke="{color}" stroke-width="2.2" stroke-linecap="round"/>"#
        ),
    };
    let opacity = if l.stale || l.percent.is_none() { "0.6" } else { "1" };
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32" width="32" height="32">
<defs><clipPath id="b">{body}</clipPath></defs>
<g opacity="{opacity}">
<g fill="none" stroke="{RIM}" stroke-width="{rim}">{body}{band}</g>
<g fill="#000000" fill-opacity="0.45">{body}</g>
{fill}
<g fill="{DETAIL}" stroke="{DETAIL}" clip-path="url(#b)">{details}</g>
<g fill="none" stroke="{color}" stroke-width="{RING}">{body}{band}</g>
{extra}
</g>
</svg>"##,
        body = s.body,
        band = s.band,
        details = s.details,
        rim = RING + 1.6,
    )
}

/// 3x5 pixel digits, one row per string ('#' = lit). Drawn as rects, so no font is needed.
const DIGITS: [[&str; 5]; 10] = [
    ["###", "#.#", "#.#", "#.#", "###"],
    [".#.", "##.", ".#.", ".#.", "###"],
    ["###", "..#", "###", "#..", "###"],
    ["###", "..#", "###", "..#", "###"],
    ["#.#", "#.#", "###", "..#", "..#"],
    ["###", "#..", "###", "..#", "###"],
    ["###", "#..", "###", "#.#", "###"],
    ["###", "..#", "..#", "..#", "..#"],
    ["###", "#.#", "###", "#.#", "###"],
    ["###", "#.#", "###", "..#", "###"],
];

/// Number mode: the % in digits (level color, white while plugged in) over a level bar.
fn number_svg(p: u8, charge: Charge, stale: bool) -> String {
    let text = p.to_string();
    let n = text.len() as f32;
    // 1-2 digits: 4-unit cells (2 px at 16 px); "100": 3-unit cells to fit.
    let cell = if text.len() <= 2 { 4.0 } else { 3.0 };
    let gap = if text.len() <= 2 { 4.0 } else { 2.0 };
    let width = n * 3.0 * cell + (n - 1.0) * gap;
    let x0 = ((32.0 - width) / 2.0).round();
    let y0 = ((24.0 - 5.0 * cell) / 2.0).round();
    let color = if charge != Charge::Discharging { "#F9FAFB" } else { level_color(p) };

    let (mut rim, mut lit) = (String::new(), String::new());
    for (i, ch) in text.bytes().enumerate() {
        let glyph = DIGITS[(ch - b'0') as usize];
        let gx = x0 + i as f32 * (3.0 * cell + gap);
        for (row, line) in glyph.iter().enumerate() {
            for (col, px) in line.bytes().enumerate() {
                if px != b'#' {
                    continue;
                }
                let (x, y) = (gx + col as f32 * cell, y0 + row as f32 * cell);
                rim += &format!(r#"<rect x="{}" y="{}" width="{}" height="{}"/>"#, x - 1.2, y - 1.2, cell + 2.4, cell + 2.4);
                lit += &format!(r#"<rect x="{x}" y="{y}" width="{cell}" height="{cell}"/>"#);
            }
        }
    }
    let bar_w = (28.0 * p as f32 / 100.0).max(if p > 0 { 2.0 } else { 0.0 });
    let opacity = if stale { "0.6" } else { "1" };
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32" width="32" height="32">
<g opacity="{opacity}">
<g fill="{RIM}">{rim}</g>
<g fill="{color}" shape-rendering="crispEdges">{lit}</g>
<rect x="2" y="25.5" width="28" height="5" rx="2.5" fill="#000000" fill-opacity="0.45" stroke="{RIM}" stroke-width="1.4"/>
<rect x="2" y="25.5" width="{bar_w}" height="5" rx="2.5" fill="{bar}"/>
</g>
</svg>"##,
        bar = level_color(p),
    )
}

/// Rasterizes the SVG to straight (non-premultiplied) RGBA.
pub fn render(svg: &str, size: u32) -> (Vec<u8>, u32) {
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).expect("svg del ícono");
    let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("pixmap");
    let s = size as f32 / 32.0;
    resvg::render(&tree, tiny_skia::Transform::from_scale(s, s), &mut pixmap.as_mut());
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    (rgba, size)
}

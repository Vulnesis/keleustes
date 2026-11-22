//! The brand mark: the Phosphor `FLOW_ARROW` glyph in gold, the one the
//! sidebar shows beside the name. Every other icon (tray, window, menu,
//! installers) is rasterised from the same embedded font here, so they all
//! match. `just icons` regenerates `packaging/icons/` from this code.

use ab_glyph::{Font, FontRef, Outline, OutlineCurve, PxScale, ScaleFont, point};

/// The brand glyph.
pub const MARK: &str = crate::icons::FLOW_ARROW;
/// Canary gold (`theme::DARK.gold`).
pub const GOLD: [u8; 3] = [0xff, 0xc4, 0x00];
/// The dark tile behind the mark (`theme::DARK.bg`).
pub const TILE: [u8; 3] = [0x05, 0x08, 0x0d];
/// A grey that reads on light and dark menus (`theme::DARK.muted`).
pub const MENU_GREY: [u8; 3] = [0x8e, 0x9b, 0xac];

/// Supersampling factor per axis.
const SS: u32 = 4;
/// Tile corner radius, as a fraction of the side.
const RADIUS: f32 = 0.22;
/// Space around the glyph on the tile, as a fraction of the side.
const TILE_PAD: f32 = 0.13;

fn font() -> FontRef<'static> {
    FontRef::try_from_slice(crate::icons::FONT).expect("the embedded Phosphor font parses")
}

/// Where a glyph lands in an `n`×`n` box (y down): `X = x0 + x·s`,
/// `Y = y0 − y·s` for a font-unit point `(x, y)`, its outline centred and
/// fitted to the box less `pad`·n on each side.
struct Fit {
    outline: Outline,
    s: f32,
    x0: f32,
    y0: f32,
}

fn fit(glyph: &str, n: f32, pad: f32) -> Fit {
    let f = font();
    let ch = glyph.chars().next().expect("a glyph");
    let outline = f
        .outline(f.glyph_id(ch))
        .unwrap_or_else(|| panic!("no outline for U+{:04X}", ch as u32));
    // Bounds are y up: min = (x_min, y_max), max = (x_max, y_min).
    let (xmin, xmax) = (outline.bounds.min.x, outline.bounds.max.x);
    let (ymin, ymax) = (outline.bounds.max.y, outline.bounds.min.y);
    let (w, h) = (xmax - xmin, ymax - ymin);
    let inner = n * (1.0 - 2.0 * pad);
    let s = inner / w.max(h);
    Fit {
        x0: n / 2.0 - (xmin + w / 2.0) * s,
        y0: n / 2.0 - h * s / 2.0 + ymax * s,
        s,
        outline,
    }
}

/// Coverage (0..=1) of `glyph` on an `n`×`n` grid, row major, drawn at
/// `SS`× and box-filtered down. `bold` widens strokes by that many output
/// pixels (so thin Phosphor strokes survive 16 px).
fn glyph_mask(glyph: &str, n: u32, pad: f32, bold: f32) -> Vec<f32> {
    let big = n * SS;
    let fit = fit(glyph, big as f32, pad);
    let f = font();
    let ch = glyph.chars().next().unwrap();
    // ab_glyph scales by the font height; ask for the scale giving `fit.s`.
    let px = fit.s * f.height_unscaled();
    let scaled = f.as_scaled(PxScale::from(px));
    debug_assert!((scaled.h_scale_factor() - fit.s).abs() < 1e-4);
    let g = f
        .glyph_id(ch)
        .with_scale_and_position(px, point(fit.x0, fit.y0));
    let mut hi = vec![0f32; (big * big) as usize];
    if let Some(o) = f.outline_glyph(g) {
        let b = o.px_bounds();
        o.draw(|x, y, c| {
            let (x, y) = (b.min.x as i64 + x as i64, b.min.y as i64 + y as i64);
            if (0..big as i64).contains(&x) && (0..big as i64).contains(&y) {
                hi[(y as u32 * big + x as u32) as usize] = c.min(1.0);
            }
        });
    }
    let r = (bold * SS as f32 / 2.0).round() as i64;
    if r > 0 {
        hi = dilate(&hi, big, r);
    }
    downsample(&hi, n)
}

/// Max filter over a disc of radius `r` (hi-res pixels).
fn dilate(src: &[f32], n: u32, r: i64) -> Vec<f32> {
    let n = n as i64;
    let disc: Vec<(i64, i64)> = (-r..=r)
        .flat_map(|dy| (-r..=r).map(move |dx| (dx, dy)))
        .filter(|(dx, dy)| dx * dx + dy * dy <= r * r)
        .collect();
    let mut out = vec![0f32; src.len()];
    for y in 0..n {
        for x in 0..n {
            let mut m = 0f32;
            for (dx, dy) in &disc {
                let (sx, sy) = (x + dx, y + dy);
                if (0..n).contains(&sx) && (0..n).contains(&sy) {
                    m = m.max(src[(sy * n + sx) as usize]);
                    if m >= 1.0 {
                        break;
                    }
                }
            }
            out[(y * n + x) as usize] = m;
        }
    }
    out
}

fn downsample(hi: &[f32], n: u32) -> Vec<f32> {
    let big = n * SS;
    let mut out = vec![0f32; (n * n) as usize];
    for y in 0..n {
        for x in 0..n {
            let mut sum = 0.0;
            for sy in 0..SS {
                for sx in 0..SS {
                    sum += hi[((y * SS + sy) * big + x * SS + sx) as usize];
                }
            }
            out[(y * n + x) as usize] = sum / (SS * SS) as f32;
        }
    }
    out
}

/// Coverage of the rounded-square tile on an `n`×`n` grid.
fn tile_mask(n: u32) -> Vec<f32> {
    let big = n * SS;
    let side = big as f32;
    let r = RADIUS * side;
    let mut hi = vec![0f32; (big * big) as usize];
    for y in 0..big {
        for x in 0..big {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // Distance outside the inner (radius-shrunk) square.
            let dx = (r - px).max(px - (side - r)).max(0.0);
            let dy = (r - py).max(py - (side - r)).max(0.0);
            if dx * dx + dy * dy <= r * r {
                hi[(y * big + x) as usize] = 1.0;
            }
        }
    }
    downsample(&hi, n)
}

/// How much to embolden `glyph` at `n` px: Phosphor regular strokes are
/// 1/16 em, under a pixel at tray sizes; widen them to about 1.25 px.
fn bold_for(glyph: &str, n: u32, pad: f32) -> f32 {
    let em = font().units_per_em().unwrap_or(1000.0);
    let stroke = fit(glyph, n as f32, pad).s * em / 16.0;
    (1.25 - stroke).clamp(0.0, 0.75)
}

/// Straight-alpha RGBA: `glyph` coverage in `fg` over optional `bg` coverage.
fn compose(n: u32, glyph: &[f32], fg: [u8; 3], bg: Option<(&[f32], [u8; 3])>) -> Vec<u8> {
    let mut px = Vec::with_capacity((n * n * 4) as usize);
    for i in 0..(n * n) as usize {
        let ga = glyph[i].clamp(0.0, 1.0);
        let (ta, tc) = match bg {
            Some((t, c)) => (t[i].clamp(0.0, 1.0), c),
            None => (0.0, [0, 0, 0]),
        };
        // The glyph sits inside the tile: clip it to the tile's coverage.
        let ga = if bg.is_some() { ga.min(ta) } else { ga };
        let a = ga + ta * (1.0 - ga);
        let mix = |k: usize| -> u8 {
            if a <= 0.0 {
                return 0;
            }
            let v = (fg[k] as f32 * ga + tc[k] as f32 * ta * (1.0 - ga)) / a;
            v.round().clamp(0.0, 255.0) as u8
        };
        px.extend_from_slice(&[mix(0), mix(1), mix(2), (a * 255.0).round() as u8]);
    }
    px
}

/// The app icon: the gold mark on the dark rounded tile, `n`×`n` RGBA.
/// Also the tray icon: the tile keeps gold legible on light and dark panels.
pub fn icon_rgba(n: u32) -> Vec<u8> {
    let g = glyph_mask(MARK, n, TILE_PAD, bold_for(MARK, n, TILE_PAD));
    let t = tile_mask(n);
    compose(n, &g, GOLD, Some((&t, TILE)))
}

/// Any Phosphor glyph in `rgb` on transparent, `n`×`n` RGBA (menu icons).
pub fn glyph_rgba(glyph: &str, n: u32, rgb: [u8; 3]) -> Vec<u8> {
    let pad = 0.06;
    let g = glyph_mask(glyph, n, pad, bold_for(glyph, n, pad));
    compose(n, &g, rgb, None)
}

/// The window icon for eframe.
pub fn window_icon() -> egui::IconData {
    let n = 256;
    egui::IconData {
        rgba: icon_rgba(n),
        width: n,
        height: n,
    }
}

/// The scalable icon: the tile and the mark's own outline, as SVG.
pub fn icon_svg() -> String {
    let n = 512.0;
    let r = RADIUS * n;
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 512 512\" width=\"512\" height=\"512\">\n\
         <title>Keleustes</title>\n\
         <rect width=\"512\" height=\"512\" rx=\"{r:.2}\" fill=\"#{:02x}{:02x}{:02x}\"/>\n\
         <path fill=\"#{:02x}{:02x}{:02x}\" d=\"{}\"/>\n</svg>\n",
        TILE[0],
        TILE[1],
        TILE[2],
        GOLD[0],
        GOLD[1],
        GOLD[2],
        svg_path(MARK, n, TILE_PAD)
    )
}

/// `glyph`'s outline as SVG path data in an `n`×`n` box (y down).
pub fn svg_path(glyph: &str, n: f32, pad: f32) -> String {
    let fit = fit(glyph, n, pad);
    let p =
        |q: &ab_glyph::Point| format!("{:.2} {:.2}", fit.x0 + q.x * fit.s, fit.y0 - q.y * fit.s);
    let mut d = String::new();
    let mut last: Option<ab_glyph::Point> = None;
    for c in &fit.outline.curves {
        let (start, end) = match c {
            OutlineCurve::Line(a, b) => (*a, *b),
            OutlineCurve::Quad(a, _, b) => (*a, *b),
            OutlineCurve::Cubic(a, _, _, b) => (*a, *b),
        };
        let joined =
            last.is_some_and(|l| (l.x - start.x).abs() < 1e-3 && (l.y - start.y).abs() < 1e-3);
        if !joined {
            if last.is_some() {
                d.push_str("Z ");
            }
            d.push_str(&format!("M{} ", p(&start)));
        }
        match c {
            OutlineCurve::Line(_, b) => d.push_str(&format!("L{} ", p(b))),
            OutlineCurve::Quad(_, c1, b) => d.push_str(&format!("Q{} {} ", p(c1), p(b))),
            OutlineCurve::Cubic(_, c1, c2, b) => {
                d.push_str(&format!("C{} {} {} ", p(c1), p(c2), p(b)))
            }
        }
        last = Some(end);
    }
    if last.is_some() {
        d.push('Z');
    }
    d
}

/// The PNG sizes `packaging/icons/` holds.
pub const PNG_SIZES: [u32; 5] = [32, 64, 128, 256, 512];
/// The sizes inside `keleustes.ico`.
pub const ICO_SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];

/// A Windows icon holding PNG images (`png(n)` encodes one size).
pub fn ico(sizes: &[u32], png: impl Fn(u32) -> Vec<u8>) -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = sizes.iter().map(|&n| (n, png(n))).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (n, data) in &images {
        let dim = if *n >= 256 { 0 } else { *n as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in images {
        out.extend_from_slice(&data);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_gold(p: &[u8]) -> bool {
        // Mostly glyph: gold dominates the blend with the tile.
        p[3] > 200 && p[0] > 0x90 && p[1] > 0x70 && p[2] < 0x40
    }

    #[test]
    fn icon_is_the_gold_mark_on_the_tile() {
        for n in [16, 22, 32, 64] {
            let px = icon_rgba(n);
            assert_eq!(px.len(), (n * n * 4) as usize);
            let gold = px.chunks(4).filter(|p| is_gold(p)).count();
            let total = (n * n) as usize;
            assert!(gold > total / 25 && gold < total / 2, "{n}: {gold} gold");
            // Gold lies inside the glyph's box, not in the padding.
            let edge = (n as f32 * TILE_PAD * 0.7) as u32;
            for y in 0..n {
                for x in 0..n {
                    if x < edge || y < edge || x >= n - edge || y >= n - edge {
                        let i = ((y * n + x) * 4) as usize;
                        assert!(!is_gold(&px[i..i + 4]), "{n}: gold at {x},{y}");
                    }
                }
            }
            // Corners are transparent (rounded tile); the centre row has tile.
            assert_eq!(px[3], 0, "{n}: corner");
            let mid = ((n / 2 * n + 1) * 4) as usize;
            assert_eq!(&px[mid..mid + 4], &[TILE[0], TILE[1], TILE[2], 255]);
        }
    }

    #[test]
    fn menu_glyphs_are_transparent_with_ink() {
        let px = glyph_rgba(crate::icons::GAUGE, 16, MENU_GREY);
        assert_eq!(px.len(), 16 * 16 * 4);
        assert!(px.chunks(4).any(|p| p[3] == 0));
        assert!(px.chunks(4).filter(|p| p[3] > 128).count() > 10);
    }

    #[test]
    fn svg_carries_the_glyph_outline() {
        let d = svg_path(MARK, 512.0, TILE_PAD);
        assert!(
            d.starts_with('M') && d.ends_with('Z') && d.len() > 100,
            "{d}"
        );
        let svg = icon_svg();
        assert!(svg.contains("#ffc400") && svg.contains("#05080d"));
        // Every coordinate stays inside the padded box.
        let nums: Vec<f32> = d
            .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
            .filter_map(|t| t.parse().ok())
            .collect();
        assert!(!nums.is_empty());
        let (lo, hi) = (512.0 * TILE_PAD - 1.0, 512.0 * (1.0 - TILE_PAD) + 1.0);
        assert!(nums.iter().all(|v| (lo..=hi).contains(v)), "{lo}..{hi}");
    }

    fn png(n: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, n, n);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            let mut w = e.write_header().unwrap();
            w.write_image_data(&icon_rgba(n)).unwrap();
        }
        out
    }

    fn png_size(bytes: &[u8]) -> (u32, u32) {
        let d = png::Decoder::new(std::io::Cursor::new(bytes));
        let r = d.read_info().unwrap();
        (r.info().width, r.info().height)
    }

    #[test]
    fn ico_holds_each_size() {
        let ico = ico(&[16, 256], png);
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 2, 0]);
        assert_eq!(ico[6], 16);
        assert_eq!(ico[22], 0, "256 is written as 0");
        let off = u32::from_le_bytes(ico[18..22].try_into().unwrap()) as usize;
        assert_eq!(png_size(&ico[off..]), (16, 16));
    }

    fn icons_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/icons")
    }

    /// The checked-in icons are what this code draws (`just icons` rewrites them).
    #[test]
    fn packaging_icons_match() {
        let dir = icons_dir();
        for n in PNG_SIZES {
            let bytes = std::fs::read(dir.join(format!("keleustes-{n}.png"))).unwrap();
            assert_eq!(png_size(&bytes), (n, n), "keleustes-{n}.png");
        }
        let svg = std::fs::read_to_string(dir.join("keleustes.svg")).unwrap();
        assert_eq!(svg, icon_svg(), "run `just icons`");
        let ico = std::fs::read(dir.join("keleustes.ico")).unwrap();
        assert_eq!(ico[4] as usize, ICO_SIZES.len());
    }

    /// Writes `packaging/icons/` (`just icons`).
    #[test]
    #[ignore]
    fn write_packaging_icons() {
        let dir = icons_dir();
        for n in PNG_SIZES {
            std::fs::write(dir.join(format!("keleustes-{n}.png")), png(n)).unwrap();
        }
        std::fs::write(dir.join("keleustes.ico"), ico(&ICO_SIZES, png)).unwrap();
        std::fs::write(dir.join("keleustes.svg"), icon_svg()).unwrap();
    }
}

//! Keleustes theme: the ThreatCanary palette (near-black panels, canary
//! gold primary, cyan/violet/green/red accents, square corners) in dark and
//! light. Screens read accent colours from [`palette`] so both modes keep
//! contrast; the top-bar switch flips the mode and it is persisted in the
//! `ui.theme` setting ("system", "light", "dark").

use egui::{Color32, CornerRadius, Stroke, Visuals};

pub const SETTING: &str = "ui.theme";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    System,
    Light,
    Dark,
}

impl Mode {
    pub fn parse(s: &str) -> Self {
        match s {
            "light" => Mode::Light,
            "dark" => Mode::Dark,
            _ => Mode::System,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::System => "system",
            Mode::Light => "light",
            Mode::Dark => "dark",
        }
    }
}

/// Accent and surface colours for the current mode. Text accents are
/// darkened in light mode so they keep 4.5:1 on white.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub bg: Color32,
    pub panel: Color32,
    pub panel2: Color32,
    pub line: Color32,
    pub muted: Color32,
    pub text: Color32,
    /// Fill colour for primary buttons and highlights.
    pub gold: Color32,
    /// Gold used as text or a thin line.
    pub gold_text: Color32,
    pub on_gold: Color32,
    pub cyan: Color32,
    pub violet: Color32,
    pub green: Color32,
    pub red: Color32,
    pub amber: Color32,
    pub blue: Color32,
    /// The outline of a highlighted panel (gold, dimmed).
    pub gold_edge: Color32,
    /// The soft glow around a highlighted panel.
    pub glow: Color32,
    /// The faint blueprint grid behind the main area.
    pub grid: Color32,
}

pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x05, 0x08, 0x0d),
    panel: Color32::from_rgb(0x0b, 0x11, 0x1a),
    panel2: Color32::from_rgb(0x10, 0x18, 0x24),
    line: Color32::from_rgb(0x1f, 0x2b, 0x3a),
    muted: Color32::from_rgb(0x8e, 0x9b, 0xac),
    text: Color32::from_rgb(0xf4, 0xf7, 0xfb),
    gold: Color32::from_rgb(0xff, 0xc4, 0x00),
    gold_text: Color32::from_rgb(0xff, 0xc4, 0x00),
    on_gold: Color32::from_rgb(0x05, 0x08, 0x0d),
    cyan: Color32::from_rgb(0x11, 0xd9, 0xf2),
    violet: Color32::from_rgb(0x9d, 0x7c, 0xff),
    green: Color32::from_rgb(0x2b, 0xe3, 0x9b),
    red: Color32::from_rgb(0xff, 0x6b, 0x73),
    amber: Color32::from_rgb(0xff, 0x8a, 0x00),
    blue: Color32::from_rgb(0x21, 0x88, 0xff),
    gold_edge: Color32::from_rgb(0x5c, 0x4a, 0x10),
    glow: Color32::from_rgba_premultiplied(33, 25, 0, 33),
    grid: Color32::from_rgba_premultiplied(10, 15, 21, 64),
};

pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xf4, 0xf6, 0xf9),
    panel: Color32::from_rgb(0xff, 0xff, 0xff),
    panel2: Color32::from_rgb(0xea, 0xee, 0xf4),
    line: Color32::from_rgb(0xd5, 0xdc, 0xe6),
    muted: Color32::from_rgb(0x4f, 0x5d, 0x6d),
    text: Color32::from_rgb(0x0a, 0x11, 0x1a),
    gold: Color32::from_rgb(0xff, 0xc4, 0x00),
    gold_text: Color32::from_rgb(0x8a, 0x53, 0x00),
    on_gold: Color32::from_rgb(0x05, 0x08, 0x0d),
    cyan: Color32::from_rgb(0x07, 0x6d, 0x7c),
    violet: Color32::from_rgb(0x6b, 0x4f, 0xd8),
    green: Color32::from_rgb(0x08, 0x73, 0x4c),
    red: Color32::from_rgb(0xb3, 0x12, 0x1e),
    amber: Color32::from_rgb(0xb3, 0x5f, 0x00),
    blue: Color32::from_rgb(0x17, 0x69, 0xd1),
    gold_edge: Color32::from_rgb(0xe2, 0xb1, 0x00),
    glow: Color32::from_rgba_premultiplied(26, 20, 0, 26),
    grid: Color32::from_rgba_premultiplied(3, 4, 6, 18),
};

/// The palette for the context's current visuals.
pub fn palette(ctx: &egui::Context) -> Palette {
    if ctx.global_style().visuals.dark_mode {
        DARK
    } else {
        LIGHT
    }
}

/// Resolves System against the OS preference egui reports.
pub fn is_dark(mode: Mode, ctx: &egui::Context) -> bool {
    match mode {
        Mode::Dark => true,
        Mode::Light => false,
        Mode::System => ctx.system_theme().is_none_or(|t| t == egui::Theme::Dark),
    }
}

pub fn visuals(dark: bool) -> Visuals {
    let p = if dark { DARK } else { LIGHT };
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let square = CornerRadius::ZERO;
    v.panel_fill = p.panel;
    v.window_fill = p.panel;
    v.extreme_bg_color = p.bg;
    v.faint_bg_color = p.panel2;
    v.code_bg_color = p.bg;
    v.window_stroke = Stroke::new(1.0, p.line);
    v.window_corner_radius = square;
    v.menu_corner_radius = square;
    v.hyperlink_color = p.cyan;
    v.warn_fg_color = p.amber;
    v.error_fg_color = p.red;
    v.override_text_color = Some(p.text);
    // A gold tint, not a gold fill: selected rows and nav items keep their
    // normal text colour and stay readable in both modes.
    v.selection.bg_fill = if dark {
        Color32::from_rgba_unmultiplied(0xff, 0xc4, 0x00, 46)
    } else {
        Color32::from_rgb(0xff, 0xec, 0xb0)
    };
    v.selection.stroke = Stroke::new(1.0, p.gold_text);
    v.window_shadow.color = Color32::from_black_alpha(if dark { 110 } else { 40 });
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = square;
    }
    v.widgets.noninteractive.bg_fill = p.panel;
    v.widgets.noninteractive.weak_bg_fill = p.panel;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.inactive.bg_fill = p.panel2;
    v.widgets.inactive.weak_bg_fill = p.panel2;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.line);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.hovered.bg_fill = p.panel2;
    v.widgets.hovered.weak_bg_fill = p.panel2;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.gold);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, p.text);
    // egui draws `strong()` text in the active colour, so it must be the
    // text colour; a pressed widget is marked by its gold outline instead.
    v.widgets.active.bg_fill = p.panel2;
    v.widgets.active.weak_bg_fill = p.panel2;
    v.widgets.active.bg_stroke = Stroke::new(1.5, p.gold);
    v.widgets.active.fg_stroke = Stroke::new(1.5, p.text);
    v.widgets.open.bg_fill = p.panel2;
    v.widgets.open.weak_bg_fill = p.panel2;
    v.widgets.open.bg_stroke = Stroke::new(1.0, p.gold);
    v
}

/// Applies the mode to the context.
pub fn apply(ctx: &egui::Context, mode: Mode) {
    let dark = is_dark(mode, ctx);
    ctx.set_visuals(visuals(dark));
}

/// A primary (gold) button.
pub fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let p = palette(ui.ctx());
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(p.on_gold).strong())
            .fill(p.gold)
            .stroke(Stroke::NONE),
    )
}

/// The small uppercase mono eyebrow above a page title.
pub fn eyebrow(ui: &mut egui::Ui, text: &str) {
    let p = palette(ui.ctx());
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .monospace()
            .size(11.0)
            .color(p.gold_text),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lum(c: Color32) -> f64 {
        let f = |v: u8| {
            let s = v as f64 / 255.0;
            if s <= 0.03928 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn text_colours_meet_aa_on_panels() {
        for p in [DARK, LIGHT] {
            for c in [
                p.text,
                p.muted,
                p.gold_text,
                p.cyan,
                p.green,
                p.red,
                p.violet,
            ] {
                assert!(contrast(c, p.panel) >= 4.5, "{c:?} on {:?}", p.panel);
            }
            assert!(contrast(p.on_gold, p.gold) >= 4.5);
        }
    }

    #[test]
    fn strong_text_and_selection_stay_readable() {
        for dark in [true, false] {
            let v = visuals(dark);
            let p = if dark { DARK } else { LIGHT };
            assert_eq!(
                v.widgets.active.fg_stroke.color, p.text,
                "strong text colour"
            );
            // Blend the selection tint over the panel, as egui paints it.
            let a = v.selection.bg_fill.a() as f32 / 255.0;
            let mix = |s: u8, d: u8| (s as f32 * a + d as f32 * (1.0 - a)).round() as u8;
            let s = v.selection.bg_fill;
            let bg = if a >= 0.999 {
                s
            } else {
                let u = s.to_srgba_unmultiplied();
                Color32::from_rgb(
                    mix(u[0], p.panel.r()),
                    mix(u[1], p.panel.g()),
                    mix(u[2], p.panel.b()),
                )
            };
            for c in [p.text, v.selection.stroke.color] {
                assert!(
                    contrast(c, bg) >= 4.5,
                    "{c:?} on selection {bg:?} (dark={dark})"
                );
            }
        }
    }

    #[test]
    fn mode_roundtrip() {
        for m in [Mode::System, Mode::Light, Mode::Dark] {
            assert_eq!(Mode::parse(m.as_str()), m);
        }
    }
}

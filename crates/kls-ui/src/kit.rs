//! The approved design's components (the Keleustes design canvas), as egui
//! building blocks. Screens use these instead of styling widgets by hand so
//! every page matches the artboards: square panels on the panel colour with
//! a 1px line, gold-edged glowing panels for what needs attention, mono
//! uppercase labels, pill chips, KPI tiles and the faint blueprint grid, plus
//! the shared layout pieces (wrapping grids, side-by-side splits) and small
//! controls (tabs, filter chips, field labels, list rows, table rules).

use crate::icons;
use crate::theme::{Palette, palette};
use egui::{
    Align, Color32, CornerRadius, CursorIcon, Frame, Layout, Margin, Response, RichText, Sense,
    Shape, Stroke, Ui, UiBuilder, vec2,
};

pub const PILL: CornerRadius = CornerRadius::same(255);

/// A plain panel: panel fill, 1px line, square corners.
pub fn card(ui: &Ui) -> Frame {
    let p = palette(ui.ctx());
    Frame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0, p.line))
        .corner_radius(CornerRadius::ZERO)
}

/// A highlighted panel ("Needs you", inspectors, drawers): gold edge and a
/// soft gold glow.
pub fn glow(ui: &Ui) -> Frame {
    let p = palette(ui.ctx());
    card(ui)
        .stroke(Stroke::new(1.0, p.gold_edge))
        .shadow(egui::Shadow {
            offset: [0, 0],
            blur: 44,
            spread: 0,
            color: p.glow,
        })
}

/// Mono, uppercase, letter-spaced label (table headers, KPI labels, groups).
pub fn caps(text: &str, color: Color32) -> RichText {
    RichText::new(text.to_uppercase())
        .monospace()
        .size(11.0)
        .color(color)
}

/// A table header cell.
pub fn th(ui: &mut Ui, text: &str) {
    let p = palette(ui.ctx());
    ui.label(caps(text, p.muted));
}

/// Page title: gold icon and the screen name, with actions on the right.
pub fn page_header(ui: &mut Ui, icon: &str, title: &str, actions: impl FnOnce(&mut Ui)) {
    let p = palette(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(RichText::new(icon).size(28.0).color(p.gold_text));
        ui.label(RichText::new(title).size(28.0).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), actions);
    });
    ui.add_space(10.0);
}

/// A titled panel: header row (icon, title, optional action on the right)
/// over a line, then the body. `highlight` uses the gold-edged glow panel.
pub fn section<R>(
    ui: &mut Ui,
    icon: &str,
    title: &str,
    highlight: bool,
    action: impl FnOnce(&mut Ui),
    body: impl FnOnce(&mut Ui) -> R,
) -> R {
    let p = palette(ui.ctx());
    let frame = if highlight { glow(ui) } else { card(ui) };
    frame
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            Frame::new()
                .inner_margin(Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon).size(16.0).color(p.gold_text));
                        ui.label(RichText::new(title).size(16.0).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), action);
                    });
                });
            let r = ui.painter();
            let rect = ui.min_rect();
            r.hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, p.line));
            Frame::new()
                .inner_margin(Margin::symmetric(16, 10))
                .show(ui, body)
                .inner
        })
        .inner
}

/// One KPI tile: icon and mono label, a large value, an optional note.
/// Clickable (returns the tile's response).
pub fn kpi(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    value: &str,
    note: Option<&str>,
    value_color: Option<Color32>,
) -> Response {
    let p = palette(ui.ctx());
    let inner = card(ui)
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_min_width(150.0);
            ui.vertical(|ui| {
                ui.label(caps(&format!("{icon} {label}"), p.muted));
                ui.label(
                    RichText::new(value)
                        .size(26.0)
                        .strong()
                        .color(value_color.unwrap_or(p.text)),
                );
                if let Some(n) = note {
                    ui.label(RichText::new(n).size(12.0).color(p.muted));
                }
            });
        });
    ui.interact(
        inner.response.rect,
        inner.response.id.with("kpi"),
        Sense::click(),
    )
}

/// A pill chip with an outline and text in `color`.
pub fn chip(ui: &mut Ui, text: &str, color: Color32) -> Response {
    Frame::new()
        .stroke(Stroke::new(1.0, color))
        .corner_radius(PILL)
        .inner_margin(Margin::symmetric(9, 2))
        .show(ui, |ui| {
            // Never wrap inside a chip: a wrapping parent moves the whole
            // chip to the next row instead of stacking its letters.
            ui.add(egui::Label::new(RichText::new(text).size(12.0).color(color)).extend());
        })
        .response
}

/// A filled pill (selected filter, primary tag).
pub fn chip_filled(ui: &mut Ui, text: &str) -> Response {
    let p = palette(ui.ctx());
    Frame::new()
        .fill(p.gold)
        .corner_radius(PILL)
        .inner_margin(Margin::symmetric(9, 2))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(RichText::new(text).size(12.0).color(p.on_gold).strong()).extend(),
            );
        })
        .response
}

/// A small status dot.
pub fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// Outline button in the line colour (secondary actions).
pub fn ghost_button(ui: &mut Ui, text: &str) -> Response {
    let p = palette(ui.ctx());
    ui.add(
        egui::Button::new(RichText::new(text).color(p.text))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0, p.line)),
    )
}

/// Outline button in red (stop, reject, revoke, delete).
pub fn danger_button(ui: &mut Ui, text: &str) -> Response {
    let p = palette(ui.ctx());
    ui.add(
        egui::Button::new(RichText::new(text).color(p.red))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0, p.red)),
    )
}

/// Paints the faint blueprint grid over `rect` (call before content).
pub fn grid_background(ui: &Ui, rect: egui::Rect) {
    let p = palette(ui.ctx());
    let painter = ui.painter_at(rect);
    let step = 56.0;
    let stroke = Stroke::new(1.0, p.grid);
    let mut x = rect.left() + step;
    while x < rect.right() {
        painter.vline(x, rect.y_range(), stroke);
        x += step;
    }
    let mut y = rect.top() + step;
    while y < rect.bottom() {
        painter.hline(rect.x_range(), y, stroke);
        y += step;
    }
}

/// Status colour by a state word (live/running/ok, waiting, failed, ended…).
pub fn status_color(p: &Palette, state: &str) -> Color32 {
    match state {
        "live" | "running" | "online" | "ok" | "succeeded" | "done" | "clean" | "started" => {
            p.green
        }
        "waiting" | "waiting_approval" | "queued" | "held" | "pending" | "warn" | "review" => {
            p.gold_text
        }
        "failed" | "refused" | "error" | "revoked" | "blocked" | "fail" => p.red,
        "paused" | "idle" | "ended" | "offline" | "cancelled" | "expired" => p.muted,
        _ => p.cyan,
    }
}

// ---- text and lines ----------------------------------------------------------

/// Monospace text.
pub fn mono(text: &str, size: f32, color: Color32) -> RichText {
    RichText::new(text).monospace().size(size).color(color)
}

/// A text link in `color`, underlined on hover.
pub fn link(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let r =
        ui.add(egui::Label::new(RichText::new(text).size(13.0).color(color)).sense(Sense::click()));
    if r.hovered() {
        ui.painter()
            .hline(r.rect.x_range(), r.rect.bottom(), Stroke::new(1.0, color));
    }
    r.on_hover_cursor(CursorIcon::PointingHand)
}

/// An empty-state line: muted icon and label.
pub fn empty(ui: &mut Ui, icon: &str, text: &str) {
    let p = palette(ui.ctx());
    ui.add_space(12.0);
    ui.label(RichText::new(icons::label(icon, text)).color(p.muted));
    ui.add_space(12.0);
}

/// A table row's bottom line. Call at the start of every table cell: draws
/// across the cell and the gaps either side, widening the clip just enough
/// for the line.
pub fn rule(ui: &mut Ui) {
    let p = palette(ui.ctx());
    let r = ui.max_rect();
    let s = ui.spacing().item_spacing;
    let half = s.x / 2.0 + 1.0;
    let clip = ui.clip_rect().expand2(vec2(half, s.y / 2.0 + 1.0));
    ui.painter().with_clip_rect(clip).hline(
        (r.left() - half)..=(r.right() + half),
        r.bottom() + s.y / 2.0,
        Stroke::new(1.0, p.line),
    );
}

/// The line under a table's `th` header cells: paint inside the header row.
pub fn header_rule(ui: &Ui) {
    let p = palette(ui.ctx());
    let r = ui.max_rect();
    ui.painter()
        .hline(r.x_range(), r.bottom(), Stroke::new(1.0, p.line));
}

/// A full-width 1px line at the cursor.
pub fn hline_full(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, color));
}

/// Paints a 1px line under each table row, given the rows' cell rects
/// (merged per row), from the left of `ui`'s content to its right edge.
pub fn row_lines(ui: &Ui, rects: &[egui::Rect], color: Color32) {
    let left = ui.min_rect().left();
    let right = ui.max_rect().right();
    let mut ys: Vec<f32> = rects.iter().map(|r| r.bottom().round()).collect();
    ys.dedup();
    for y in ys {
        ui.painter().hline(left..=right, y, Stroke::new(1.0, color));
    }
}

/// A 6px meter on the second panel colour, `frac` (0..=1) filled in `color`.
pub fn meter(ui: &mut Ui, frac: f32, color: Color32) {
    let p = palette(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, p.panel2);
    let mut fill = rect;
    fill.set_width(rect.width() * frac.clamp(0.0, 1.0));
    ui.painter().rect_filled(fill, 0.0, color);
}

// ---- chips, tabs and form labels ---------------------------------------------

/// A clickable filter chip: filled gold when `on`, outlined in `color`
/// otherwise.
pub fn pick_chip(ui: &mut Ui, text: &str, on: bool, color: Color32) -> Response {
    let r = if on {
        chip_filled(ui, text)
    } else {
        chip(ui, text, color)
    };
    ui.interact(r.rect, r.id.with("pick"), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand)
}

/// A clickable filter pill outlined in the text colour (`pick_chip`).
pub fn pill(ui: &mut Ui, text: &str, on: bool) -> Response {
    let p = palette(ui.ctx());
    pick_chip(ui, text, on, p.text)
}

/// A clickable filter chip outlined in the muted colour (`pick_chip`).
pub fn toggle_chip(ui: &mut Ui, text: &str, on: bool) -> Response {
    let p = palette(ui.ctx());
    pick_chip(ui, text, on, p.muted)
}

/// A small outlined pill in the line colour (labels, capabilities).
pub fn tag(ui: &mut Ui, text: &str) {
    let p = palette(ui.ctx());
    Frame::new()
        .stroke(Stroke::new(1.0, p.line))
        .corner_radius(PILL)
        .inner_margin(Margin::symmetric(8, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0).color(p.muted));
        });
}

/// A tab button: gold edge and second panel fill when `on`, line edge and
/// muted text otherwise.
pub fn tab(ui: &mut Ui, text: &str, on: bool) -> Response {
    tab_rich(ui, RichText::new(text), on, 0.0)
}

/// `tab` with the caller's text style (e.g. a size) and a minimum height.
pub fn tab_rich(ui: &mut Ui, text: RichText, on: bool, min_h: f32) -> Response {
    let p = palette(ui.ctx());
    let (fg, edge, fill) = if on {
        (p.text, p.gold, p.panel2)
    } else {
        (p.muted, p.line, Color32::TRANSPARENT)
    };
    ui.add(
        egui::Button::new(text.strong().color(fg))
            .fill(fill)
            .stroke(Stroke::new(1.0, edge))
            .min_size(vec2(0.0, min_h)),
    )
}

/// The small muted label above a form field.
pub fn field_label(ui: &mut Ui, text: &str) -> Response {
    let p = palette(ui.ctx());
    ui.label(RichText::new(text).size(12.0).color(p.muted))
}

/// A control with its `field_label` caption above, 4px apart.
pub fn field<R>(ui: &mut Ui, caption: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        field_label(ui, caption);
        add(ui)
    })
    .inner
}

// ---- rows and tiles ----------------------------------------------------------

/// A full-width list row: padding, a line below and a hover tint;
/// `selected` adds the tint and a gold left edge. Widgets inside stay
/// clickable; the returned response is the row's own click.
pub fn list_row<R>(
    ui: &mut Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    selected: bool,
    add: impl FnOnce(&mut Ui) -> R,
) -> (Response, R) {
    let p = palette(ui.ctx());
    let out = ui.scope_builder(UiBuilder::new().id_salt(salt).sense(Sense::click()), |ui| {
        let hovered = ui.response().contains_pointer();
        let bg = ui.painter().add(Shape::Noop);
        let inner = Frame::new()
            .inner_margin(Margin::symmetric(4, 11))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui)
            });
        let rect = inner.response.rect;
        if selected || hovered {
            ui.painter()
                .set(bg, Shape::rect_filled(rect, 0.0, p.panel2));
        }
        if selected {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
                0.0,
                p.gold,
            );
        }
        ui.painter()
            .hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, p.line));
        inner.inner
    });
    (
        out.response.on_hover_cursor(CursorIcon::PointingHand),
        out.inner,
    )
}

/// A clickable panel filling its width (grid cards, list tiles): 16px
/// padding, a 1px line, gold and glowing when selected, gold edge on hover.
/// The click is sensed under the content, so buttons inside still work.
pub fn tile(ui: &mut Ui, selected: bool, body: impl FnOnce(&mut Ui)) -> Response {
    let p = palette(ui.ctx());
    let r = ui
        .scope_builder(UiBuilder::new().sense(Sense::click()), |ui| {
            let hovered = ui.response().hovered();
            let edge = if selected {
                p.gold
            } else if hovered {
                p.gold_edge
            } else {
                p.line
            };
            let mut f = card(ui)
                .stroke(Stroke::new(1.0, edge))
                .inner_margin(Margin::same(16));
            if selected {
                f = f.shadow(egui::Shadow {
                    offset: [0, 0],
                    blur: 30,
                    spread: 0,
                    color: p.glow,
                });
            }
            f.show(ui, |ui| {
                ui.set_width(ui.available_width());
                body(ui)
            });
        })
        .response;
    r.on_hover_cursor(CursorIcon::PointingHand)
}

/// A status tile filling its width: panel, 1px `edge`, 16px padding, at
/// least 76px tall, content stacked 6px apart.
pub fn status_tile(ui: &mut Ui, edge: Color32, body: impl FnOnce(&mut Ui)) {
    card(ui)
        .stroke(Stroke::new(1.0, edge))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(76.0);
            ui.spacing_mut().item_spacing.y = 6.0;
            body(ui);
        });
}

/// A panel filling its width with a 3px `color` strip on top: name, a large
/// value, a note.
pub fn strip_tile(ui: &mut Ui, color: Color32, name: &str, value: &str, note: &str) -> Response {
    let p = palette(ui.ctx());
    let resp = card(ui)
        .inner_margin(Margin {
            left: 16,
            right: 16,
            top: 19,
            bottom: 16,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(name).strong().size(15.0));
            ui.label(RichText::new(value).strong().size(26.0));
            ui.label(RichText::new(note).size(13.0).color(p.muted));
        })
        .response;
    let strip = egui::Rect::from_min_size(resp.rect.min, vec2(resp.rect.width(), 3.0));
    ui.painter().rect_filled(strip, 0.0, color);
    resp
}

/// A KPI tile exactly `width` wide, margins and stroke included (`kpi`
/// sizes to its content): icon and mono label, a large value, a note.
/// Clickable, gold edge on hover.
pub fn kpi_tile(
    ui: &mut Ui,
    width: f32,
    icon: &str,
    label: &str,
    value: &str,
    note: Option<&str>,
    value_color: Option<Color32>,
) -> Response {
    let p = palette(ui.ctx());
    let inner = card(ui)
        .inner_margin(Margin::symmetric(18, 14))
        .show(ui, |ui| {
            // The grid lays tiles out in a row; the tile's own content stacks.
            ui.vertical(|ui| {
                // 2 × 18px margin and the 1px stroke either side.
                ui.set_width(width - 38.0);
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.add(egui::Label::new(caps(&format!("{icon} {label}"), p.muted)).truncate());
                ui.label(
                    RichText::new(value)
                        .size(28.0)
                        .strong()
                        .color(value_color.unwrap_or(p.text)),
                );
                ui.add(
                    egui::Label::new(RichText::new(note.unwrap_or(" ")).size(12.0).color(p.muted))
                        .truncate(),
                );
            });
        });
    let r = ui.interact(
        inner.response.rect,
        inner.response.id.with("kpi"),
        Sense::click(),
    );
    if r.hovered() {
        ui.painter().rect_stroke(
            r.rect,
            0.0,
            Stroke::new(1.0, p.gold_edge),
            egui::StrokeKind::Inside,
        );
    }
    r.on_hover_cursor(CursorIcon::PointingHand)
}

// ---- layout --------------------------------------------------------------------

/// Height offered to side-by-side children that should grow as tall as
/// they need (nested scroll areas cap themselves).
pub const GROW: f32 = 10_000.0;

/// Column count and (whole-pixel) column width for `n` cells at least
/// `min_w` wide, `gap` apart, in width `w` (CSS `repeat(auto-fit,
/// minmax(min_w, 1fr))`). With `stretch`, fewer cells than would fit widen
/// to fill the row; otherwise they keep the full row's column width.
pub fn grid_columns(w: f32, min_w: f32, gap: f32, n: usize, stretch: bool) -> (usize, f32) {
    let mut cols = (((w + gap) / (min_w + gap)).floor() as usize).max(1);
    if stretch {
        cols = cols.min(n.max(1));
    }
    let cw = ((w - gap * (cols - 1) as f32) / cols as f32).floor();
    (cols, cw)
}

/// A wrapping grid of equal cells (see `grid_columns`). Each cell is laid
/// out top-down in its own column of exactly the cell width, so content
/// stacks inside it.
#[derive(Clone, Copy, Debug)]
pub struct Grid {
    min_w: f32,
    gap: f32,
    stretch: bool,
    tight_rows: bool,
    cell_h: f32,
}

impl Grid {
    /// Cells at least `min_w` wide, 12px apart, stretching to fill a short
    /// row; rows `gap` plus the item spacing apart.
    pub fn new(min_w: f32) -> Self {
        Self {
            min_w,
            gap: 12.0,
            stretch: true,
            tight_rows: false,
            cell_h: 0.0,
        }
    }

    /// Space between columns and between rows.
    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    /// A short row keeps the full row's column width instead of stretching.
    pub fn keep_width(mut self) -> Self {
        self.stretch = false;
        self
    }

    /// Rows exactly `gap` apart, with `gap` after the last row too.
    pub fn tight_rows(mut self) -> Self {
        self.tight_rows = true;
        self
    }

    /// Offer cells `GROW` height (for content that sizes to what is left).
    pub fn tall(mut self) -> Self {
        self.cell_h = GROW;
        self
    }

    /// Lays `n` cells out; `cell` gets the index and the cell width.
    pub fn show(self, ui: &mut Ui, n: usize, mut cell: impl FnMut(&mut Ui, usize, f32)) {
        if n == 0 {
            return;
        }
        let (cols, cw) = grid_columns(ui.available_width(), self.min_w, self.gap, n, self.stretch);
        let sy = ui.spacing().item_spacing.y;
        for (row, start) in (0..n).step_by(cols).enumerate() {
            if row > 0 && !self.tight_rows {
                ui.add_space(self.gap);
            }
            ui.horizontal_top(|ui| {
                let sx = ui.spacing().item_spacing.x;
                for (c, i) in (start..(start + cols).min(n)).enumerate() {
                    if c > 0 {
                        ui.add_space((self.gap - sx).max(0.0));
                    }
                    ui.allocate_ui_with_layout(
                        vec2(cw, self.cell_h),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(cw);
                            cell(ui, i, cw);
                        },
                    );
                }
            });
            if self.tight_rows {
                ui.add_space(self.gap - sy);
            }
        }
    }
}

/// `n` cells in equal columns (at least `min_w` wide, at most `max_cols`),
/// separated by 1px lines (the artboards' `gap: 1px` grids).
pub fn cell_grid(
    ui: &mut Ui,
    n: usize,
    min_w: f32,
    max_cols: usize,
    mut cell: impl FnMut(&mut Ui, usize),
) {
    let p = palette(ui.ctx());
    let stroke = Stroke::new(1.0, p.line);
    let w = ui.available_width();
    let cols = ((w / min_w).floor() as usize).clamp(1, max_cols.max(1));
    let cw = w / cols as f32;
    let spacing = ui.spacing().item_spacing;
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let idx: Vec<usize> = (0..n).collect();
    for (r, chunk) in idx.chunks(cols).enumerate() {
        let row = ui
            .horizontal_top(|ui| {
                for &i in chunk {
                    let h = ui.available_height().min(GROW);
                    ui.allocate_ui_with_layout(vec2(cw, h), Layout::top_down(Align::Min), |ui| {
                        ui.set_width(cw);
                        ui.spacing_mut().item_spacing = spacing;
                        Frame::new()
                            .inner_margin(Margin::symmetric(14, 10))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                cell(ui, i)
                            });
                    });
                }
            })
            .response
            .rect;
        let x = row.left();
        if r > 0 {
            ui.painter()
                .hline(x..=x + cw * cols as f32, row.top(), stroke);
        }
        for c in 1..cols {
            ui.painter().vline(x + cw * c as f32, row.y_range(), stroke);
        }
    }
    ui.spacing_mut().item_spacing = spacing;
}

/// How `Split` shares the width between its two columns.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Widths {
    /// The first column takes this share of the width after the gap (at
    /// least its minimum); the second the rest.
    First(f32),
    /// The second column takes this share of the whole width (at least its
    /// minimum); the first the rest.
    Second(f32),
    /// Two equal (whole-pixel) halves.
    Halves,
    /// The first column takes `share` of the whole width, clamped to its
    /// minimum..=`max`; the second the rest.
    FirstClamped { share: f32, max: f32 },
    /// CSS `flex: 1 1 <min>`: each column its minimum plus half the spare
    /// width.
    Flex,
}

/// Two panels side by side when both minimum widths (and the gap) fit,
/// stacked (first above second, `gap` apart) otherwise.
#[derive(Clone, Copy, Debug)]
pub struct Split {
    min_a: f32,
    min_b: f32,
    widths: Widths,
    gap: f32,
    fit_height: bool,
}

impl Split {
    /// Columns at least `min_a` and `min_b` wide, 16px apart, offered
    /// `GROW` height.
    pub fn new(min_a: f32, min_b: f32, widths: Widths) -> Self {
        Self {
            min_a,
            min_b,
            widths,
            gap: 16.0,
            fit_height: false,
        }
    }

    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    /// Offer the columns the height left in `ui` instead of `GROW`.
    pub fn fit_height(mut self) -> Self {
        self.fit_height = true;
        self
    }

    /// The two column widths in width `w`, or `None` to stack.
    pub fn widths(&self, w: f32) -> Option<(f32, f32)> {
        let gap = self.gap;
        if w < self.min_a + self.min_b + gap {
            return None;
        }
        Some(match self.widths {
            Widths::First(share) => {
                let a = ((w - gap) * share).max(self.min_a);
                (a, w - gap - a)
            }
            Widths::Second(share) => {
                let b = (w * share).max(self.min_b);
                (w - b - gap, b)
            }
            Widths::Halves => {
                let a = ((w - gap) / 2.0).floor();
                (a, a)
            }
            Widths::FirstClamped { share, max } => {
                let a = (w * share).clamp(self.min_a, max);
                (a, w - a - gap)
            }
            Widths::Flex => {
                let extra = (w - self.min_a - self.min_b - gap) / 2.0;
                (self.min_a + extra, self.min_b + extra)
            }
        })
    }

    /// Lays the two panels out; `add` gets the side: 0 first, 1 second.
    pub fn show(self, ui: &mut Ui, mut add: impl FnMut(&mut Ui, usize)) {
        let Some((wa, wb)) = self.widths(ui.available_width()) else {
            add(ui, 0);
            ui.add_space(self.gap);
            add(ui, 1);
            return;
        };
        let h = if self.fit_height {
            ui.available_height().min(GROW)
        } else {
            GROW
        };
        ui.horizontal_top(|ui| {
            let sx = ui.spacing().item_spacing.x;
            for (side, w) in [(0, wa), (1, wb)] {
                if side == 1 {
                    ui.add_space((self.gap - sx).max(0.0));
                }
                ui.allocate_ui_with_layout(vec2(w, h), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(w);
                    add(ui, side);
                });
            }
        });
    }
}

/// A main column and a side column (inspector, builder, preview): side by
/// side, each scrolling, when there is room for both; stacked (side first)
/// in one scroll otherwise. `side` is skipped when `show_side` is false.
/// `state` is handed to both.
#[allow(clippy::too_many_arguments)]
pub fn split_scroll<S>(
    ui: &mut Ui,
    state: &mut S,
    id: &str,
    show_side: bool,
    side_w: f32,
    min_main: f32,
    main: impl FnOnce(&mut Ui, &mut S),
    side: impl FnOnce(&mut Ui, &mut S),
) {
    let gap = 16.0;
    let w = ui.available_width();
    if !show_side {
        egui::ScrollArea::vertical()
            .id_salt((id, "main"))
            .auto_shrink([false, false])
            .show(ui, |ui| main(ui, state));
        return;
    }
    if w >= min_main + gap + side_w {
        let h = ui.available_height();
        let main_w = w - gap - side_w;
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(vec2(main_w, h), Layout::top_down(Align::Min), |ui| {
                ui.set_min_size(vec2(main_w, h));
                egui::ScrollArea::vertical()
                    .id_salt((id, "main"))
                    .auto_shrink([false, false])
                    .show(ui, |ui| main(ui, state));
            });
            ui.add_space(gap - ui.spacing().item_spacing.x);
            ui.allocate_ui_with_layout(vec2(side_w, h), Layout::top_down(Align::Min), |ui| {
                ui.set_min_size(vec2(side_w, h));
                egui::ScrollArea::vertical()
                    .id_salt((id, "side"))
                    .auto_shrink([false, false])
                    .show(ui, |ui| side(ui, state));
            });
        });
    } else {
        egui::ScrollArea::vertical()
            .id_salt((id, "stack"))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                side(ui, state);
                ui.add_space(gap);
                main(ui, state);
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{DARK, LIGHT};

    #[test]
    fn status_colours_are_distinct_for_the_main_states() {
        for p in [DARK, LIGHT] {
            let live = status_color(&p, "live");
            let wait = status_color(&p, "waiting");
            let fail = status_color(&p, "failed");
            let off = status_color(&p, "ended");
            assert!(live != wait && wait != fail && fail != off && live != off);
        }
    }

    #[test]
    fn grid_columns_fit_the_width() {
        // 3 × 220 + 2 × 12 = 684 fits in 700; a fourth does not.
        assert_eq!(grid_columns(700.0, 220.0, 12.0, 10, true), (3, 225.0));
        // Never fewer than one column, even when narrower than min_w.
        assert_eq!(grid_columns(100.0, 220.0, 12.0, 10, true).0, 1);
        // A short row stretches, or keeps the full row's width.
        assert_eq!(grid_columns(700.0, 220.0, 12.0, 2, true), (2, 344.0));
        assert_eq!(grid_columns(700.0, 220.0, 12.0, 2, false), (3, 225.0));
        // No items still means one (full-width) column.
        assert_eq!(grid_columns(700.0, 220.0, 12.0, 0, true), (1, 700.0));
        // Cells and gaps never overflow the width.
        for w in [301.0, 517.3, 999.9, 1440.0] {
            let (c, cw) = grid_columns(w, 170.0, 12.0, 8, true);
            assert!(cw * c as f32 + 12.0 * (c - 1) as f32 <= w);
        }
    }

    #[test]
    fn split_widths_fill_the_row_or_stack() {
        let gap = 16.0;
        let s = |w| Split::new(300.0, 400.0, w);
        assert_eq!(s(Widths::Halves).widths(700.0), None);
        for widths in [
            Widths::First(1.0 / 3.0),
            Widths::Second(0.4),
            Widths::FirstClamped {
                share: 0.28,
                max: 320.0,
            },
            Widths::Flex,
        ] {
            let (a, b) = s(widths).widths(1000.0).unwrap();
            assert!(a >= 300.0 && b >= 400.0, "{widths:?}");
            assert!((a + gap + b - 1000.0).abs() < 1e-3, "{widths:?}");
        }
        assert_eq!(s(Widths::Halves).widths(1017.0), Some((500.0, 500.0)));
        assert_eq!(s(Widths::Flex).widths(1016.0), Some((450.0, 550.0)));
        assert_eq!(
            s(Widths::FirstClamped {
                share: 0.28,
                max: 320.0
            })
            .widths(2000.0)
            .unwrap()
            .0,
            320.0
        );
    }
}

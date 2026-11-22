//! Read-only view of a harness session's terminal (the Screen tab), and its
//! allowlisted key buttons. The harness's bytes are parsed by a VT emulator
//! into a cell grid and drawn as text; they never reach a real terminal.
//! OSC sequences (clipboard 52, titles 0/1/2, cwd 7, links 8) are dropped
//! before parsing. Keys go through the session inbox, gated and audited.

use crate::model::{Model, Notice};
use crate::theme::palette;
use crate::{icons, kit};
use egui::{Color32, FontId, RichText, text::LayoutJob};

/// Most bytes kept to replay.
pub const REPLAY_MAX: usize = 256 * 1024;

/// Removes every OSC sequence (`ESC ]` .. `BEL` or `ESC \`), keeping state
/// across chunks.
#[derive(Default)]
pub struct OscFilter {
    in_osc: bool,
    esc: bool,
}

impl OscFilter {
    pub fn filter(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        for &b in input {
            if self.in_osc {
                if b == 0x07 || (self.esc && b == b'\\') {
                    self.in_osc = false;
                }
                self.esc = b == 0x1b;
                continue;
            }
            if self.esc {
                self.esc = false;
                if b == b']' {
                    self.in_osc = true;
                    continue;
                }
                out.push(0x1b);
            }
            if b == 0x1b {
                self.esc = true;
                continue;
            }
            // C1 OSC.
            if b == 0x9d {
                self.in_osc = true;
                continue;
            }
            out.push(b);
        }
        out
    }
}

/// One harness terminal as a cell grid.
pub struct ScreenView {
    parser: vt100::Parser,
    osc: OscFilter,
    /// Stream offset just past the last byte fed.
    pub offset: u64,
}

impl Default for ScreenView {
    fn default() -> Self {
        Self::new(40, 120)
    }
}

impl ScreenView {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, 0),
            osc: OscFilter::default(),
            offset: 0,
        }
    }

    /// A viewer's first frame: the size and the replay.
    pub fn open(&mut self, rows: u16, cols: u16, replay: &[u8], offset: u64) {
        *self = Self::new(rows.clamp(5, 120), cols.clamp(20, 300));
        let tail = &replay[replay.len().saturating_sub(REPLAY_MAX)..];
        self.feed(tail, offset.saturating_sub(tail.len() as u64));
    }

    /// Feeds output that starts at stream offset `at`; bytes already seen
    /// are skipped.
    pub fn feed(&mut self, bytes: &[u8], at: u64) {
        let skip = self.offset.saturating_sub(at) as usize;
        if skip >= bytes.len() && at + (bytes.len() as u64) <= self.offset && self.offset > 0 {
            return;
        }
        let new = &bytes[skip.min(bytes.len())..];
        let clean = self.osc.filter(new);
        self.parser.process(&clean);
        self.offset = at + bytes.len() as u64;
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser
            .screen_mut()
            .set_size(rows.clamp(5, 120), cols.clamp(20, 300));
    }

    /// The screen as plain text, one line per row.
    pub fn text(&self) -> String {
        self.parser.screen().contents()
    }

    fn color(c: vt100::Color, default: Color32) -> Color32 {
        const BASE: [Color32; 16] = [
            Color32::from_rgb(0, 0, 0),
            Color32::from_rgb(205, 49, 49),
            Color32::from_rgb(13, 188, 121),
            Color32::from_rgb(229, 229, 16),
            Color32::from_rgb(36, 114, 200),
            Color32::from_rgb(188, 63, 188),
            Color32::from_rgb(17, 168, 205),
            Color32::from_rgb(229, 229, 229),
            Color32::from_rgb(102, 102, 102),
            Color32::from_rgb(241, 76, 76),
            Color32::from_rgb(35, 209, 139),
            Color32::from_rgb(245, 245, 67),
            Color32::from_rgb(59, 142, 234),
            Color32::from_rgb(214, 112, 214),
            Color32::from_rgb(41, 184, 219),
            Color32::from_rgb(255, 255, 255),
        ];
        match c {
            vt100::Color::Default => default,
            vt100::Color::Idx(i) if (i as usize) < 16 => BASE[i as usize],
            vt100::Color::Idx(i) if i >= 232 => {
                let v = 8 + (i - 232) * 10;
                Color32::from_rgb(v, v, v)
            }
            vt100::Color::Idx(i) => {
                let i = i - 16;
                let s = |x: u8| if x == 0 { 0 } else { 55 + x * 40 };
                Color32::from_rgb(s(i / 36), s((i / 6) % 6), s(i % 6))
            }
            vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        }
    }

    /// Draws the grid in monospace, read-only.
    pub fn ui(&self, ui: &mut egui::Ui) {
        let p = palette(ui.ctx());
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let font = FontId::monospace(12.0);
        let mut job = LayoutJob::default();
        for r in 0..rows {
            let mut run = String::new();
            let mut run_fmt: Option<egui::TextFormat> = None;
            for c in 0..cols {
                let Some(cell) = screen.cell(r, c) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let (mut fg, mut bg) = (
                    Self::color(cell.fgcolor(), p.text),
                    Self::color(cell.bgcolor(), Color32::TRANSPARENT),
                );
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                    if fg == Color32::TRANSPARENT {
                        fg = p.panel;
                    }
                }
                let fmt = egui::TextFormat {
                    font_id: font.clone(),
                    color: fg,
                    background: bg,
                    ..Default::default()
                };
                let s = cell.contents();
                let s = if s.is_empty() { " " } else { s };
                if run_fmt.as_ref() != Some(&fmt) {
                    if let Some(f) = run_fmt.take() {
                        job.append(&run, 0.0, f);
                    }
                    run.clear();
                    run_fmt = Some(fmt);
                }
                run.push_str(s);
            }
            if let Some(f) = run_fmt {
                job.append(&run, 0.0, f);
            }
            job.append(
                "\n",
                0.0,
                egui::TextFormat {
                    font_id: font.clone(),
                    ..Default::default()
                },
            );
        }
        egui::Frame::new()
            .fill(p.panel)
            .inner_margin(6.0)
            .show(ui, |ui| {
                egui::ScrollArea::both()
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(job)
                                .selectable(true)
                                .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    });
            });
    }
}

/// The key buttons for a harness's allowlist.
pub fn key_label(key: &str) -> String {
    match key {
        "enter" => "⏎".into(),
        "esc" => "Esc".into(),
        "up" => "↑".into(),
        "down" => "↓".into(),
        "left" => "←".into(),
        "right" => "→".into(),
        "tab" => "Tab".into(),
        "space" => "Space".into(),
        "backspace" => "⌫".into(),
        "ctrl-c" => "Ctrl-C".into(),
        k => k.to_uppercase(),
    }
}

/// Draws the key row and sends a pressed key through the inbox (kill
/// switch, allowlist and audit in `Core::harness_key`).
pub fn keys_ui(ui: &mut egui::Ui, model: &Model, session_id: &str, allow: &[String]) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(icons::label(icons::KEY, "Keys")).small());
        for k in allow {
            if kit::ghost_button(ui, &key_label(k)).clicked() {
                let (sid, key, allow) = (session_id.to_string(), k.clone(), allow.to_vec());
                model.act(move |m| async move {
                    m.core().harness_key(&sid, &key, &allow, "owner").await?;
                    Ok(Some(Notice::Info(format!("key {key} sent"))))
                });
            }
        }
    });
}

/// The keys `s`'s harness allows, from its node's advert.
pub fn allowed_keys(nodes: &[kls_store::Node], s: &kls_core::sessions::Session) -> Vec<String> {
    let Some(h) = kls_core::harness::session_harness(s) else {
        return Vec::new();
    };
    nodes
        .iter()
        .find(|n| n.id == s.node_id)
        .and_then(|n| n.host.harnesses.iter().find(|x| x.id == h))
        .map(|x| x.allow_keys.clone())
        .unwrap_or_default()
}

/// The native session an ended harness session can be resumed from: its
/// node still advertises the harness as launchable with resume, and the
/// session recorded the harness's own id.
pub fn resumable(
    nodes: &[kls_store::Node],
    s: &kls_core::sessions::Session,
    h: Option<&kls_store::belai::SessionHarness>,
) -> Option<String> {
    let h = h.filter(|h| !h.session_id.is_empty())?;
    let n = nodes.iter().find(|n| n.id == s.node_id)?;
    n.host
        .harnesses
        .iter()
        .any(|x| x.id == h.harness && x.launchable() && x.resume)
        .then(|| h.session_id.clone())
}

/// A Resume button for an ended harness session: a new dispatch of the same
/// harness in the same directory that resumes the native session (kill
/// switch, plan and audit in `Core::dispatch_harness`).
pub fn resume_ui(
    ui: &mut egui::Ui,
    model: &Model,
    nodes: &[kls_store::Node],
    s: &kls_core::sessions::Session,
    h: Option<&kls_store::belai::SessionHarness>,
) {
    let Some(native) = resumable(nodes, s, h) else {
        return;
    };
    let Some(harness) = h.map(|h| h.harness.clone()) else {
        return;
    };
    if ui
        .button(icons::label(icons::ARROW_COUNTER_CLOCKWISE, "Resume"))
        .clicked()
    {
        let (node, cwd) = (s.node_id.clone(), s.cwd.clone());
        model.act(move |m| async move {
            let n = m
                .core()
                .store()
                .node(&node)
                .await?
                .ok_or_else(|| anyhow::anyhow!("the node is gone"))?;
            let keys = kls_core::vault::list_secrets(m.core())
                .await
                .unwrap_or_default();
            let req = kls_core::harness::HarnessRequest {
                node_id: node,
                harness,
                cwd,
                resume: native,
                actor: "owner".into(),
                ..Default::default()
            };
            m.core()
                .dispatch_harness(&req, &n.host.harnesses, &keys, &m.hub)
                .await?;
            Ok(Some(Notice::Info("resume dispatched".into())))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc_sequences_are_dropped_across_chunks() {
        let mut f = OscFilter::default();
        let mut out = f.filter(b"a\x1b]52;c;aGVsbG8=");
        out.extend(f.filter(b"\x07b\x1b]0;title\x1b\\c\x1b[1md"));
        assert_eq!(out, b"abc\x1b[1md");
    }

    #[test]
    fn grid_renders_text_without_clipboard_or_title() {
        let mut s = ScreenView::new(5, 40);
        s.feed(
            b"\x1b]52;c;c2VjcmV0\x07\x1b]2;owned\x07hello \x1b[31mred\x1b[0m",
            0,
        );
        let t = s.text();
        assert!(t.starts_with("hello red"), "{t:?}");
        assert!(!t.contains("c2VjcmV0") && !t.contains("owned"));
    }

    #[test]
    fn replay_then_feed_skips_overlap() {
        let mut s = ScreenView::default();
        s.open(5, 40, b"abc", 3);
        s.feed(b"cde", 2);
        assert!(s.text().starts_with("abcde"), "{:?}", s.text());
    }

    #[test]
    fn key_labels() {
        assert_eq!(key_label("enter"), "⏎");
        assert_eq!(key_label("1"), "1");
    }
}

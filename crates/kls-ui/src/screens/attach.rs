//! The attach control (belai-counterparts #53): a local file path, stored
//! content-addressed by `kls_core::attachments::store_file`, shown as a chip
//! per attachment until the prompt or the start that carries it is sent.
//! Shared by the session composer and the New session dialog. Agent B.

use crate::icons;
use crate::kit;
use crate::model::Model;
use crate::theme::palette;
use egui::RichText;
use kls_core::attachments::{AttachmentRef, MAX_ATTACHMENTS};
use kls_core::hosts::Origin;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct AttachBox {
    path: String,
    items: Arc<Mutex<Vec<AttachmentRef>>>,
}

impl AttachBox {
    /// The attachments so far.
    pub fn list(&self) -> Vec<AttachmentRef> {
        self.items.lock().unwrap().clone()
    }

    /// Empties the box (after a send).
    pub fn clear(&mut self) {
        self.items.lock().unwrap().clear();
        self.path.clear();
    }

    /// The path field, the Attach button and one chip per attachment.
    /// `surface` names where it was attached (provenance).
    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model, surface: &str) {
        let p = palette(ui.ctx());
        let full = self.items.lock().unwrap().len() >= MAX_ATTACHMENTS;
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.path)
                    .hint_text("File path · PNG, JPEG, text")
                    .desired_width((ui.available_width() - 110.0).max(120.0)),
            );
            let go = ui.add_enabled(
                !full && !self.path.trim().is_empty(),
                egui::Button::new(icons::label(icons::UPLOAD_SIMPLE, "Attach")),
            );
            if go.clicked() {
                let path = std::path::PathBuf::from(self.path.trim());
                let items = self.items.clone();
                let origin = Origin {
                    actor: "owner".into(),
                    surface: surface.into(),
                    ..Default::default()
                };
                model.act(move |m| async move {
                    let a = kls_core::attachments::store_file(m.core(), &path, &origin).await?;
                    let mut l = items.lock().unwrap();
                    if !l.iter().any(|x| x.sha256 == a.sha256) {
                        l.push(a);
                    }
                    Ok(None)
                });
                self.path.clear();
            }
        });
        let mut drop = None;
        let items = self.items.lock().unwrap().clone();
        if !items.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for (i, a) in items.iter().enumerate() {
                    kit::chip(
                        ui,
                        &icons::label(icons::FILE_TEXT, &format!("{} · {}", a.name, size(a.size))),
                        p.muted,
                    )
                    .on_hover_text(format!(
                        "{} · sha256 {}",
                        a.mime,
                        &a.sha256[..12]
                    ));
                    if ui.small_button(icons::X).on_hover_text("Remove").clicked() {
                        drop = Some(i);
                    }
                }
                if full {
                    ui.label(RichText::new(format!("Max {MAX_ATTACHMENTS}")).color(p.amber));
                }
            });
        }
        if let Some(i) = drop {
            self.items.lock().unwrap().remove(i);
        }
    }
}

fn size(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MiB", n as f64 / (1 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{} KiB", n >> 10)
    } else {
        format!("{n} B")
    }
}

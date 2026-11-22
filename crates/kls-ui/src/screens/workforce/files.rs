//! Agent files in the builder (sf-parity "Builder extras: files"; the
//! website's "Agent files"): text files the agent carries, attached from a
//! local path under the path the profile lists them by, shipped with the
//! profile when it is published. Checks, storage and audit are in
//! `kls_core::attachments`. Agent B.

use crate::model::Model;
use crate::theme::palette;
use crate::{icons, kit};
use egui::RichText;
use kls_core::attachments::{AGENT_FILE_MAX_BYTES, AGENT_FILES_MAX, AGENT_FILES_TOTAL_MAX};
use kls_core::hosts::Origin;
use kls_core::kls_store::attach::AgentFile;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct FilesBox {
    /// The agent the list was loaded for.
    loaded_for: String,
    list: Arc<Mutex<Vec<AgentFile>>>,
    path: String,
    keep_as: String,
}

fn origin() -> Origin {
    Origin {
        actor: "owner".into(),
        surface: "workforce/builder/files".into(),
        ..Default::default()
    }
}

impl FilesBox {
    fn reload(&mut self, model: &Model, agent_id: &str) {
        self.loaded_for = agent_id.to_string();
        let (id, list) = (agent_id.to_string(), self.list.clone());
        model.act(move |m| async move {
            let got = kls_core::attachments::agent_files(m.core(), &id).await?;
            *list.lock().unwrap() = got;
            m.egui_ctx().request_repaint();
            Ok(None)
        });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model, agent_id: &str, width: f32) {
        let p = palette(ui.ctx());
        kit::field_label(ui, &icons::label(icons::FILE_TEXT, "Agent files"));
        if agent_id.is_empty() {
            ui.label(RichText::new("Save first").color(p.muted));
            return;
        }
        if self.loaded_for != agent_id {
            self.list.lock().unwrap().clear();
            self.reload(model, agent_id);
        }
        let files = self.list.lock().unwrap().clone();
        let total: i64 = files.iter().map(|f| f.size_bytes).sum();
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.path)
                    .hint_text("Local file")
                    .desired_width((width * 0.45).max(120.0)),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.keep_as)
                    .hint_text("Keep as (docs/guide.md)")
                    .desired_width((width * 0.3).max(100.0)),
            );
            let full = files.len() >= AGENT_FILES_MAX;
            if ui
                .add_enabled(
                    !full && !self.path.trim().is_empty(),
                    egui::Button::new(icons::label(icons::UPLOAD_SIMPLE, "Attach")),
                )
                .on_hover_text(format!(
                    "Text · max {} KiB each · {} files · {} MiB",
                    AGENT_FILE_MAX_BYTES >> 10,
                    AGENT_FILES_MAX,
                    AGENT_FILES_TOTAL_MAX >> 20
                ))
                .clicked()
            {
                let (id, file, keep) = (
                    agent_id.to_string(),
                    std::path::PathBuf::from(self.path.trim()),
                    self.keep_as.trim().to_string(),
                );
                let list = self.list.clone();
                model.act(move |m| async move {
                    kls_core::attachments::add_agent_file_from(
                        m.core(),
                        &id,
                        &file,
                        &keep,
                        &origin(),
                    )
                    .await?;
                    *list.lock().unwrap() =
                        kls_core::attachments::agent_files(m.core(), &id).await?;
                    Ok(None)
                });
                self.path.clear();
                self.keep_as.clear();
            }
        });
        if files.is_empty() {
            return;
        }
        let mut gone = None;
        for f in &files {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&f.path).monospace());
                ui.label(
                    RichText::new(format!("{} B · {}", f.size_bytes, &f.sha256[..12]))
                        .size(11.0)
                        .color(p.muted),
                )
                .on_hover_text(format!("Added by {}", f.added_by));
                if ui
                    .small_button(icons::TRASH)
                    .on_hover_text("Remove")
                    .clicked()
                {
                    gone = Some(f.path.clone());
                }
            });
        }
        ui.label(
            RichText::new(format!(
                "{}/{AGENT_FILES_MAX} · {} KiB · ships on publish",
                files.len(),
                total >> 10
            ))
            .size(11.0)
            .color(p.muted),
        );
        if let Some(path) = gone {
            let (id, list) = (agent_id.to_string(), self.list.clone());
            model.act(move |m| async move {
                kls_core::attachments::remove_agent_file(m.core(), &id, &path, &origin()).await?;
                *list.lock().unwrap() = kls_core::attachments::agent_files(m.core(), &id).await?;
                Ok(None)
            });
        }
    }
}

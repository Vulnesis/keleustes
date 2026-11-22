//! Settings > Project overrides: belai's per-project preferences on each
//! node, edited here and sent as a `project_prefs` dispatch. A change shows
//! as pending until the node reports matching values.

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme;
use kls_core::audit::overrides::{self as ov, ProjectRow};
use std::collections::BTreeMap;

/// One project being edited: (node, path) and the draft prefs.
struct Edit {
    node_id: String,
    path: String,
    /// Rows of (key, value); an emptied value clears the key.
    rows: Vec<(String, String)>,
    original: BTreeMap<String, String>,
    new_key: String,
    new_value: String,
}

pub struct Projects {
    data: Loader<Vec<ProjectRow>>,
    edit: Option<Edit>,
    filter: String,
}

impl Projects {
    pub fn new(model: &Model) -> Self {
        Self {
            data: Loader::new(
                model,
                &[
                    "host.inventory",
                    "settings.project_prefs",
                    "node.",
                    "factory.",
                ],
                |core| async move { ov::project_overrides(&core).await },
            ),
            edit: None,
            filter: String::new(),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let p = theme::palette(ui.ctx());
        let rows = match self.data.get() {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
                return;
            }
            Some(Ok(r)) => r,
        };
        if rows.is_empty() {
            kit::empty(ui, icons::FOLDER, "No projects reported by any node");
            return;
        }
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text(icons::label(icons::FUNNEL, "Filter"))
                .desired_width(240.0),
        );
        let f = self.filter.to_lowercase();
        for r in rows.iter().filter(|r| {
            f.is_empty()
                || r.path.to_lowercase().contains(&f)
                || r.repo.to_lowercase().contains(&f)
                || r.node_name.to_lowercase().contains(&f)
        }) {
            kit::hline_full(ui, p.line);
            ui.horizontal_wrapped(|ui| {
                kit::dot(ui, if r.online { p.green } else { p.muted });
                ui.label(egui::RichText::new(&r.node_name).strong());
                ui.label(kit::mono(&r.path, 12.0, p.muted));
                if !r.repo.is_empty() {
                    kit::tag(ui, &r.repo);
                }
                ui.label(
                    egui::RichText::new(format!("{} keys", r.prefs.len()))
                        .size(12.0)
                        .color(p.muted),
                )
                .on_hover_text(
                    r.prefs
                        .iter()
                        .map(|(k, v)| format!("{k} = {v}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                if let Some(c) = &r.last_change {
                    let (icon, text, color) = match c.state.as_str() {
                        "sent" => (icons::HOURGLASS, "Waiting for the node", p.gold_text),
                        "applied" => (icons::CHECK_CIRCLE, "Applied", p.green),
                        "not_applied" => (icons::WARNING, "Not applied", p.amber),
                        _ => (icons::PROHIBIT, "Refused", p.red),
                    };
                    kit::chip(ui, &icons::label(icon, text), color).on_hover_text(format!(
                        "{}{}",
                        fmt::when(c.at),
                        if c.reason.is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", c.reason)
                        }
                    ));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(r.editable && r.online, |ui| {
                        if kit::ghost_button(ui, &icons::label(icons::PENCIL_SIMPLE, "Edit"))
                            .on_disabled_hover_text(if r.editable {
                                "The node is offline"
                            } else {
                                "The host runs without --web-project-settings"
                            })
                            .clicked()
                        {
                            self.edit = Some(Edit {
                                node_id: r.node_id.clone(),
                                path: r.path.clone(),
                                rows: r
                                    .prefs
                                    .iter()
                                    .map(|(k, v)| (k.clone(), v.clone()))
                                    .collect(),
                                original: r.prefs.clone(),
                                new_key: String::new(),
                                new_value: String::new(),
                            });
                        }
                    });
                });
            });
        }
        self.edit_dialog(ui.ctx(), model);
    }

    fn edit_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(e) = &mut self.edit else {
            return;
        };
        let p = theme::palette(ctx);
        let mut open = true;
        let mut send = false;
        egui::Window::new(icons::label(icons::SLIDERS, "Project overrides"))
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(kit::mono(&e.path, 12.0, p.muted));
                egui::Grid::new("project_prefs")
                    .num_columns(2)
                    .show(ui, |ui| {
                        for (k, v) in e.rows.iter_mut() {
                            ui.label(kit::mono(k, 12.0, p.text));
                            ui.add(egui::TextEdit::singleline(v).hint_text("cleared"));
                            ui.end_row();
                        }
                        ui.add(egui::TextEdit::singleline(&mut e.new_key).hint_text("new.key"));
                        ui.add(egui::TextEdit::singleline(&mut e.new_value).hint_text("value"));
                        ui.end_row();
                    });
                ui.weak(icons::label(icons::INFO, "An empty value clears the key"));
                let (set, unset) = diff(e);
                ui.add_enabled_ui(!(set.is_empty() && unset.is_empty()), |ui| {
                    if theme::primary_button(
                        ui,
                        &icons::label(
                            icons::PAPER_PLANE_TILT,
                            &format!("Send {}", set.len() + unset.len()),
                        ),
                    )
                    .on_hover_text("Sent to the node; applied when it reports back")
                    .clicked()
                    {
                        send = true;
                    }
                });
            });
        if send {
            let (set, unset) = diff(e);
            let (node, path) = (e.node_id.clone(), e.path.clone());
            model.act(move |m| async move {
                ov::set_project_prefs(m.core(), &node, &path, &set, &unset, "owner").await?;
                Ok(Some(Notice::Info("Sent; waiting for the node".into())))
            });
            open = false;
        }
        if !open {
            self.edit = None;
        }
    }
}

/// What the draft changes: keys to set and keys to clear.
fn diff(e: &Edit) -> (BTreeMap<String, String>, Vec<String>) {
    let mut set = BTreeMap::new();
    let mut unset = Vec::new();
    for (k, v) in &e.rows {
        let v = v.trim();
        if v.is_empty() {
            unset.push(k.clone());
        } else if e.original.get(k).map(String::as_str) != Some(v) {
            set.insert(k.clone(), v.to_string());
        }
    }
    let nk = e.new_key.trim();
    if !nk.is_empty() && !e.new_value.trim().is_empty() {
        set.insert(nk.to_string(), e.new_value.trim().to_string());
    }
    (set, unset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_sets_changed_and_clears_emptied() {
        let original: BTreeMap<String, String> = [
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ]
        .into();
        let e = Edit {
            node_id: "n".into(),
            path: "p".into(),
            rows: vec![("a".into(), "1".into()), ("b".into(), "".into())],
            original,
            new_key: "c".into(),
            new_value: "3".into(),
        };
        let (set, unset) = diff(&e);
        assert_eq!(set.len(), 1);
        assert_eq!(set.get("c").map(String::as_str), Some("3"));
        assert_eq!(unset, vec!["b".to_string()]);
    }
}

//! Draft AGENTS.md (belai-counterparts row 54), opened from Repositories:
//! a host drafts the file, a person reads (and may edit) it here, and
//! Accept opens a pull request through the forge plane. The draft is model
//! text from a host and is only ever shown as plain text.

use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme::{palette, primary_button};
use kls_core::belai::initdraft::{self, Draft};
use std::sync::{Arc, Mutex};

/// Who reviews in the UI.
const BY: &str = "owner";

/// The drafts window for one repository.
pub struct Window {
    repo: Option<String>,
    node: String,
    nodes: Loader<Vec<(String, String)>>,
    drafts: Loader<Vec<Draft>>,
    /// The draft under review and its edited text.
    review: Option<(String, String)>,
    busy: Arc<Mutex<bool>>,
}

impl Window {
    pub fn new(model: &Model) -> Self {
        Self {
            repo: None,
            node: String::new(),
            nodes: Loader::new(model, &["node."], |core| async move {
                let online = match core.gateway().ok() {
                    Some(g) => g.online().await,
                    None => Vec::new(),
                };
                Ok(core
                    .store()
                    .nodes()
                    .await?
                    .into_iter()
                    .filter(|n| online.contains(&n.id) && initdraft::drafts_on(n))
                    .map(|n| (n.id, n.name))
                    .collect())
            }),
            drafts: Loader::new(model, &["init_draft."], |core| async move {
                Ok(initdraft::list(&core).await)
            }),
            review: None,
            busy: Arc::default(),
        }
    }

    /// Opens the window for `repo`.
    pub fn open(&mut self, repo: &str) {
        self.repo = Some(repo.to_string());
        self.review = None;
    }

    pub fn ui(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new(icons::label(icons::FILE_TEXT, "AGENTS.md"))
            .open(&mut open)
            .collapsible(false)
            .default_width(640.0)
            .show(ctx, |ui| self.body(ui, model, &repo));
        if !open {
            self.repo = None;
        }
    }

    fn body(&mut self, ui: &mut egui::Ui, model: &Model, repo: &str) {
        let p = palette(ui.ctx());
        ui.label(kit::mono(repo, 13.0, p.muted));
        let nodes = match self.nodes.get() {
            Some(Ok(n)) => n,
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        let busy = *self.busy.lock().unwrap();
        ui.horizontal(|ui| {
            if nodes.is_empty() {
                ui.label(icons::label(icons::WARNING, "No online host drafts"))
                    .on_hover_text("Needs a Belai host that lists init_draft");
                return;
            }
            if !nodes.iter().any(|(id, _)| *id == self.node) {
                self.node = nodes[0].0.clone();
            }
            let name = nodes
                .iter()
                .find(|(id, _)| *id == self.node)
                .map(|(_, n)| n.clone())
                .unwrap_or_default();
            egui::ComboBox::from_id_salt("init_draft_node")
                .selected_text(icons::label(icons::HARD_DRIVES, &name))
                .show_ui(ui, |ui| {
                    for (id, n) in &nodes {
                        ui.selectable_value(&mut self.node, id.clone(), n);
                    }
                });
            let draft = ui
                .add_enabled_ui(!busy, |ui| {
                    primary_button(ui, &icons::label(icons::ROBOT, "Draft"))
                })
                .inner;
            if busy {
                ui.spinner();
            }
            if draft.clicked() {
                let (node, repo, flag) = (self.node.clone(), repo.to_string(), self.busy.clone());
                *flag.lock().unwrap() = true;
                model.act(move |m| async move {
                    let r = initdraft::request(m.core(), &node, "", &repo, BY).await;
                    *flag.lock().unwrap() = false;
                    r?;
                    Ok(Some(Notice::Info("Draft ready".into())))
                });
            }
        });
        ui.separator();
        let drafts: Vec<Draft> = match self.drafts.get() {
            Some(Ok(d)) => d.into_iter().filter(|d| d.repo == repo).collect(),
            _ => Vec::new(),
        };
        if drafts.is_empty() {
            kit::empty(ui, icons::FILE_TEXT, "No drafts");
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("init_drafts")
            .max_height(480.0)
            .show(ui, |ui| {
                for d in &drafts {
                    self.draft_row(ui, model, d);
                    ui.add_space(6.0);
                }
            });
    }

    fn draft_row(&mut self, ui: &mut egui::Ui, model: &Model, d: &Draft) {
        let p = palette(ui.ctx());
        kit::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                kit::chip(ui, &d.status, kit::status_color(&p, &d.status));
                ui.label(icons::label(icons::HARD_DRIVES, &d.node_name));
                ui.label(kit::mono(&d.path, 12.0, p.muted));
                ui.label(kit::mono(
                    &d.sha256[..12.min(d.sha256.len())],
                    11.0,
                    p.muted,
                ))
                .on_hover_text(format!(
                    "sha256 {}\nby {} at {}",
                    d.sha256,
                    d.requested_by,
                    crate::fmt::when(d.requested_at)
                ));
                if !d.pr_url.is_empty() {
                    ui.hyperlink_to(
                        icons::label(icons::GIT_PULL_REQUEST, "Pull request"),
                        &d.pr_url,
                    );
                }
            });
            let reviewing = self.review.as_ref().is_some_and(|(id, _)| *id == d.id);
            if d.status != "pending" {
                return;
            }
            if !reviewing {
                if kit::ghost_button(ui, &icons::label(icons::EYE, "Review")).clicked() {
                    self.review = Some((d.id.clone(), d.markdown.clone()));
                }
                return;
            }
            if let Some((_, text)) = self.review.as_mut() {
                ui.add(
                    egui::TextEdit::multiline(text)
                        .code_editor()
                        .desired_rows(18)
                        .desired_width(f32::INFINITY),
                );
            }
            ui.horizontal(|ui| {
                if primary_button(ui, &icons::label(icons::GIT_PULL_REQUEST, "Accept")).clicked()
                    && let Some((id, text)) = self.review.take()
                {
                    let edited = (text != d.markdown).then_some(text);
                    model.act(move |m| async move {
                        let svc = kls_forge::service(m.core());
                        let d = kls_forge::forge::propose::accept_init_draft(
                            &svc,
                            m.core(),
                            &id,
                            edited.as_deref(),
                            BY,
                        )
                        .await?;
                        Ok(Some(Notice::Info(format!("Opened {}", d.pr_url))))
                    });
                }
                if kit::danger_button(ui, &icons::label(icons::X, "Reject")).clicked()
                    && let Some((id, _)) = self.review.take()
                {
                    model.act(move |m| async move {
                        initdraft::decide(m.core(), &id, "rejected", "", None, BY).await?;
                        Ok(Some(Notice::Info("Draft rejected".into())))
                    });
                }
                if kit::ghost_button(ui, &icons::label(icons::X, "Close")).clicked() {
                    self.review = None;
                }
            });
        });
    }
}

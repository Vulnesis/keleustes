//! Knowledge across hosts (belai-counterparts row 49), opened from Memory:
//! one search over every online host's index, each hit naming its host, and
//! a reindex per advertised index. Snippets are host text, shown as plain
//! text only.

use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme::{palette, primary_button};
use kls_core::belai::knowledge::{self, SearchReport};
use std::sync::{Arc, Mutex};

const BY: &str = "owner";
const SCOPES: [(&str, &str); 3] = [
    ("", "All"),
    ("profile", "Profiles"),
    ("project", "Projects"),
];

/// (node id, node name, [(scope, key, docs)]).
type NodeIndexes = (String, String, Vec<(String, String, i64)>);

pub struct Window {
    pub open: bool,
    query: String,
    scope: String,
    result: Arc<Mutex<Option<Result<SearchReport, String>>>>,
    busy: Arc<Mutex<bool>>,
    indexes: Loader<Vec<NodeIndexes>>,
}

impl Window {
    pub fn new(model: &Model) -> Self {
        Self {
            open: false,
            query: String::new(),
            scope: String::new(),
            result: Arc::default(),
            busy: Arc::default(),
            indexes: Loader::new(model, &["node."], |core| async move {
                let online = match core.gateway().ok() {
                    Some(g) => g.online().await,
                    None => Vec::new(),
                };
                Ok(core
                    .store()
                    .nodes()
                    .await?
                    .into_iter()
                    .filter(|n| online.contains(&n.id))
                    .map(|n| {
                        let ix = knowledge::indexes(&n);
                        (n.id, n.name, ix)
                    })
                    .filter(|(_, _, ix)| !ix.is_empty())
                    .collect())
            }),
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, model: &Model) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(icons::label(icons::BOOKS, "Knowledge"))
            .open(&mut open)
            .collapsible(false)
            .default_width(680.0)
            .show(ctx, |ui| self.body(ui, model));
        self.open = open;
    }

    fn body(&mut self, ui: &mut egui::Ui, model: &Model) {
        let p = palette(ui.ctx());
        let busy = *self.busy.lock().unwrap();
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text(icons::label(icons::MAGNIFYING_GLASS, "Search"))
                    .desired_width(320.0),
            );
            egui::ComboBox::from_id_salt("knowledge_scope")
                .selected_text(
                    SCOPES
                        .iter()
                        .find(|(k, _)| *k == self.scope)
                        .map(|(_, l)| *l)
                        .unwrap_or("All"),
                )
                .show_ui(ui, |ui| {
                    for (k, l) in SCOPES {
                        ui.selectable_value(&mut self.scope, k.to_string(), l);
                    }
                });
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let go = ui
                .add_enabled_ui(!busy && !self.query.trim().is_empty(), |ui| {
                    primary_button(ui, &icons::label(icons::MAGNIFYING_GLASS, "Search"))
                })
                .inner
                .clicked();
            if busy {
                ui.spinner();
            }
            if (go || enter) && !busy && !self.query.trim().is_empty() {
                let (q, scope) = (self.query.clone(), self.scope.clone());
                let (slot, flag) = (self.result.clone(), self.busy.clone());
                *flag.lock().unwrap() = true;
                model.act(move |m| async move {
                    let r = knowledge::search(m.core(), &q, &scope, "", 20, BY).await;
                    *flag.lock().unwrap() = false;
                    let refused = r.as_ref().ok().and_then(|r| r.refused.clone());
                    *slot.lock().unwrap() = Some(r.map_err(|e| format!("{e:#}")));
                    Ok(refused.map(Notice::Error))
                });
            }
        });
        if let Some(r) = self.result.lock().unwrap().clone() {
            match r {
                Err(e) => {
                    ui.colored_label(p.red, e);
                }
                Ok(rep) => {
                    ui.horizontal_wrapped(|ui| {
                        for (n, s) in &rep.nodes {
                            let c = if s == "ok" { p.green } else { p.red };
                            kit::chip(ui, &icons::label(icons::HARD_DRIVES, n), c).on_hover_text(s);
                        }
                        if rep.partial {
                            ui.label(icons::label(icons::WARNING, "Partial"));
                        }
                    });
                    if rep.hits.is_empty() {
                        kit::empty(ui, icons::MAGNIFYING_GLASS, "No hits");
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("knowledge_hits")
                        .max_height(360.0)
                        .show(ui, |ui| {
                            for h in &rep.hits {
                                kit::card(ui).show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        ui.label(icons::label(icons::HARD_DRIVES, &h.node_name));
                                        ui.label(kit::mono(&h.index, 12.0, p.muted));
                                        ui.label(kit::mono(
                                            &format!("{:.2}", h.score),
                                            12.0,
                                            p.muted,
                                        ));
                                    });
                                    ui.label(kit::mono(&h.address, 12.0, p.muted));
                                    // Host text: plain label, never markup.
                                    ui.label(egui::RichText::new(&h.snippet));
                                });
                                ui.add_space(4.0);
                            }
                        });
                }
            }
        }
        ui.separator();
        ui.label(kit::caps("Indexes", p.gold_text));
        match self.indexes.get() {
            Some(Ok(nodes)) if nodes.is_empty() => {
                kit::empty(ui, icons::BOOKS, "No host lists an index");
            }
            Some(Ok(nodes)) => {
                for (id, name, ixs) in nodes {
                    for (scope, key, docs) in ixs {
                        ui.horizontal(|ui| {
                            ui.label(icons::label(icons::HARD_DRIVES, &name));
                            ui.label(kit::mono(&format!("{scope}:{key}"), 12.0, p.muted));
                            ui.label(format!("{docs} docs"));
                            if kit::ghost_button(
                                ui,
                                &icons::label(icons::ARROWS_CLOCKWISE, "Reindex"),
                            )
                            .on_hover_text("The host re-ingests and classifies")
                            .clicked()
                            {
                                let (id, scope, key) = (id.clone(), scope.clone(), key.clone());
                                model.act(move |m| async move {
                                    knowledge::reindex(m.core(), &id, &scope, &key, BY).await?;
                                    Ok(Some(Notice::Info("Reindex sent".into())))
                                });
                            }
                        });
                    }
                }
            }
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
            }
            None => {
                ui.spinner();
            }
        }
    }
}

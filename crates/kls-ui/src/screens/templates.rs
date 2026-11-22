//! Templates screen: the built-in catalogue, a preview of each graph, use
//! (copy into a new draft workflow) and dismiss. Owner: M3 engine.

use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::screens::workflows;
use crate::theme::{self, palette};
use egui::{RichText, Stroke};
use kls_engine::templates::{self, CATEGORIES, Template};
use std::collections::HashMap;
use std::sync::Arc;

pub const READY: bool = true;

pub struct Screen {
    list: Loader<Arc<Vec<(Template, bool)>>>,
    selected: Option<String>,
    category: Option<&'static str>,
    show_dismissed: bool,
    name: String,
    repos: String,
    q: String,
    budget: String,
    /// Templates the last 7 days of unanswered GitHub events point at.
    suggested: Loader<Vec<kls_engine::delivery::Suggestion>>,
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        let list = Loader::new(model, &["template."], |core| async move {
            Ok(Arc::new(templates::template_list(&core).await?))
        });
        Self {
            list,
            selected: None,
            category: None,
            show_dismissed: false,
            name: String::new(),
            repos: String::new(),
            q: String::new(),
            budget: String::new(),
            suggested: Loader::new(model, &["github.", "schedule.fired"], |core| async move {
                let since = kls_store::now_ms() - 7 * 86_400_000;
                let u = kls_engine::delivery::unanswered(&core, since, 5_000).await?;
                Ok(kls_engine::delivery::rank_templates(&u))
            }),
        }
    }

    pub fn focus(&mut self, id: &str) {
        self.select(id);
    }

    fn select(&mut self, id: &str) {
        if self.selected.as_deref() != Some(id) {
            self.selected = Some(id.to_string());
            self.name = templates::template(id)
                .map(|t| t.name.clone())
                .unwrap_or_default();
            self.repos.clear();
            self.budget.clear();
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        kit::page_header(ui, icons::STACK, "Templates", |_| {});
        self.filter_strip(ui);
        if let Some(Ok(s)) = self.suggested.get()
            && !s.is_empty()
        {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(kit::caps(
                    &icons::label(icons::LIGHTNING, "Suggested"),
                    palette(ui.ctx()).gold_text,
                ));
                for sg in s.iter().take(4) {
                    let name = templates::template(&sg.template_id)
                        .map(|t| t.name.clone())
                        .unwrap_or_default();
                    if kit::toggle_chip(
                        ui,
                        &format!("{name} · {}", sg.score),
                        self.selected.as_deref() == Some(&sg.template_id),
                    )
                    .on_hover_text(sg.reasons.join(", "))
                    .clicked()
                    {
                        self.select(&sg.template_id.clone());
                    }
                }
            });
        }
        ui.add_space(16.0);
        let items = match self.list.get() {
            Some(Ok(i)) => i,
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        let q = self.q.trim().to_lowercase();
        let shown: Vec<(Template, bool)> = items
            .iter()
            .filter(|(t, d)| {
                (self.show_dismissed || !d) && self.category.is_none_or(|c| t.category == c)
            })
            .filter(|(t, _)| q.is_empty() || template_matches(t, &q))
            .cloned()
            .collect();
        if self.selected.is_none()
            && let Some((t, _)) = shown.first()
        {
            self.select(&t.id.clone());
        }
        let current = self
            .selected
            .clone()
            .and_then(|id| items.iter().find(|(t, _)| t.id == id).cloned());

        let side_w = (ui.available_width() * 0.38).clamp(340.0, 520.0);
        kit::split_scroll(
            ui,
            self,
            "templates",
            current.is_some(),
            side_w,
            480.0,
            |ui, s| s.grid_ui(ui, &shown),
            |ui, s| {
                if let Some((t, dismissed)) = &current {
                    kit::glow(ui)
                        .inner_margin(egui::Margin::same(16))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            s.preview_ui(ui, model, t, *dismissed);
                        });
                }
            },
        );
    }

    /// Category pills and the dismissed toggle, in a gold-edged strip.
    fn filter_strip(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.ctx());
        kit::card(ui)
            .stroke(Stroke::new(1.0, p.gold_edge))
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(kit::caps(
                        &icons::label(icons::TAG, "Category"),
                        p.gold_text,
                    ));
                    if kit::pill(ui, "All", self.category.is_none()).clicked() {
                        self.category = None;
                    }
                    for c in CATEGORIES {
                        if kit::pill(ui, c, self.category == Some(c)).clicked() {
                            self.category = Some(c);
                        }
                    }
                    ui.add_space(8.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.q)
                            .hint_text(icons::label(icons::MAGNIFYING_GLASS, "Search"))
                            .desired_width(180.0),
                    );
                    ui.checkbox(
                        &mut self.show_dismissed,
                        icons::label(icons::EYE_SLASH, "Dismissed"),
                    );
                });
            });
    }

    fn grid_ui(&mut self, ui: &mut egui::Ui, shown: &[(Template, bool)]) {
        if shown.is_empty() {
            kit::empty(ui, icons::TRAY, "No templates");
            return;
        }
        kit::Grid::new(240.0)
            .keep_width()
            .tight_rows()
            .show(ui, shown.len(), |ui, i, _| {
                let (t, dismissed) = &shown[i];
                let sel = self.selected.as_deref() == Some(t.id.as_str());
                if template_card(ui, t, *dismissed, sel).clicked() {
                    self.select(&t.id);
                }
            });
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui, model: &Model, t: &Template, dismissed: bool) {
        let p = palette(ui.ctx());
        ui.label(kit::caps("Preview", p.gold_text));
        ui.add(egui::Label::new(RichText::new(&t.name).size(20.0).strong()).wrap());
        let (lo, hi) = t.tokens_per_run();
        ui.horizontal_wrapped(|ui| {
            let meta = |ui: &mut egui::Ui, icon: &str, text: &str, hover: &str| {
                ui.label(
                    RichText::new(icons::label(icon, text))
                        .monospace()
                        .size(12.0)
                        .color(p.muted),
                )
                .on_hover_text(hover);
            };
            meta(ui, icons::TAG, &t.category, "Category");
            meta(ui, icons::LIGHTNING, &t.trigger.label, "Trigger");
            meta(ui, icons::EXPORT, &t.output, "Output");
            meta(ui, icons::COINS, &format!("{lo}–{hi}"), "Tokens per run");
            if !t.requires.crews.is_empty() {
                meta(
                    ui,
                    icons::USERS_THREE,
                    &t.requires.crews.join(", "),
                    "Crews",
                );
            }
        });
        ui.add_space(4.0);
        ui.label(&t.description);
        if t.requires.integrations.iter().any(|i| i == "github") {
            kit::chip(
                ui,
                &icons::label(icons::WARNING, "GitHub required"),
                p.amber,
            )
            .on_hover_text("Its GitHub trigger and steps do nothing until GitHub is connected.");
        }
        ui.add_space(8.0);
        workflows::preview(ui, &t.graph, &HashMap::new(), 200.0);
        ui.add_space(10.0);
        ui.label(kit::caps(
            &icons::label(icons::LIST_CHECKS, "Steps"),
            p.muted,
        ));
        for (i, n) in t.graph.nodes.iter().enumerate() {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("{}.", i + 1))
                        .monospace()
                        .color(p.muted),
                );
                ui.label(RichText::new(&n.title).size(13.0));
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        workflows::type_label(&n.kind),
                        n.summary()
                    ))
                    .size(12.0)
                    .color(p.muted),
                );
            });
        }
        ui.add_space(10.0);
        kit::field_label(ui, "Name");
        ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(f32::INFINITY));
        ui.add_space(4.0);
        kit::field_label(ui, "Repos");
        ui.add(
            egui::TextEdit::singleline(&mut self.repos)
                .hint_text("owner/name, owner/*")
                .desired_width(f32::INFINITY),
        )
        .on_hover_text("Optional, comma-separated");
        ui.add_space(4.0);
        kit::field_label(ui, "Run budget, tokens");
        ui.add(
            egui::TextEdit::singleline(&mut self.budget)
                .hint_text("template default")
                .desired_width(f32::INFINITY),
        );
        let budget: Option<i64> = self
            .budget
            .trim()
            .replace([',', '_'], "")
            .parse()
            .ok()
            .filter(|b: &i64| *b > 0);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if theme::primary_button(ui, &icons::label(icons::COPY, "Use template")).clicked() {
                let (id, name) = (t.id.clone(), self.name.clone());
                let repos: Vec<String> = self
                    .repos
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                model.act(move |m| async move {
                    let w =
                        templates::use_template(m.core(), &id, Some(&name), repos, budget).await?;
                    m.go(crate::Screen::Workflows, Some(w.id));
                    Ok(Some(Notice::Info(format!(
                        "{} is a draft; publish it to turn it on",
                        w.name
                    ))))
                });
            }
            let label = if dismissed {
                icons::label(icons::ARROW_COUNTER_CLOCKWISE, "Restore")
            } else {
                icons::label(icons::EYE_SLASH, "Dismiss")
            };
            if kit::ghost_button(ui, &label).clicked() {
                let id = t.id.clone();
                model.act(move |m| async move {
                    templates::set_template_dismissed(m.core(), &id, !dismissed).await?;
                    Ok(None)
                });
                if !dismissed && !self.show_dismissed {
                    self.selected = None;
                }
            }
        });
    }
}

/// One catalogue card: category and badges in mono on top, the name, and
/// the trigger and flow underneath.
fn template_card(
    ui: &mut egui::Ui,
    t: &Template,
    dismissed: bool,
    selected: bool,
) -> egui::Response {
    let p = palette(ui.ctx());
    kit::tile(ui, selected, |ui| {
        ui.set_min_height(92.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(icons::label(icons::TAG, &t.category))
                    .monospace()
                    .size(11.0)
                    .color(p.muted),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if dismissed {
                    ui.label(
                        RichText::new(icons::label(icons::EYE_SLASH, "dismissed"))
                            .monospace()
                            .size(11.0)
                            .color(p.muted),
                    );
                }
                for b in &t.badges {
                    ui.label(RichText::new(b).monospace().size(11.0).color(p.gold_text));
                }
            });
        });
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(&t.name).size(15.0).strong()).wrap());
        ui.add_space(2.0);
        ui.add(
            egui::Label::new(
                RichText::new(icons::label(icons::LIGHTNING, &t.trigger.label))
                    .size(12.0)
                    .color(p.muted),
            )
            .truncate(),
        );
        ui.add(
            egui::Label::new(RichText::new(t.flow.join(" → ")).size(12.0).color(p.muted))
                .truncate(),
        );
    })
}

/// Search over name, description, id, category, flow and output; `ci` also
/// finds check runs and test failures.
fn template_matches(t: &Template, q: &str) -> bool {
    let hay = format!(
        "{} {} {} {} {} {}",
        t.name,
        t.description,
        t.id,
        t.category,
        t.flow.join(" "),
        t.output
    )
    .to_lowercase();
    let synonyms: &[&str] = match q {
        "ci" => &["check", "ci", "test"],
        "pr" => &["pull request", "pr"],
        "deps" | "dependencies" => &["depend", "upgrade"],
        _ => &[],
    };
    hay.contains(q) || synonyms.iter().any(|s| hay.contains(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_with_synonyms() {
        let t = templates::template("ci-failure-fixer").unwrap();
        assert!(template_matches(t, "ci"));
        assert!(template_matches(t, "bug"));
        assert!(!template_matches(t, "zzz"));
    }
}

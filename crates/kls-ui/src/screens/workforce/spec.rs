//! The agent builder's typed sections: behaviour, protections, handoffs,
//! workspace, budget and memory. They edit a belai [`ProfileSpec`] that is
//! saved as the profile's `meta_json` (the format a node reads).

use crate::{icons, kit, theme::palette};
use egui::RichText;
use kls_engine::catalogue::{self, AUTONOMY, CLAIM_LISTS, EFFORT, ISOLATION, PROFILE_MODES};
use kls_engine::catalogue::{
    BudgetSpec, CardMove, KanbanSpec, KnowledgeSpec, MemorySpec, ProfileSpec, WorkspaceSpec,
};

/// belai's checkpoint reasons a handoff can hold for.
pub const CHECKPOINTS: [&str; 5] = ["", "plan_approval", "root_cause", "visual", "pre_pr"];

fn join(v: &[String]) -> String {
    v.join(", ")
}

fn split(s: &str) -> Vec<String> {
    s.split([',', '\n'])
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

fn num<T: std::str::FromStr>(s: &str) -> Option<T> {
    let t = s.trim();
    if t.is_empty() { None } else { t.parse().ok() }
}

/// Text buffers over a [`ProfileSpec`].
#[derive(Clone, Debug, Default)]
pub struct SpecForm {
    pub spec: ProfileSpec,
    lists: Vec<String>,
    labels: String,
    ok_list: String,
    ok_labels: String,
    ok_drop: String,
    fail_list: String,
    fail_labels: String,
    fail_drop: String,
    handoff_to: String,
    handoff_labels: String,
    checkpoint: String,
    max_attempts: String,
    lease: String,
    max_cost: String,
    finalize_at: String,
    passes: String,
    wall: String,
    knowledge: String,
    /// Fields that do not parse.
    bad: Vec<String>,
}

impl SpecForm {
    pub fn from_meta(meta_json: &str) -> Self {
        Self::from_spec(ProfileSpec::parse(meta_json))
    }

    pub fn from_spec(spec: ProfileSpec) -> Self {
        let k = spec.kanban.clone().unwrap_or_default();
        let ok = k.on_success.clone().unwrap_or_default();
        let fail = k.on_failure.clone().unwrap_or_default();
        let b = spec.budget.clone().unwrap_or_default();
        Self {
            lists: k.lists.clone(),
            labels: join(&k.labels),
            ok_list: ok.list,
            ok_labels: join(&ok.labels),
            ok_drop: join(&ok.drop_labels),
            fail_list: fail.list,
            fail_labels: join(&fail.labels),
            fail_drop: join(&fail.drop_labels),
            handoff_to: join(&k.handoff_to),
            handoff_labels: join(&k.handoff_labels),
            checkpoint: k.handoff_checkpoint.clone(),
            max_attempts: if k.max_attempts > 0 {
                k.max_attempts.to_string()
            } else {
                String::new()
            },
            lease: k.lease.clone(),
            max_cost: b
                .max_cost_usd_per_item
                .map(|v| v.to_string())
                .unwrap_or_default(),
            finalize_at: b.finalize_at.map(|v| v.to_string()).unwrap_or_default(),
            passes: b
                .max_passes_per_item
                .map(|v| v.to_string())
                .unwrap_or_default(),
            wall: b.max_wall_per_item.clone(),
            knowledge: spec
                .knowledge
                .as_ref()
                .map(|k| join(&k.paths))
                .unwrap_or_default(),
            spec,
            bad: Vec::new(),
        }
    }

    /// The spec with the buffers applied.
    pub fn build(&mut self) -> ProfileSpec {
        self.bad.clear();
        let mut s = self.spec.clone();
        if s.mode == "worker" || !self.lists.is_empty() || !self.labels.trim().is_empty() {
            let mut k: KanbanSpec = s.kanban.clone().unwrap_or_default();
            k.lists = self.lists.clone();
            k.labels = split(&self.labels);
            let mv = |list: &str, labels: &str, drop: &str| {
                let m = CardMove {
                    list: list.trim().to_string(),
                    labels: split(labels),
                    drop_labels: split(drop),
                };
                (m != CardMove::default()).then_some(m)
            };
            k.on_success = mv(&self.ok_list, &self.ok_labels, &self.ok_drop);
            k.on_failure = mv(&self.fail_list, &self.fail_labels, &self.fail_drop);
            k.handoff_to = split(&self.handoff_to);
            k.handoff_labels = split(&self.handoff_labels);
            k.handoff_checkpoint = self.checkpoint.clone();
            k.max_attempts = match num::<i64>(&self.max_attempts) {
                Some(n) => n,
                None if self.max_attempts.trim().is_empty() => 0,
                None => {
                    self.bad.push("Max attempts: a number".into());
                    0
                }
            };
            k.lease = self.lease.trim().to_string();
            s.kanban = Some(k);
        } else {
            s.kanban = None;
        }
        let mut b: BudgetSpec = s.budget.clone().unwrap_or_default();
        let mut f64_field = |text: &str, label: &str| -> Option<f64> {
            match num::<f64>(text) {
                Some(v) => Some(v),
                None if text.trim().is_empty() => None,
                None => {
                    self.bad.push(format!("{label}: a number"));
                    None
                }
            }
        };
        b.max_cost_usd_per_item = f64_field(&self.max_cost, "Cost per item");
        b.finalize_at = f64_field(&self.finalize_at, "Finalize at");
        b.max_passes_per_item = num::<i64>(&self.passes);
        if !self.passes.trim().is_empty() && b.max_passes_per_item.is_none() {
            self.bad.push("Passes per item: a number".into());
        }
        b.max_wall_per_item = self.wall.trim().to_string();
        s.budget = (b != BudgetSpec::default()).then_some(b);
        let paths = split(&self.knowledge);
        s.knowledge = (!paths.is_empty()).then_some(KnowledgeSpec { paths });
        s
    }

    /// The `meta_json` to save: the spec without the fields the profile
    /// row already holds (name, prompt, tools, provider, model ...).
    pub fn meta_json(&mut self) -> String {
        let mut s = self.build();
        s.name.clear();
        s.description.clear();
        s.display_name.clear();
        s.system_prompt.clear();
        s.tools.clear();
        s.provider.clear();
        s.model.clear();
        s.to_json()
    }

    /// Blocking problems (belai's rules plus unparsable fields).
    pub fn problems(&mut self) -> Vec<String> {
        let s = self.build();
        let mut out = self.bad.clone();
        out.extend(s.problems());
        out
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let full = ui.available_width();
        let p = palette(ui.ctx());
        let s = &mut self.spec;

        sub(ui, icons::SLIDERS, "Behaviour");
        ui.horizontal_wrapped(|ui| {
            combo(ui, "spec_mode", "Mode", &mut s.mode, &PROFILE_MODES);
            combo(ui, "spec_autonomy", "Autonomy", &mut s.autonomy, &AUTONOMY);
            combo(ui, "spec_effort", "Effort", &mut s.effort, &EFFORT);
            kit::field(ui, "Max iterations", |ui| {
                ui.add(egui::DragValue::new(&mut s.max_iterations).range(0..=1000))
            });
            ui.checkbox(&mut s.reflection, "Reflection");
        });
        if s.mode == "schedule" {
            text(ui, "Schedule", &mut s.schedule, "0 9 * * 1-5", full);
        }
        if s.mode == "monitor" {
            text(ui, "Monitor condition", &mut s.monitor_condition, "", full);
        }

        sub(ui, icons::SHIELD_CHECK, "Protections");
        ui.horizontal_wrapped(|ui| {
            tristate(ui, "spec_guard", "Guardrails", &mut s.guardrails);
            tristate(ui, "spec_ask", "Ask permission", &mut s.ask_permission);
        });

        if s.mode == "worker" {
            sub(ui, icons::KANBAN, "Handoffs");
            kit::field_label(ui, "Claims from");
            ui.horizontal_wrapped(|ui| {
                for l in CLAIM_LISTS {
                    let mut on = self.lists.iter().any(|x| x == l);
                    if ui.checkbox(&mut on, l).changed() {
                        if on {
                            self.lists.push(l.to_string());
                        } else {
                            self.lists.retain(|x| x != l);
                        }
                    }
                }
            });
            text(ui, "Claim labels", &mut self.labels, "build", full);
            ui.columns(2, |c| {
                kit::field_label(&mut c[0], &icons::label(icons::CHECK_CIRCLE, "On success"));
                list_combo(&mut c[0], "spec_ok_list", &mut self.ok_list);
                text(
                    &mut c[0],
                    "Add labels",
                    &mut self.ok_labels,
                    "needs-review",
                    0.0,
                );
                text(&mut c[0], "Drop labels", &mut self.ok_drop, "build", 0.0);
                kit::field_label(&mut c[1], &icons::label(icons::X_CIRCLE, "On failure"));
                list_combo(&mut c[1], "spec_fail_list", &mut self.fail_list);
                text(&mut c[1], "Add labels", &mut self.fail_labels, "", 0.0);
                text(&mut c[1], "Drop labels", &mut self.fail_drop, "", 0.0);
            });
            text(ui, "Hands to", &mut self.handoff_to, "belai:builder", full);
            text(
                ui,
                "Handoff labels",
                &mut self.handoff_labels,
                "build",
                full,
            );
            ui.horizontal_wrapped(|ui| {
                combo(
                    ui,
                    "spec_checkpoint",
                    "Hold for",
                    &mut self.checkpoint,
                    &CHECKPOINTS,
                );
                small(ui, "Max attempts", &mut self.max_attempts, "3");
                small(ui, "Lease", &mut self.lease, "30m");
            });
        }

        sub(ui, icons::FOLDER, "Workspace");
        let w = s.workspace.get_or_insert_with(WorkspaceSpec::default);
        ui.horizontal_wrapped(|ui| {
            combo(ui, "spec_iso", "Isolation", &mut w.isolation, &ISOLATION);
            combo(
                ui,
                "spec_publish",
                "Publish",
                &mut w.publish,
                &["agent", "none"],
            );
            ui.checkbox(
                &mut w.read_only,
                icons::label(icons::LOCK_SIMPLE, "Read only"),
            );
        });
        if *w == WorkspaceSpec::default() {
            s.workspace = None;
        }

        sub(ui, icons::COINS, "Budget and memory");
        ui.horizontal_wrapped(|ui| {
            small(ui, "Cost per item, USD", &mut self.max_cost, "3");
            small(ui, "Finalize at", &mut self.finalize_at, "0.85");
            small(ui, "Passes per item", &mut self.passes, "20");
            small(ui, "Wall per item", &mut self.wall, "2h");
        });
        let mut mem = s.memory.as_ref().is_some_and(|m| m.enabled);
        if ui
            .checkbox(&mut mem, icons::label(icons::BRAIN, "Memory"))
            .changed()
        {
            s.memory = mem.then_some(MemorySpec { enabled: true });
        }
        text(
            ui,
            "Knowledge paths",
            &mut self.knowledge,
            ".vulnetix",
            full,
        );
        if !s.extra.is_empty() {
            ui.label(
                RichText::new(icons::label(
                    icons::INFO,
                    &format!(
                        "Kept: {}",
                        s.extra.keys().cloned().collect::<Vec<_>>().join(", ")
                    ),
                ))
                .color(p.muted),
            );
        }
    }
}

fn sub(ui: &mut egui::Ui, icon: &str, title: &str) {
    ui.add_space(10.0);
    ui.label(kit::caps(
        &icons::label(icon, title),
        palette(ui.ctx()).muted,
    ));
    ui.add_space(2.0);
}

fn text(ui: &mut egui::Ui, label: &str, v: &mut String, hint: &str, width: f32) {
    kit::field_label(ui, label);
    let w = if width > 0.0 {
        width
    } else {
        ui.available_width()
    };
    ui.add(
        egui::TextEdit::singleline(v)
            .hint_text(hint)
            .desired_width(w),
    );
}

fn small(ui: &mut egui::Ui, label: &str, v: &mut String, hint: &str) {
    kit::field(ui, label, |ui| {
        ui.add(
            egui::TextEdit::singleline(v)
                .hint_text(hint)
                .desired_width(90.0),
        )
    });
}

fn combo(ui: &mut egui::Ui, id: &str, label: &str, v: &mut String, opts: &[&str]) {
    kit::field(ui, label, |ui| {
        egui::ComboBox::from_id_salt(id)
            .selected_text(if v.is_empty() { "default" } else { v.as_str() })
            .show_ui(ui, |ui| {
                ui.selectable_value(v, String::new(), "default");
                for o in opts.iter().filter(|o| !o.is_empty()) {
                    ui.selectable_value(v, o.to_string(), *o);
                }
            })
    });
}

fn list_combo(ui: &mut egui::Ui, id: &str, v: &mut String) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(if v.is_empty() { "stay" } else { v.as_str() })
        .show_ui(ui, |ui| {
            ui.selectable_value(v, String::new(), "stay");
            for l in CLAIM_LISTS {
                ui.selectable_value(v, l.to_string(), l);
            }
        });
}

fn tristate(ui: &mut egui::Ui, id: &str, label: &str, v: &mut Option<bool>) {
    kit::field(ui, label, |ui| {
        let text = match v {
            None => "default",
            Some(true) => "on",
            Some(false) => "off",
        };
        egui::ComboBox::from_id_salt(id)
            .selected_text(text)
            .show_ui(ui, |ui| {
                ui.selectable_value(v, None, "default");
                ui.selectable_value(v, Some(true), "on");
                ui.selectable_value(v, Some(false), "off");
            })
    });
}

/// A builtin to start from; `Some` when one was picked.
pub fn builtin_picker(ui: &mut egui::Ui) -> Option<&'static ProfileSpec> {
    let mut out = None;
    ui.menu_button(icons::label(icons::BOOKS, "From builtin"), |ui| {
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .show(ui, |ui| {
                for p in catalogue::profiles().iter().filter(|p| p.is_worker()) {
                    let label = if p.display_name.is_empty() {
                        p.name.clone()
                    } else {
                        format!("{} · {}", p.name, p.display_name)
                    };
                    if ui.button(label).on_hover_text(&p.description).clicked() {
                        out = Some(p);
                        ui.close();
                    }
                }
            });
    })
    .response
    .on_hover_text(catalogue::SOURCE);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_round_trips_through_the_form() {
        for p in catalogue::profiles() {
            let mut f = SpecForm::from_spec(p.clone());
            assert!(f.problems().is_empty(), "{}: {:?}", p.name, f.problems());
            assert_eq!(&f.build(), p, "{}", p.name);
            let meta = f.meta_json();
            let back = ProfileSpec::parse(&meta);
            assert!(back.system_prompt.is_empty() && back.name.is_empty());
            assert_eq!(back.kanban, p.kanban, "{}", p.name);
        }
    }

    #[test]
    fn bad_numbers_block() {
        let mut f = SpecForm::from_meta(r#"{"mode":"worker"}"#);
        f.lists = vec!["backlog".into()];
        f.labels = "fix".into();
        f.max_attempts = "three".into();
        f.max_cost = "x".into();
        let pr = f.problems();
        assert!(pr.iter().any(|x| x.starts_with("Max attempts")));
        assert!(pr.iter().any(|x| x.starts_with("Cost per item")));
        f.max_attempts = "3".into();
        f.max_cost = "2.5".into();
        assert!(f.problems().is_empty(), "{:?}", f.problems());
        let s = f.build();
        assert_eq!(s.kanban.unwrap().max_attempts, 3);
        assert_eq!(s.budget.unwrap().max_cost_usd_per_item, Some(2.5));
    }
}

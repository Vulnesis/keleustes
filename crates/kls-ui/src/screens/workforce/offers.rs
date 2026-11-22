//! Agent builder: per-field draft offers (Take / Drop / Take all / Undo)
//! and autosave of the unsaved builder ("Continue your draft").

use crate::{icons, kit, theme::palette};
use egui::RichText;
use kls_core::vault::{AgentDraft, AgentProfile};

/// Settings key the unsaved builder is kept under.
pub const DRAFT_KEY: &str = "workforce.builder.draft";
/// Autosave at most this often while editing.
pub const AUTOSAVE_MS: i64 = 2_000;

/// The fields a draft offers, in form order.
pub const FIELDS: [&str; 7] = [
    "name",
    "display_name",
    "description",
    "prompt",
    "provider",
    "model",
    "tools",
];

pub fn offered(d: &AgentDraft, field: &str) -> String {
    match field {
        "name" => d.name.clone(),
        "display_name" => d.display_name.clone(),
        "description" => d.description.clone(),
        "prompt" => d.prompt.clone(),
        "provider" => d.provider.clone(),
        "model" => d.model.clone(),
        "tools" => d.tools.join(", "),
        _ => String::new(),
    }
}

pub fn current(p: &AgentProfile, tools: &str, field: &str) -> String {
    match field {
        "name" => p.name.clone(),
        "display_name" => p.display_name.clone(),
        "description" => p.description.clone(),
        "prompt" => p.prompt.clone(),
        "provider" => p.provider.clone(),
        "model" => p.model.clone(),
        "tools" => tools.to_string(),
        _ => String::new(),
    }
}

pub fn take(p: &mut AgentProfile, tools: &mut String, field: &str, value: String) {
    match field {
        "name" => p.name = value,
        "display_name" => p.display_name = value,
        "description" => p.description = value,
        "prompt" => p.prompt = value,
        "provider" => p.provider = value,
        "model" => p.model = value,
        "tools" => *tools = value,
        _ => {}
    }
}

/// A draft's open offers: fields it changes that are not yet taken or dropped.
#[derive(Clone, Debug, Default)]
pub struct Offers {
    pub draft: Option<AgentDraft>,
    pub dropped: Vec<&'static str>,
    /// The form before the last Take, for Undo.
    pub undo: Option<(AgentProfile, String)>,
}

impl Offers {
    pub fn open(&self, p: &AgentProfile, tools: &str) -> Vec<(&'static str, String)> {
        let Some(d) = &self.draft else {
            return Vec::new();
        };
        FIELDS
            .iter()
            .filter(|f| !self.dropped.contains(f))
            .map(|f| (*f, offered(d, f)))
            .filter(|(f, v)| !v.trim().is_empty() && *v != current(p, tools, f))
            .collect()
    }

    /// The offers panel. Returns true when it changed the form.
    pub fn ui(&mut self, ui: &mut egui::Ui, p: &mut AgentProfile, tools: &mut String) -> bool {
        let open = self.open(p, tools);
        let pal = palette(ui.ctx());
        let mut changed = false;
        if open.is_empty() && self.undo.is_none() {
            return false;
        }
        kit::glow(ui).inner_margin(10).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(icons::label(
                        icons::SPARKLE,
                        &format!("Draft offers {}", open.len()),
                    ))
                    .strong()
                    .color(pal.gold_text),
                );
                if !open.is_empty()
                    && kit::ghost_button(ui, &icons::label(icons::CHECKS, "Take all")).clicked()
                {
                    self.undo = Some((p.clone(), tools.clone()));
                    for (f, v) in &open {
                        take(p, tools, f, v.clone());
                    }
                    changed = true;
                }
                if let Some((prev, prev_tools)) = self.undo.clone()
                    && kit::ghost_button(ui, &icons::label(icons::ARROW_COUNTER_CLOCKWISE, "Undo"))
                        .clicked()
                {
                    *p = prev;
                    *tools = prev_tools;
                    self.undo = None;
                    changed = true;
                }
            });
            for (f, v) in open {
                ui.horizontal(|ui| {
                    ui.label(kit::caps(f, pal.muted));
                    let short: String = v.chars().take(80).collect();
                    ui.add(egui::Label::new(RichText::new(short).size(12.0)).truncate())
                        .on_hover_text(&v);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if kit::ghost_button(ui, icons::X)
                            .on_hover_text("Drop")
                            .clicked()
                        {
                            self.dropped.push(f);
                        }
                        if kit::ghost_button(ui, icons::CHECK)
                            .on_hover_text("Take")
                            .clicked()
                        {
                            self.undo = Some((p.clone(), tools.clone()));
                            take(p, tools, f, v.clone());
                            changed = true;
                        }
                    });
                });
            }
        });
        changed
    }
}

/// What autosave keeps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Saved {
    pub profile: AgentProfile,
    pub tools: String,
    pub meta_json: String,
    pub at: i64,
}

impl Saved {
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "profile": self.profile, "tools": self.tools, "meta": self.meta_json, "at": self.at,
        })
        .to_string()
    }

    pub fn parse(s: &str) -> Option<Saved> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        Some(Saved {
            profile: serde_json::from_value(v.get("profile")?.clone()).ok()?,
            tools: v
                .get("tools")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            meta_json: v
                .get("meta")
                .and_then(|t| t.as_str())
                .unwrap_or("{}")
                .to_string(),
            at: v.get("at").and_then(|t| t.as_i64()).unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_take_drop_undo() {
        let mut p = AgentProfile {
            name: "fixer".into(),
            ..Default::default()
        };
        let mut tools = String::new();
        let mut o = Offers {
            draft: Some(AgentDraft {
                name: "fixer".into(),
                description: "Fixes".into(),
                prompt: "Do it".into(),
                tools: vec!["Read".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        let open = o.open(&p, &tools);
        assert_eq!(
            open.iter().map(|(f, _)| *f).collect::<Vec<_>>(),
            ["description", "prompt", "tools"],
            "same name is no offer"
        );
        o.dropped.push("prompt");
        o.undo = Some((p.clone(), tools.clone()));
        take(&mut p, &mut tools, "tools", "Read".into());
        assert_eq!(tools, "Read");
        assert_eq!(o.open(&p, &tools).len(), 1);
        let s = Saved {
            profile: p.clone(),
            tools: tools.clone(),
            ..Default::default()
        };
        let back = Saved::parse(&s.to_json()).unwrap();
        assert_eq!(back, s);
    }
}

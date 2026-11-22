//! The New session dialog's Agent row (belai or a detected harness on the
//! chosen node), the harness options (mode, model, keys, resume), and the
//! key-mode setting (global, per harness, per vault key).

use crate::model::{Model, Notice};
use crate::theme::palette;
use crate::{icons, kit};
use egui::RichText;
use kls_core::harness::{HarnessInfo, HarnessRequest, KeyMode, KeyPolicy};
use kls_store::vault::SecretMeta;

/// The Agent row's choice: belai (None) or a harness id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentPicker {
    pub harness: Option<String>,
    pub mode: String,
    pub model: String,
    /// The vault key picked for this session (hand-off); None is own login.
    pub key_id: Option<String>,
    pub resume: String,
}

/// What the dialog shows when no node offers a harness.
pub const NO_HARNESSES: &str = "No harnesses · run belai rc here";

fn state_note(h: &HarnessInfo) -> String {
    match h.state.as_str() {
        "gui" => "open only".into(),
        "broken" => "broken".into(),
        "configured" => "config only".into(),
        "unverified" => "unverified".into(),
        s => s.into(),
    }
}

/// Keys the dialog may offer for `h`: vault keys under a variable the
/// harness reads, whose resolved mode is not own login.
pub fn offered_keys<'a>(
    h: &HarnessInfo,
    policy: &KeyPolicy,
    keys: &'a [SecretMeta],
) -> Vec<&'a SecretMeta> {
    keys.iter()
        .filter(|k| h.grant_env.contains(&k.env))
        .filter(|k| policy.resolve(&h.id, Some(&k.id)) != KeyMode::OwnLogin)
        .collect()
}

impl AgentPicker {
    pub fn is_harness(&self) -> bool {
        self.harness.is_some()
    }

    /// The request for the chosen harness.
    pub fn request(&self, node_id: &str, cwd: &str, prompt: &str) -> Option<HarnessRequest> {
        let harness = self.harness.clone()?;
        Some(HarnessRequest {
            node_id: node_id.into(),
            harness,
            cwd: cwd.trim().into(),
            prompt: prompt.trim().into(),
            mode: self.mode.clone(),
            model: self.model.trim().into(),
            resume: self.resume.trim().into(),
            key_id: self.key_id.clone(),
            actor: "owner".into(),
            ..Default::default()
        })
    }

    /// The Agent row. `belai_version` labels the belai entry.
    pub fn agent_ui(&mut self, ui: &mut egui::Ui, belai_version: &str, list: &[HarnessInfo]) {
        let p = palette(ui.ctx());
        let (ready, detected) = kls_core::harness::picker(list);
        let current = match &self.harness {
            None => format!("belai {belai_version}"),
            Some(id) => list
                .iter()
                .find(|h| &h.id == id)
                .map(|h| h.name.clone())
                .unwrap_or_else(|| id.clone()),
        };
        egui::ComboBox::from_id_salt("harness-agent")
            .selected_text(icons::label(icons::ROBOT, &current))
            .width(320.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(self.harness.is_none(), format!("belai  {belai_version}"))
                    .clicked()
                {
                    *self = Self::default();
                }
                for h in &ready {
                    let mut text = format!("{}  {}", h.name, h.version);
                    if h.logged_in {
                        text.push_str("  ✓ login");
                    }
                    if !h.a2a {
                        text.push_str("  ⚠ no A2A");
                    }
                    if let Some(s) = startup_text(h) {
                        text.push_str("  ");
                        text.push_str(&s);
                    }
                    if ui
                        .selectable_label(self.harness.as_deref() == Some(&h.id), text)
                        .on_hover_text(caveats_text(h))
                        .clicked()
                    {
                        *self = Self {
                            harness: Some(h.id.clone()),
                            ..Default::default()
                        };
                    }
                }
                if !detected.is_empty() {
                    ui.separator();
                    ui.label(RichText::new("detected").small().color(p.muted));
                    for h in &detected {
                        ui.add_enabled(
                            false,
                            egui::Button::new(format!("{}  {}", h.name, state_note(h))),
                        )
                        .on_disabled_hover_text(h.detail.clone());
                    }
                }
                if ready.is_empty() && detected.is_empty() {
                    ui.label(RichText::new(NO_HARNESSES).small().color(p.muted));
                }
            });
    }

    /// Mode, model, keys and resume for the chosen harness.
    pub fn options_ui(
        &mut self,
        ui: &mut egui::Ui,
        list: &[HarnessInfo],
        policy: &KeyPolicy,
        keys: &[SecretMeta],
        resumable: &[(String, String)],
    ) {
        let Some(h) = self
            .harness
            .as_ref()
            .and_then(|id| list.iter().find(|h| &h.id == id))
        else {
            return;
        };
        let p = palette(ui.ctx());
        if !h.modes.is_empty() {
            ui.horizontal(|ui| {
                kit::field_label(ui, &icons::label(icons::SLIDERS, "Mode"));
                for m in &h.modes {
                    let on = self.mode == *m || (self.mode.is_empty() && m == "default");
                    if kit::pill(ui, &m.replace('_', " "), on).clicked() {
                        self.mode = if m == "default" {
                            String::new()
                        } else {
                            m.clone()
                        };
                    }
                }
            });
        }
        ui.horizontal(|ui| {
            kit::field_label(ui, &icons::label(icons::BRAIN, "Model"));
            ui.add(egui::TextEdit::singleline(&mut self.model).hint_text("harness default"));
        });
        let offered = offered_keys(h, policy, keys);
        ui.horizontal(|ui| {
            kit::field_label(ui, &icons::label(icons::KEY, "Keys"));
            if kit::pill(ui, "Own login", self.key_id.is_none()).clicked() {
                self.key_id = None;
            }
            if offered.is_empty() {
                ui.label(RichText::new("no vault key allowed").small().color(p.muted));
            } else {
                let sel = self
                    .key_id
                    .as_ref()
                    .and_then(|id| offered.iter().find(|k| &k.id == id))
                    .map(|k| format!("{} · {}", k.provider, k.env))
                    .unwrap_or_else(|| "Vault key".into());
                egui::ComboBox::from_id_salt("harness-key")
                    .selected_text(sel)
                    .show_ui(ui, |ui| {
                        for k in &offered {
                            let mode = policy.resolve(&h.id, Some(&k.id));
                            let text = format!("{} · {} · {}", k.provider, k.env, mode.as_str());
                            if ui
                                .selectable_label(self.key_id.as_ref() == Some(&k.id), text)
                                .clicked()
                            {
                                self.key_id = Some(k.id.clone());
                            }
                        }
                    });
            }
        });
        if self.key_id.is_some() {
            ui.label(
                RichText::new(icons::label(
                    icons::WARNING,
                    "The harness, its tools and this user can read the key",
                ))
                .small()
                .color(p.amber),
            );
        }
        if h.resume && !resumable.is_empty() {
            ui.horizontal(|ui| {
                kit::field_label(ui, &icons::label(icons::ARROW_COUNTER_CLOCKWISE, "Resume"));
                let sel = resumable
                    .iter()
                    .find(|(id, _)| *id == self.resume)
                    .map(|(_, t)| t.clone())
                    .unwrap_or_else(|| "none".into());
                egui::ComboBox::from_id_salt("harness-resume")
                    .selected_text(sel)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(self.resume.is_empty(), "none")
                            .clicked()
                        {
                            self.resume.clear();
                        }
                        for (id, title) in resumable {
                            if ui.selectable_label(self.resume == *id, title).clicked() {
                                self.resume = id.clone();
                            }
                        }
                    });
            });
        }
        ui.label(
            RichText::new(icons::label(
                icons::SHIELD_CHECK,
                &format!("{} rules", h.name),
            ))
            .small()
            .color(p.muted),
        );
        for c in &h.caveats {
            ui.label(
                RichText::new(icons::label(icons::INFO, &c.note))
                    .small()
                    .color(p.muted),
            )
            .on_hover_text(c.code.clone());
        }
    }
}

/// The key-mode setting: global, per harness, per vault key. Saving is
/// audited with who set it (`Core::set_harness_key_policy`).
#[derive(Default)]
pub struct KeyPolicyEditor {
    pub draft: Option<KeyPolicy>,
    pub new_harness: String,
}

fn mode_pills(ui: &mut egui::Ui, m: &mut KeyMode) {
    for k in [KeyMode::OwnLogin, KeyMode::Handoff, KeyMode::Always] {
        let word = match k {
            KeyMode::OwnLogin => "Own login",
            KeyMode::Handoff => "Hand-off",
            KeyMode::Always => "Always",
        };
        if kit::pill(ui, word, *m == k).clicked() {
            *m = k;
        }
    }
}

impl KeyPolicyEditor {
    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model, saved: &KeyPolicy, keys: &[SecretMeta]) {
        let p = palette(ui.ctx());
        let draft = self.draft.get_or_insert_with(|| saved.clone());
        ui.label(kit::caps("Harness keys", p.muted));
        ui.horizontal(|ui| {
            kit::field_label(ui, "All harnesses");
            mode_pills(ui, &mut draft.global);
        });
        let mut drop_h = None;
        for (id, m) in draft.harnesses.iter_mut() {
            ui.horizontal(|ui| {
                kit::field_label(ui, &icons::label(icons::ROBOT, id));
                mode_pills(ui, m);
                if kit::ghost_button(ui, icons::X).clicked() {
                    drop_h = Some(id.clone());
                }
            });
        }
        if let Some(id) = drop_h {
            draft.harnesses.remove(&id);
        }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.new_harness).hint_text("harness id"));
            if kit::ghost_button(ui, &icons::label(icons::PLUS, "Harness")).clicked() {
                let id = self.new_harness.trim().to_string();
                if !id.is_empty() {
                    draft.harnesses.entry(id).or_insert(draft.global);
                }
                self.new_harness.clear();
            }
        });
        for k in keys {
            ui.horizontal(|ui| {
                kit::field_label(
                    ui,
                    &icons::label(icons::KEY, &format!("{} · {}", k.provider, k.env)),
                );
                let mut m = draft.keys.get(&k.id).copied();
                if kit::pill(ui, "Inherit", m.is_none()).clicked() {
                    m = None;
                }
                let mut cur = m.unwrap_or(draft.global);
                let before = cur;
                mode_pills(ui, &mut cur);
                if cur != before || m.is_some() {
                    m = Some(cur);
                }
                match m {
                    Some(v) => {
                        draft.keys.insert(k.id.clone(), v);
                    }
                    None => {
                        draft.keys.remove(&k.id);
                    }
                }
            });
        }
        if !saved.updated_by.is_empty() {
            ui.label(
                RichText::new(format!("set by {}", saved.updated_by))
                    .small()
                    .color(p.muted),
            );
        }
        let changed = *draft != *saved;
        if ui
            .add_enabled(
                changed,
                egui::Button::new(icons::label(icons::FLOPPY_DISK, "Save")),
            )
            .clicked()
        {
            let d = draft.clone();
            self.draft = None;
            model.act(move |m| async move {
                m.core().set_harness_key_policy(d, "owner").await?;
                Ok(Some(Notice::Info("harness key modes saved".into())))
            });
        }
    }
}

/// "~1.6 s start (p95 4.0 s, 12 runs)" from the node's measured startups.
pub fn startup_text(h: &HarnessInfo) -> Option<String> {
    if h.startup_samples <= 0 || h.startup_p50_ms <= 0 {
        return None;
    }
    let s = |ms: i64| format!("{:.1} s", ms as f64 / 1000.0);
    Some(format!(
        "~{} start (p95 {}, {} runs)",
        s(h.startup_p50_ms),
        s(h.startup_p95_ms),
        h.startup_samples
    ))
}

/// The caveat codes and notes, one per line, for a hover.
pub fn caveats_text(h: &HarnessInfo) -> String {
    let mut out: Vec<String> = h
        .caveats
        .iter()
        .map(|c| format!("{}: {}", c.code, c.note))
        .collect();
    if h.resume {
        out.push(format!("resume: verified by {}", h.continuity.join(", ")));
    }
    if out.is_empty() {
        out.push("no known caveats".into());
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h() -> HarnessInfo {
        HarnessInfo {
            id: "claude-code".into(),
            name: "Claude Code".into(),
            state: "launch".into(),
            grant_env: vec!["ANTHROPIC_API_KEY".into()],
            ..Default::default()
        }
    }

    #[test]
    fn startup_and_caveats_text() {
        let mut x = h();
        assert!(startup_text(&x).is_none());
        x.startup_p50_ms = 1600;
        x.startup_p95_ms = 4000;
        x.startup_samples = 12;
        assert_eq!(
            startup_text(&x).unwrap(),
            "~1.6 s start (p95 4.0 s, 12 runs)"
        );
        x.caveats.push(kls_store::HarnessCaveat {
            code: "posix_only".into(),
            note: "POSIX only".into(),
        });
        assert!(caveats_text(&x).contains("posix_only: POSIX only"));
    }

    #[test]
    fn new_session_spec_picks_harness() {
        let mut a = AgentPicker::default();
        assert!(a.request("n1", "/w", "hi").is_none(), "belai by default");
        a.harness = Some("claude-code".into());
        a.mode = "plan".into();
        let r = a.request("n1", " /w ", " hi ").unwrap();
        assert_eq!(
            (
                r.harness.as_str(),
                r.cwd.as_str(),
                r.prompt.as_str(),
                r.mode.as_str()
            ),
            ("claude-code", "/w", "hi", "plan")
        );
        assert!(r.key_id.is_none(), "own login by default");
    }

    #[test]
    fn keys_offered_only_when_the_policy_allows() {
        let keys = vec![
            SecretMeta {
                id: "k1".into(),
                provider: "anthropic".into(),
                env: "ANTHROPIC_API_KEY".into(),
                ..Default::default()
            },
            SecretMeta {
                id: "k2".into(),
                provider: "openai".into(),
                env: "OPENAI_API_KEY".into(),
                ..Default::default()
            },
        ];
        assert!(offered_keys(&h(), &KeyPolicy::default(), &keys).is_empty());
        let pol = KeyPolicy {
            global: KeyMode::Handoff,
            ..Default::default()
        };
        let got = offered_keys(&h(), &pol, &keys);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "k1");
    }
}

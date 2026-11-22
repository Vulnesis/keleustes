//! Settings screen: the settings document by section, a save bar with the
//! unsaved count, discard, YAML export and import, and the factory controls
//! (kill switch, A2A blackout). Owner: M6 finish workstream.

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme;
use egui::{Align, Color32, Frame, Layout, Margin, RichText, Stroke, Ui, vec2};
use kls_core::audit::config::{self, Field, Kind, Saved, SettingsDoc, SettingsError};
use kls_core::board::model::FactoryControl;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub const READY: bool = true;

/// Nav entries after the document's sections.
const CONTROL: &str = "__control";
/// The focus id that opens the Stop confirm on behalf of the tray
/// (`keleustes://settings/control:stop`); the stop is audited as `owner:tray`.
pub const STOP_FROM_TRAY: &str = "control:stop";
const YAML: &str = "__yaml";
const PROJECTS: &str = "__projects";

/// Space between panels.
const GAP: f32 = 16.0;
/// Narrowest field column.
const FIELD_MIN: f32 = 260.0;

#[derive(Clone, Default)]
struct Data {
    doc: Arc<SettingsDoc>,
    control: FactoryControl,
    mode: kls_core::control::Mode,
}

/// One change an import would make: key, current, imported.
type PlanRow = (String, Value, Value);
type Plan = Result<Vec<PlanRow>, String>;

enum Outcome {
    Saved(Saved),
    Conflict(i64),
    Failed(String),
}

pub struct Screen {
    data: Loader<Data>,
    section: String,
    /// Scroll the selected section's card into view on the next frame.
    scroll_to: bool,
    /// Edited values (canonical), by key.
    draft: BTreeMap<String, Value>,
    /// Text being typed for text-like fields, by key.
    text: BTreeMap<String, String>,
    /// Per-key validation errors of the text being typed.
    errors: BTreeMap<String, String>,
    base_rev: Option<i64>,
    /// The document the draft was last rebased on.
    base_doc: Option<Arc<SettingsDoc>>,
    rebased: bool,
    saving: bool,
    outcome: Arc<Mutex<Option<Outcome>>>,
    /// The theme picked in Appearance, saved at once; shown until the
    /// stored value catches up.
    theme_pending: Option<String>,
    theme_outcome: Arc<Mutex<Option<Outcome>>>,
    message: Option<(bool, String)>,
    control_reason: String,
    /// Harness key modes and the vault keys (metadata) they name.
    projects: super::project_overrides::Projects,
    harness_keys: Loader<(
        kls_core::harness::KeyPolicy,
        Vec<kls_store::vault::SecretMeta>,
    )>,
    key_editor: super::harness_picker::KeyPolicyEditor,
    /// A kill-switch change waiting for its confirm.
    kill_confirm: Option<kls_core::control::Mode>,
    /// Who asked for it: `owner`, or `owner:tray` when the tray opened it.
    kill_by: &'static str,
    /// The A2A blackout waiting for its confirm.
    a2a_confirm: bool,
    yaml: String,
    yaml_path: String,
    plan: Arc<Mutex<Option<Plan>>>,
    /// The webhook address being typed (sealed on save, never shown back).
    chat_url: String,
    /// The host of the sealed webhook, if one is set.
    chat_host: Loader<Option<String>>,
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        let data = Loader::new(model, &["settings.", "factory."], |core| async move {
            Ok(Data {
                doc: Arc::new(core.settings_doc().await?),
                control: core.factory_control().await?,
                mode: kls_core::control::state(&core).await?.mode,
            })
        });
        Self {
            data,
            section: config::sections()[0].id.to_string(),
            scroll_to: false,
            draft: BTreeMap::new(),
            text: BTreeMap::new(),
            errors: BTreeMap::new(),
            base_rev: None,
            base_doc: None,
            rebased: false,
            saving: false,
            outcome: Arc::default(),
            theme_pending: None,
            theme_outcome: Arc::default(),
            message: None,
            control_reason: String::new(),
            kill_confirm: None,
            kill_by: "owner",
            projects: super::project_overrides::Projects::new(model),
            harness_keys: Loader::new(model, &["harness.", "vault."], |core| async move {
                Ok((
                    core.harness_key_policy().await?,
                    kls_core::vault::list_secrets(&core)
                        .await
                        .unwrap_or_default(),
                ))
            }),
            key_editor: Default::default(),
            a2a_confirm: false,
            yaml: String::new(),
            yaml_path: String::new(),
            plan: Arc::default(),
            chat_url: String::new(),
            chat_host: Loader::new(model, &["notify.", "settings."], |core| async move {
                kls_core::audit::notify::webhook_host(&core).await
            }),
        }
    }

    /// Opens a section (`appearance`, `node`, … `control`, `yaml`) or the
    /// section holding a key.
    pub fn focus(&mut self, id: &str) {
        if config::sections().iter().any(|s| s.id == id) {
            self.section = id.into();
        } else if let Some(s) = config::section_of(id) {
            self.section = s.id.into();
        } else if id == "control" {
            self.section = CONTROL.into();
        } else if id == STOP_FROM_TRAY {
            // The tray's Stop never acts on its own: it asks here first.
            self.section = CONTROL.into();
            self.kill_confirm = Some(kls_core::control::Mode::Stop);
            self.kill_by = "owner:tray";
        } else if id == "projects" {
            self.section = PROJECTS.into();
        } else if id == "yaml" {
            self.section = YAML.into();
        } else {
            return;
        }
        self.scroll_to = true;
    }

    fn discard(&mut self) {
        self.draft.clear();
        self.text.clear();
        self.errors.clear();
        self.rebased = false;
    }

    fn take_outcome(&mut self, ctx: &egui::Context) {
        if let Some(o) = self.theme_outcome.lock().unwrap().take() {
            match o {
                Outcome::Saved(_) => {}
                Outcome::Conflict(current) => {
                    self.theme_pending = None;
                    self.message = Some((
                        false,
                        format!("Theme not saved: settings changed (revision {current})"),
                    ));
                }
                Outcome::Failed(e) => {
                    self.theme_pending = None;
                    self.message = Some((false, e));
                }
            }
        }
        let Some(o) = self.outcome.lock().unwrap().take() else {
            return;
        };
        self.saving = false;
        match o {
            Outcome::Saved(s) => {
                if let Some(Value::String(t)) = s.changed.get(config::keys::THEME) {
                    theme::apply(ctx, theme::Mode::parse(t));
                }
                let mut msg = format!(
                    "Saved {} change{} (revision {}).",
                    s.changed.len(),
                    if s.changed.len() == 1 { "" } else { "s" },
                    s.revision
                );
                if !s.restart.is_empty() {
                    msg.push_str(&format!(
                        " Restart Keleustes for {} to take effect.",
                        s.restart.join(", ")
                    ));
                }
                self.message = Some((true, msg));
                self.discard();
                self.base_rev = Some(s.revision);
            }
            Outcome::Conflict(current) => {
                self.message = Some((
                    false,
                    format!(
                        "Someone saved the settings since you opened them (now revision {current}). Your edits are kept on top of the new values; review and save again."
                    ),
                ));
                self.base_rev = Some(current);
            }
            Outcome::Failed(e) => self.message = Some((false, e)),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let pal = theme::palette(ui.ctx());
        self.take_outcome(ui.ctx());
        let data = match self.data.get() {
            Some(Ok(d)) => d,
            Some(Err(e)) => {
                kit::page_header(ui, icons::GEAR_SIX, "Settings", |_| {});
                ui.colored_label(pal.red, e);
                return;
            }
            None => {
                kit::page_header(ui, icons::GEAR_SIX, "Settings", |_| {});
                ui.spinner();
                return;
            }
        };
        let doc = data.doc.clone();
        // Rebase the draft over a newer copy: keep only real differences,
        // and flag it when a drafted key's stored value moved underneath.
        match self.base_rev {
            None => self.base_rev = Some(doc.revision),
            Some(r) if r != doc.revision => {
                self.draft.retain(|k, v| doc.values.get(k) != Some(v));
                let moved = match &self.base_doc {
                    Some(old) => self
                        .draft
                        .keys()
                        .any(|k| old.values.get(k) != doc.values.get(k)),
                    None => !self.draft.is_empty(),
                };
                if moved {
                    self.rebased = true;
                }
                self.base_rev = Some(doc.revision);
            }
            _ => {}
        }
        self.base_doc = Some(doc.clone());
        self.draft.retain(|k, v| doc.values.get(k) != Some(v));
        if let Some(t) = &self.theme_pending
            && doc.values.get(config::keys::THEME).and_then(|v| v.as_str()) == Some(t.as_str())
        {
            self.theme_pending = None;
        }

        kit::page_header(ui, icons::GEAR_SIX, "Settings", |ui| {
            if kit::ghost_button(ui, &icons::label(icons::SCROLL, "Changes"))
                .on_hover_text("Setting changes in the audit ledger")
                .clicked()
            {
                model.go(crate::Screen::Audit, Some("kind:setting".into()));
            }
            ui.label(
                RichText::new(format!("rev {}", doc.revision))
                    .monospace()
                    .size(12.0)
                    .color(pal.muted),
            )
            .on_hover_text(format!("As of {}", fmt::when(doc.as_of)));
        });

        let wide = ui.available_width() >= 200.0 + GAP + 520.0;
        if wide {
            egui::Panel::left("settings_nav")
                .resizable(false)
                .exact_size(240.0)
                .frame(Frame::NONE.inner_margin(Margin {
                    left: 0,
                    right: GAP as i8,
                    top: 0,
                    bottom: 0,
                }))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("settings_nav_scroll")
                        .show(ui, |ui| self.nav_ui(ui, &data.control));
                });
        }

        egui::Panel::bottom("settings_save")
            .resizable(false)
            .frame(Frame::NONE.inner_margin(Margin {
                left: 0,
                right: 0,
                top: 12,
                bottom: 0,
            }))
            .show(ui, |ui| self.save_bar(ui, model, &doc));

        egui::CentralPanel::default()
            .frame(Frame::NONE)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("settings_cards")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if !wide {
                            self.nav_pills(ui, &data.control);
                            ui.add_space(12.0);
                        }
                        for s in config::sections() {
                            let on = self.section == s.id;
                            let rect = ui
                                .scope(|ui| {
                                    kit::section(
                                        ui,
                                        section_icon(s.id),
                                        s.title,
                                        on,
                                        |ui| {
                                            ui.label(RichText::new(icons::INFO).color(pal.muted))
                                                .on_hover_text(s.desc);
                                            if s.id == "budgets"
                                                && kit::ghost_button(
                                                    ui,
                                                    &icons::label(
                                                        icons::ARROW_SQUARE_OUT,
                                                        "Budgets",
                                                    ),
                                                )
                                                .clicked()
                                            {
                                                model.go(crate::Screen::Budgets, None);
                                            }
                                        },
                                        |ui| {
                                            self.fields_ui(ui, model, s.fields, &doc);
                                            if s.id == "notifications" {
                                                self.chat_ui(ui, model);
                                            }
                                        },
                                    );
                                })
                                .response
                                .rect;
                            self.maybe_scroll(ui, s.id, rect);
                            ui.add_space(14.0);
                        }
                        let rect = ui
                            .scope(|ui| {
                                kit::section(
                                    ui,
                                    icons::POWER,
                                    "Control",
                                    self.section == CONTROL || data.control.stopped,
                                    |ui| {
                                        ui.label(RichText::new(icons::INFO).color(pal.muted))
                                            .on_hover_text(
                                                "Applies at once, tells every node, and is recorded in the ledger",
                                            );
                                    },
                                    |ui| self.control_ui(ui, model, &data.control, data.mode),
                                );
                            })
                            .response
                            .rect;
                        self.maybe_scroll(ui, CONTROL, rect);
                        ui.add_space(14.0);
                        let rect = ui
                            .scope(|ui| {
                                kit::section(
                                    ui,
                                    icons::FOLDER,
                                    "Project overrides",
                                    self.section == PROJECTS,
                                    |ui| {
                                        ui.label(RichText::new(icons::INFO).color(pal.muted))
                                            .on_hover_text(
                                                "belai settings for one directory on one node. Sent to the node (it needs --web-project-settings), recorded in the ledger.",
                                            );
                                    },
                                    |ui| self.projects.ui(ui, model),
                                );
                            })
                            .response
                            .rect;
                        self.maybe_scroll(ui, PROJECTS, rect);
                        ui.add_space(14.0);
                        // Harness keys (harnesses.md): global, per harness, per key.
                        if let Some(Ok(h)) = self.harness_keys.get() {
                            kit::section(
                                ui,
                                icons::KEY,
                                "Harness keys",
                                false,
                                |_| {},
                                |ui| self.key_editor.ui(ui, model, &h.0, &h.1),
                            );
                            ui.add_space(14.0);
                        }
                        let rect = ui
                            .scope(|ui| {
                                kit::section(
                                    ui,
                                    icons::FILE_TEXT,
                                    "YAML",
                                    self.section == YAML,
                                    |ui| {
                                        ui.label(RichText::new(icons::INFO).color(pal.muted))
                                            .on_hover_text(
                                                "kind: FactorySettings. An import is a whole document: keys it leaves out reset to default.",
                                            );
                                    },
                                    |ui| self.yaml_ui(ui, model, doc.revision),
                                );
                            })
                            .response
                            .rect;
                        self.maybe_scroll(ui, YAML, rect);
                        ui.add_space(8.0);
                    });
            });
    }

    fn maybe_scroll(&mut self, ui: &mut Ui, id: &str, rect: egui::Rect) {
        if self.scroll_to && self.section == id {
            ui.scroll_to_rect(rect, Some(Align::TOP));
            self.scroll_to = false;
        }
    }

    /// Unsaved or invalid fields in a section.
    fn dirty_in(&self, fields: &[Field]) -> usize {
        fields
            .iter()
            .filter(|f| self.draft.contains_key(f.key) || self.errors.contains_key(f.key))
            .count()
    }

    /// The section list (wide layout).
    fn nav_ui(&mut self, ui: &mut Ui, control: &FactoryControl) {
        let pal = theme::palette(ui.ctx());
        kit::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            for s in config::sections() {
                let dirty = self.dirty_in(s.fields);
                if nav_row(
                    ui,
                    section_icon(s.id),
                    s.title,
                    dirty,
                    self.section == s.id,
                    None,
                )
                .clicked()
                {
                    self.section = s.id.into();
                    self.scroll_to = true;
                }
            }
            let (icon, color) = if control.stopped {
                (icons::STOP, Some(pal.red))
            } else {
                (icons::POWER, None)
            };
            if nav_row(ui, icon, "Control", 0, self.section == CONTROL, color).clicked() {
                self.section = CONTROL.into();
                self.scroll_to = true;
            }
            if nav_row(
                ui,
                icons::FOLDER,
                "Project overrides",
                0,
                self.section == PROJECTS,
                None,
            )
            .clicked()
            {
                self.section = PROJECTS.into();
                self.scroll_to = true;
            }
            if nav_row(ui, icons::FILE_TEXT, "YAML", 0, self.section == YAML, None).clicked() {
                self.section = YAML.into();
                self.scroll_to = true;
            }
        });
    }

    /// The section list as wrapped pills (narrow layout).
    fn nav_pills(&mut self, ui: &mut Ui, control: &FactoryControl) {
        let pal = theme::palette(ui.ctx());
        ui.horizontal_wrapped(|ui| {
            let mut entries: Vec<(&str, String, usize, Option<Color32>)> = config::sections()
                .iter()
                .map(|s| (s.id, s.title.to_string(), self.dirty_in(s.fields), None))
                .collect();
            entries.push((
                CONTROL,
                "Control".into(),
                0,
                control.stopped.then_some(pal.red),
            ));
            entries.push((PROJECTS, "Project overrides".into(), 0, None));
            entries.push((YAML, "YAML".into(), 0, None));
            for (id, title, dirty, color) in entries {
                let text = if dirty > 0 {
                    format!("{title} • {dirty}")
                } else {
                    title
                };
                let r = if self.section == id {
                    kit::chip_filled(ui, &text)
                } else {
                    kit::chip(ui, &text, color.unwrap_or(pal.text))
                };
                if ui
                    .interact(r.rect, r.id.with("nav"), egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    self.section = id.into();
                    self.scroll_to = true;
                }
            }
        });
    }

    /// The sticky save bar: unsaved count, Export YAML, Discard, Save.
    fn save_bar(&mut self, ui: &mut Ui, model: &Model, doc: &SettingsDoc) {
        let pal = theme::palette(ui.ctx());
        let unsaved = self.draft.len();
        let invalid = self.errors.len();
        let edge = if unsaved > 0 || invalid > 0 {
            pal.gold_edge
        } else {
            pal.line
        };
        Frame::new()
            .fill(pal.panel2)
            .stroke(Stroke::new(1.0, edge))
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    if unsaved == 0 && invalid == 0 {
                        ui.label(
                            RichText::new(icons::label(icons::CHECK, "No unsaved changes"))
                                .size(13.0)
                                .color(pal.muted),
                        )
                        .on_hover_text(format!(
                            "Saved · revision {} · as of {}",
                            doc.revision,
                            fmt::when(doc.as_of)
                        ));
                    } else {
                        ui.label(
                            RichText::new(format!(
                                "{unsaved} unsaved change{}",
                                if unsaved == 1 { "" } else { "s" }
                            ))
                            .size(13.0)
                            .strong()
                            .color(pal.gold_text),
                        );
                        if invalid > 0 {
                            ui.colored_label(
                                pal.red,
                                icons::label(icons::WARNING, &format!("{invalid} invalid")),
                            );
                        }
                    }
                    if self.rebased {
                        ui.colored_label(
                            pal.amber,
                            icons::label(icons::ARROWS_CLOCKWISE, "Rebased"),
                        )
                        .on_hover_text("Saved elsewhere; your edits sit on the new values");
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let can_save = unsaved > 0 && self.errors.is_empty() && !self.saving;
                        let save = ui
                            .add_enabled_ui(can_save, |ui| {
                                theme::primary_button(ui, &icons::label(icons::CHECK, "Save"))
                            })
                            .inner;
                        if save.clicked() {
                            self.save(model, doc.revision);
                        }
                        let can_discard = unsaved > 0 || !self.errors.is_empty();
                        if ui
                            .add_enabled_ui(can_discard, |ui| {
                                kit::ghost_button(ui, &icons::label(icons::X, "Discard"))
                            })
                            .inner
                            .clicked()
                        {
                            self.discard();
                            self.message = None;
                        }
                        if kit::ghost_button(ui, &icons::label(icons::EXPORT, "Export YAML"))
                            .on_hover_text("Write to the data folder and copy")
                            .clicked()
                        {
                            export_yaml(model);
                        }
                        if self.saving {
                            ui.spinner();
                        }
                    });
                });
                if let Some((ok, m)) = &self.message {
                    ui.colored_label(if *ok { pal.green } else { pal.red }, m);
                }
            });
    }

    fn save(&mut self, model: &Model, revision: i64) {
        let changes = self.draft.clone();
        let slot = self.outcome.clone();
        self.saving = true;
        self.message = None;
        self.rebased = false;
        model.act(move |m| async move {
            let o = match m.core().settings_save(revision, changes).await {
                Ok(s) => Outcome::Saved(s),
                Err(e) => match e.downcast_ref::<SettingsError>() {
                    Some(SettingsError::Conflict { current }) => Outcome::Conflict(*current),
                    _ => Outcome::Failed(format!("{e:#}")),
                },
            };
            *slot.lock().unwrap() = Some(o);
            m.egui_ctx().request_repaint();
            Ok(None)
        });
    }

    /// Saves the theme on its own, at once, so the whole app follows.
    fn save_theme(&mut self, ctx: &egui::Context, model: &Model, revision: i64, value: &str) {
        theme::apply(ctx, theme::Mode::parse(value));
        self.theme_pending = Some(value.to_string());
        let slot = self.theme_outcome.clone();
        let value = value.to_string();
        model.act(move |m| async move {
            let changes = BTreeMap::from([(config::keys::THEME.to_string(), Value::String(value))]);
            let core = m.core();
            let mut r = core.settings_save(revision, changes.clone()).await;
            // Only this key changes, so a newer revision is safe to retry on.
            if let Err(e) = &r
                && let Some(SettingsError::Conflict { current }) = e.downcast_ref::<SettingsError>()
            {
                r = core.settings_save(*current, changes).await;
            }
            let o = match r {
                Ok(s) => Outcome::Saved(s),
                Err(e) => match e.downcast_ref::<SettingsError>() {
                    Some(SettingsError::Conflict { current }) => Outcome::Conflict(*current),
                    _ => Outcome::Failed(format!("{e:#}")),
                },
            };
            *slot.lock().unwrap() = Some(o);
            m.egui_ctx().request_repaint();
            Ok(None)
        });
    }

    /// A section's fields in as many columns as fit.
    fn fields_ui(
        &mut self,
        ui: &mut Ui,
        model: &Model,
        fields: &'static [Field],
        doc: &SettingsDoc,
    ) {
        kit::Grid::new(FIELD_MIN)
            .gap(14.0)
            .show(ui, fields.len(), |ui, i, w| {
                self.field_ui(ui, model, &fields[i], doc, w);
            });
    }

    /// The label row above a field: name, help, restart, undo.
    fn field_label(&mut self, ui: &mut Ui, f: &'static Field, dirty: bool, name: bool) {
        let pal = theme::palette(ui.ctx());
        if name {
            let color = if dirty { pal.gold_text } else { pal.muted };
            let hover = if f.help.is_empty() {
                f.key.to_string()
            } else {
                format!("{}\n{}", f.key, f.help)
            };
            ui.label(RichText::new(f.label).size(12.0).color(color))
                .on_hover_text(hover);
        }
        if !f.help.is_empty() {
            ui.label(RichText::new(icons::QUESTION).size(12.0).color(pal.muted))
                .on_hover_text(f.help);
        }
        if config::PENDING.contains(&f.key) {
            ui.label(RichText::new(icons::HOURGLASS).size(12.0).color(pal.amber))
                .on_hover_text("Saved and audited; the engine applies it once wired");
        }
        if f.restart {
            ui.label(
                RichText::new(icons::ARROWS_CLOCKWISE)
                    .size(12.0)
                    .color(pal.amber),
            )
            .on_hover_text("Needs a restart");
        }
        if dirty
            && ui
                .small_button(icons::ARROW_CLOCKWISE)
                .on_hover_text("Undo this change")
                .clicked()
        {
            self.draft.remove(f.key);
            self.text.remove(f.key);
            self.errors.remove(f.key);
        }
    }

    fn field_ui(
        &mut self,
        ui: &mut Ui,
        model: &Model,
        f: &'static Field,
        doc: &SettingsDoc,
        width: f32,
    ) {
        let pal = theme::palette(ui.ctx());
        let server = doc.values.get(f.key).cloned().unwrap_or(Value::Null);
        let current = self.draft.get(f.key).cloned().unwrap_or(server.clone());
        let dirty = self.draft.contains_key(f.key);
        let mut set: Option<Value> = None;
        if f.kind == Kind::Bool {
            ui.horizontal(|ui| {
                let mut b = current.as_bool().unwrap_or(false);
                let color = if dirty { pal.gold_text } else { pal.text };
                if ui
                    .checkbox(&mut b, RichText::new(f.label).size(14.0).color(color))
                    .on_hover_text(f.key)
                    .changed()
                {
                    set = Some(Value::Bool(b));
                }
                self.field_label(ui, f, dirty, false);
            });
        } else {
            ui.horizontal(|ui| self.field_label(ui, f, dirty, true));
            match f.kind {
                Kind::ReadOnly => {
                    ui.add(
                        egui::Label::new(RichText::new(config::encode(&current)).monospace())
                            .wrap(),
                    );
                }
                Kind::Enum if f.options.len() <= 3 => {
                    let theme_key = f.key == config::keys::THEME;
                    let cur = if theme_key {
                        self.theme_pending
                            .clone()
                            .unwrap_or_else(|| current.as_str().unwrap_or("").to_string())
                    } else {
                        current.as_str().unwrap_or("").to_string()
                    };
                    if let Some(v) = segmented(ui, f.options, &cur, width) {
                        if theme_key {
                            if v != cur {
                                self.save_theme(ui.ctx(), model, doc.revision, &v);
                            }
                        } else {
                            set = Some(Value::String(v));
                        }
                    }
                }
                Kind::Enum => {
                    let cur = current.as_str().unwrap_or("").to_string();
                    let shown = f
                        .options
                        .iter()
                        .find(|(v, _)| *v == cur)
                        .map(|(_, l)| l.to_string())
                        .unwrap_or(cur.clone());
                    egui::ComboBox::from_id_salt(f.key)
                        .selected_text(shown)
                        .width(width)
                        .show_ui(ui, |ui| {
                            for (v, label) in f.options {
                                if ui.selectable_label(cur == *v, *label).clicked() {
                                    set = Some(Value::String(v.to_string()));
                                }
                            }
                        });
                }
                _ => {
                    let buf = self
                        .text
                        .entry(f.key.to_string())
                        .or_insert_with(|| config::encode(&current));
                    let hint = if f.default.is_empty() {
                        "(empty)".to_string()
                    } else {
                        format!("default {}", f.default)
                    };
                    let mut edit = egui::TextEdit::singleline(buf)
                        .hint_text(hint)
                        .desired_width(width);
                    if !matches!(f.kind, Kind::Str | Kind::StrList) {
                        edit = edit.font(egui::TextStyle::Monospace);
                    }
                    let resp = ui.add(edit);
                    if resp.changed() {
                        match config::validate(f, &Value::String(buf.clone())) {
                            Ok(v) => {
                                self.errors.remove(f.key);
                                set = Some(v);
                            }
                            Err(e) => {
                                self.errors.insert(f.key.to_string(), e);
                            }
                        }
                    }
                    if !resp.has_focus() && !self.errors.contains_key(f.key) && !dirty {
                        // Follow the stored value when not being edited.
                        *buf = config::encode(&server);
                    }
                }
            }
        }
        if let Some(v) = set {
            if v == server {
                self.draft.remove(f.key);
            } else {
                self.draft.insert(f.key.to_string(), v);
            }
        }
        if let Some(e) = self.errors.get(f.key) {
            ui.label(
                RichText::new(format!("{} {e}", f.label))
                    .size(12.0)
                    .color(pal.red),
            );
        }
    }

    fn control_ui(
        &mut self,
        ui: &mut egui::Ui,
        model: &Model,
        c: &FactoryControl,
        mode: kls_core::control::Mode,
    ) {
        use kls_core::control::Mode;
        let pal = theme::palette(ui.ctx());
        ui.label(RichText::new("Reason").size(12.0).color(pal.muted));
        ui.add(
            egui::TextEdit::singleline(&mut self.control_reason)
                .hint_text("Reason")
                .char_limit(kls_core::board::REASON_MAX)
                .desired_width(360.0_f32.min(ui.available_width())),
        )
        .on_hover_text("Shown to nodes and recorded in the audit ledger");
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            let chip = match mode {
                Mode::Running => kit::chip(ui, &icons::label(icons::PLAY, "Running"), pal.green),
                Mode::Pause => kit::chip(ui, &icons::label(icons::PAUSE, "Paused"), pal.amber),
                Mode::Stop => kit::chip(ui, &icons::label(icons::STOP, "Stopped"), pal.red),
            };
            if !mode.running() {
                let mut hover = c
                    .stopped_since
                    .map(|at| format!("since {}", fmt::when(at)))
                    .unwrap_or_default();
                if !c.stopped_reason.is_empty() {
                    hover.push_str(&format!(" · {}", c.stopped_reason));
                }
                chip.on_hover_text(hover);
            }
            if mode == Mode::Running
                && kit::ghost_button(ui, &icons::label(icons::PAUSE, "Pause"))
                    .on_hover_text("Nothing new starts; running steps settle")
                    .clicked()
            {
                self.kill_confirm = Some(Mode::Pause);
                self.kill_by = "owner";
            }
            if mode != Mode::Stop
                && kit::danger_button(ui, &icons::label(icons::STOP, "Stop all"))
                    .on_hover_text("Cancels open runs and stops dispatched sessions")
                    .clicked()
            {
                self.kill_confirm = Some(Mode::Stop);
                self.kill_by = "owner";
            }
            if !mode.running()
                && theme::primary_button(ui, &icons::label(icons::PLAY, "Resume"))
                    .on_hover_text("Held schedule fires run once")
                    .clicked()
            {
                self.kill_confirm = Some(Mode::Running);
                self.kill_by = "owner";
            }
            ui.add_space(16.0);
            let chip = if c.a2a_blackout {
                kit::chip(
                    ui,
                    &icons::label(icons::PROHIBIT, "A2A blackout"),
                    pal.amber,
                )
            } else {
                kit::chip(
                    ui,
                    &icons::label(icons::BROADCAST, "A2A relayed"),
                    pal.green,
                )
            };
            if c.a2a_blackout {
                let mut hover = c
                    .a2a_since
                    .map(|at| format!("since {}", fmt::when(at)))
                    .unwrap_or_default();
                if !c.a2a_reason.is_empty() {
                    hover.push_str(&format!(" · {}", c.a2a_reason));
                }
                chip.on_hover_text(hover);
            }
            if c.a2a_blackout {
                if kit::ghost_button(ui, &icons::label(icons::BROADCAST, "Lift blackout")).clicked()
                {
                    model.act(|m| async move {
                        m.core().set_a2a_blackout(false, "").await?;
                        Ok(None)
                    });
                }
            } else if kit::ghost_button(ui, &icons::label(icons::PROHIBIT, "A2A blackout"))
                .on_hover_text("Withhold agent messages between nodes")
                .clicked()
            {
                self.a2a_confirm = true;
            }
        });
        self.control_confirm(ui.ctx(), model, mode);
    }

    /// The confirm windows for the kill switch and the A2A blackout.
    fn control_confirm(
        &mut self,
        ctx: &egui::Context,
        model: &Model,
        mode: kls_core::control::Mode,
    ) {
        use kls_core::control::Mode;
        let pal = theme::palette(ctx);
        if let Some(target) = self.kill_confirm {
            let (title, lines): (&str, &[&str]) = match target {
                Mode::Pause => (
                    "Pause the factory?",
                    &[
                        "Nothing new starts: dispatches, jobs, triggers, schedules.",
                        "Running steps settle. Schedule fires are held.",
                        "No key leaves the vault.",
                    ],
                ),
                Mode::Stop => (
                    "Stop the factory?",
                    &[
                        "Every open run is cancelled; its sessions are told to stop.",
                        "Sessions dispatched for cards are told to stop.",
                        "Nothing new starts. No key leaves the vault.",
                    ],
                ),
                Mode::Running => (
                    "Resume the factory?",
                    &["Held schedule fires run per their catch-up policy."],
                ),
            };
            let mut close = false;
            egui::Window::new(icons::label(icons::WARNING, title))
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    for l in lines {
                        ui.label(*l);
                    }
                    if !self.control_reason.trim().is_empty() {
                        ui.label(
                            RichText::new(format!("Reason: {}", self.control_reason.trim()))
                                .color(pal.muted),
                        );
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let go = match target {
                            Mode::Running => {
                                theme::primary_button(ui, &icons::label(icons::PLAY, "Resume"))
                            }
                            Mode::Pause => {
                                kit::danger_button(ui, &icons::label(icons::PAUSE, "Pause"))
                            }
                            Mode::Stop => {
                                kit::danger_button(ui, &icons::label(icons::STOP, "Stop all"))
                            }
                        };
                        if go.clicked() && target != mode {
                            let reason = self.control_reason.trim().to_string();
                            let by = self.kill_by;
                            model.act(move |m| async move {
                                let r = kls_core::control::set_mode(m.core(), target, &reason, by)
                                    .await?;
                                Ok(Some(Notice::Info(match target {
                                    Mode::Stop => format!(
                                        "Stopped: {} runs cancelled, {} sessions stopped{}",
                                        r.runs_cancelled,
                                        r.sessions_stopped,
                                        if r.errors.is_empty() {
                                            String::new()
                                        } else {
                                            format!(", {} errors", r.errors.len())
                                        }
                                    ),
                                    Mode::Pause => "Paused".into(),
                                    Mode::Running => "Resumed".into(),
                                })))
                            });
                            close = true;
                        }
                        if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                            close = true;
                        }
                    });
                });
            if close {
                self.kill_confirm = None;
            }
        }
        if self.a2a_confirm {
            let mut close = false;
            egui::Window::new(icons::label(icons::PROHIBIT, "A2A blackout?"))
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Agents stop sending and reading messages across nodes.");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if kit::danger_button(ui, &icons::label(icons::PROHIBIT, "Black out"))
                            .clicked()
                        {
                            let reason = self.control_reason.trim().to_string();
                            model.act(move |m| async move {
                                m.core().set_a2a_blackout(true, &reason).await?;
                                Ok(None)
                            });
                            close = true;
                        }
                        if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                            close = true;
                        }
                    });
                });
            if close {
                self.a2a_confirm = false;
            }
        }
    }

    /// The chat channel's webhook: sealed in the vault on save, shown back
    /// only by host; a test post goes through the same checks as events.
    fn chat_ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let pal = theme::palette(ui.ctx());
        let host = self.chat_host.get().and_then(Result::ok).flatten();
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(icons::label(icons::CHAT_TEXT, "Webhook"))
                    .size(12.0)
                    .color(pal.muted),
            )
            .on_hover_text("Sealed in the vault; never shown, logged or audited in full");
            match &host {
                Some(h) => {
                    kit::chip(ui, &icons::label(icons::LOCK_SIMPLE, h), pal.green);
                }
                None => {
                    kit::chip(ui, &icons::label(icons::MINUS, "Not set"), pal.muted);
                }
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.chat_url)
                    .password(true)
                    .hint_text("https://hooks.slack.com/…")
                    .desired_width(260.0),
            );
            let typed = !self.chat_url.trim().is_empty();
            if ui
                .add_enabled(
                    typed,
                    egui::Button::new(icons::label(icons::FLOPPY_DISK, "Seal")),
                )
                .clicked()
            {
                let url = kls_core::vault::SecretString::new(std::mem::take(&mut self.chat_url));
                model.act(move |m| async move {
                    let h = kls_core::audit::notify::set_webhook(m.core(), url).await?;
                    Ok(Some(Notice::Info(format!(
                        "Webhook sealed ({})",
                        h.unwrap_or_default()
                    ))))
                });
            }
            if host.is_some() {
                if kit::ghost_button(ui, &icons::label(icons::PAPER_PLANE_TILT, "Send test"))
                    .clicked()
                {
                    model.act(|m| async move {
                        let o = kls_core::audit::notify::post_test(m.core()).await?;
                        Ok(Some(Notice::Info(format!("Chat: {o:?}"))))
                    });
                }
                if kit::ghost_button(ui, icons::TRASH)
                    .on_hover_text("Forget the webhook")
                    .clicked()
                {
                    model.act(|m| async move {
                        kls_core::audit::notify::set_webhook(
                            m.core(),
                            kls_core::vault::SecretString::new(""),
                        )
                        .await?;
                        Ok(Some(Notice::Info("Webhook removed".into())))
                    });
                }
            }
        });
    }

    fn yaml_ui(&mut self, ui: &mut egui::Ui, model: &Model, revision: i64) {
        let pal = theme::palette(ui.ctx());
        ui.horizontal_wrapped(|ui| {
            if kit::ghost_button(ui, &icons::label(icons::EXPORT, "Export"))
                .on_hover_text("Write to the data folder and copy")
                .clicked()
            {
                export_yaml(model);
            }
            if kit::ghost_button(ui, &icons::label(icons::DOWNLOAD_SIMPLE, "Load current"))
                .on_hover_text("Current settings into the editor")
                .clicked()
            {
                let core = model.core().clone();
                if let Ok(y) = model.rt.block_on(async move { core.settings_yaml().await }) {
                    self.yaml = y;
                }
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.yaml_path)
                    .hint_text(icons::label(icons::FOLDER, "File path"))
                    .desired_width(280.0),
            );
            if kit::ghost_button(ui, &icons::label(icons::UPLOAD_SIMPLE, "Read")).clicked() {
                match std::fs::read_to_string(self.yaml_path.trim()) {
                    Ok(t) => self.yaml = t,
                    Err(e) => self.message = Some((false, format!("read {}: {e}", self.yaml_path))),
                }
            }
        });
        ui.add_space(8.0);
        ui.add(
            egui::TextEdit::multiline(&mut self.yaml)
                .code_editor()
                .hint_text("Paste YAML")
                .desired_rows(12)
                .desired_width(f32::INFINITY),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let has = !self.yaml.trim().is_empty();
            if ui
                .add_enabled_ui(has, |ui| {
                    kit::ghost_button(ui, &icons::label(icons::EYE, "Preview"))
                })
                .inner
                .clicked()
            {
                let yaml = self.yaml.clone();
                let slot = self.plan.clone();
                model.act(move |m| async move {
                    let r = m
                        .core()
                        .settings_import_plan(&yaml)
                        .await
                        .map(|p| p.into_iter().map(|(k, (a, b))| (k, a, b)).collect())
                        .map_err(|e| format!("{e:#}"));
                    *slot.lock().unwrap() = Some(r);
                    Ok(None)
                });
            }
            if ui
                .add_enabled_ui(has && self.draft.is_empty(), |ui| {
                    theme::primary_button(ui, &icons::label(icons::UPLOAD_SIMPLE, "Import"))
                })
                .inner
                .on_disabled_hover_text("Save or discard first")
                .clicked()
            {
                let yaml = self.yaml.clone();
                let slot = self.outcome.clone();
                self.saving = true;
                model.act(move |m| async move {
                    let o = match m.core().settings_import_yaml(revision, &yaml).await {
                        Ok(s) => Outcome::Saved(s),
                        Err(e) => match e.downcast_ref::<SettingsError>() {
                            Some(SettingsError::Conflict { current }) => {
                                Outcome::Conflict(*current)
                            }
                            _ => Outcome::Failed(format!("{e:#}")),
                        },
                    };
                    *slot.lock().unwrap() = Some(o);
                    Ok(None)
                });
                *self.plan.lock().unwrap() = None;
            }
        });
        if let Some(p) = self.plan.lock().unwrap().as_ref() {
            ui.add_space(8.0);
            match p {
                Ok(rows) if rows.is_empty() => {
                    ui.weak(icons::label(icons::CHECK, "No changes"));
                }
                Ok(rows) => {
                    ui.label(kit::caps(&format!("{} changes", rows.len()), pal.muted));
                    egui::Grid::new("yaml_plan").striped(true).show(ui, |ui| {
                        for (k, a, b) in rows {
                            ui.label(RichText::new(k).monospace());
                            ui.label(RichText::new(config::encode(a)).color(pal.muted));
                            ui.label("→");
                            ui.label(RichText::new(config::encode(b)).color(pal.gold_text));
                            ui.end_row();
                        }
                    });
                }
                Err(e) => {
                    ui.colored_label(pal.red, e);
                }
            }
        }
    }
}

/// Writes the settings YAML to the data folder and copies it.
fn export_yaml(model: &Model) {
    model.act(|m| async move {
        let yaml = m.core().settings_yaml().await?;
        let dir = m.core().data_dir().join("exports");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("settings-{}.yaml", kls_store::now_ms()));
        tokio::fs::write(&path, &yaml).await?;
        m.egui_ctx().copy_text(yaml);
        Ok(Some(Notice::Info(format!(
            "Exported to {} (also copied)",
            path.display()
        ))))
    });
}

fn section_icon(id: &str) -> &'static str {
    match id {
        "appearance" => icons::SUN,
        "node" => icons::PLUGS_CONNECTED,
        "autonomy" => icons::ROBOT,
        "defaults" => icons::SLIDERS,
        "models" => icons::CPU,
        "github" => icons::GITHUB_LOGO,
        "signing" => icons::FINGERPRINT,
        "notifications" => icons::BELL,
        "schedules" => icons::CALENDAR,
        "persistence" => icons::DATABASE,
        "budgets" => icons::WALLET,
        "vault" => icons::LOCK_SIMPLE,
        "estimates" => icons::TREND_UP,
        "crews" => icons::USERS_THREE,
        "systemone" => icons::LIGHTNING,
        "hosts" => icons::HARD_DRIVES,
        "quota" => icons::GITHUB_LOGO,
        "kanban" => icons::KANBAN,
        _ => icons::GEAR_SIX,
    }
}

/// One section-list row: a 2px gold left edge and the text colour when
/// selected, muted otherwise; the unsaved count on the right.
fn nav_row(
    ui: &mut Ui,
    icon: &str,
    title: &str,
    dirty: usize,
    selected: bool,
    color: Option<Color32>,
) -> egui::Response {
    let p = theme::palette(ui.ctx());
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), 38.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(rect, 0.0, p.panel2);
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, vec2(2.0, rect.height())),
            0.0,
            p.gold,
        );
    } else if resp.hovered() {
        painter.rect_filled(rect, 0.0, p.panel2.gamma_multiply(0.6));
    }
    let fg = color.unwrap_or(if selected || resp.hovered() {
        p.text
    } else {
        p.muted
    });
    painter.text(
        egui::pos2(rect.left() + 14.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        icons::label(icon, title),
        egui::FontId::proportional(14.0),
        fg,
    );
    if dirty > 0 {
        painter.text(
            egui::pos2(rect.right() - 14.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            dirty.to_string(),
            egui::FontId::monospace(12.0),
            p.gold_text,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A joined row of options; the selected one is filled gold. Returns the
/// clicked option's stored value.
fn segmented(
    ui: &mut Ui,
    options: &[(&'static str, &'static str)],
    current: &str,
    width: f32,
) -> Option<String> {
    let p = theme::palette(ui.ctx());
    let n = options.len().max(1) as f32;
    let bw = (width / n).clamp(72.0, 120.0);
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (v, label) in options {
            let on = *v == current;
            let (fill, fg, edge) = if on {
                (p.gold, p.on_gold, p.gold)
            } else {
                (p.bg, p.text, p.line)
            };
            let b = egui::Button::new(RichText::new(*label).strong().color(fg))
                .fill(fill)
                .stroke(Stroke::new(1.0, edge))
                .min_size(vec2(bw, 36.0));
            if ui.add(b).clicked() {
                picked = Some(v.to_string());
            }
        }
    });
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_has_an_icon() {
        for s in config::sections() {
            assert_ne!(section_icon(s.id), icons::GEAR_SIX, "{}", s.id);
        }
    }
}

//! Provider Vault screen. Owner: governance workstream.
//!
//! Provider keys by provider, each with its test chip (untested / testing /
//! accepted / refused HTTP N / unreachable), integrity state and actions
//! (re-test, edit label, rotate, delete); the grant log and the vault
//! activity log. A key test is an indicator only: saving never waits for
//! it and is never refused because of it.

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme;
use egui_extras::{Column, TableBuilder};
use kls_core::vault::{
    self as v, ProbeResult, ProbeState, Provider, SecretMeta, SecretString, SecretVersion,
    VaultActivity, VaultView, connectors, kiro, probe,
};
use std::sync::{Arc, Mutex};

pub const READY: bool = true;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Keys,
    Grants,
    Mcp,
    Connectors,
    Activity,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Filter {
    #[default]
    All,
    Changes,
    Denied,
}

/// A one-shot background result the window polls each frame.
type Slot<T> = Arc<Mutex<Option<T>>>;
/// A save's outcome: on failure the reason and the typed value back.
type SaveResult = Result<(), (String, Option<SecretString>)>;
/// A key's lineage as it loads.
type Versions = Result<Vec<SecretVersion>, String>;

/// The add / rotate / label form. The typed value stays only until the save
/// succeeds; on an error it comes back so nothing typed is lost.
#[derive(Default)]
struct KeyForm {
    rotate_id: Option<String>,
    label_only: Option<String>,
    provider: String,
    env: String,
    label: String,
    label_touched: bool,
    value: String,
    reveal: bool,
    saving: Option<Slot<SaveResult>>,
    error: Option<String>,
    /// The in-dialog test of the typed value (never gates Save).
    test: Option<Slot<ProbeResult>>,
    tested: Option<ProbeResult>,
    /// (provider, value length) the test ran for; a change clears it.
    tested_for: (String, usize),
    /// A generic env-var secret instead of a provider key.
    env_kind: bool,
    /// Lifetime in days (0: never expires).
    expiry_days: i64,
    /// Nodes that may receive it (empty: every node).
    nodes: Vec<String>,
    /// A Kiro credential: a sign-in (JSON) or, with `kiro_api`, a kiro-cli
    /// API key.
    kiro: bool,
    kiro_api: bool,
    /// A sign-in file to open, and where the typed value came from.
    kiro_path: String,
    kiro_source: &'static str,
}

enum Confirm {
    Secret(String, String),
    Provider(String),
    Connector(String, String),
}

pub struct Screen {
    data: Loader<VaultView>,
    tab: Tab,
    filter: Filter,
    key_form: Option<KeyForm>,
    provider_form: Option<(Provider, String)>,
    confirm: Option<Confirm>,
    focus_key: Option<String>,
    access: Option<AccessForm>,
    /// A key's lineage window and its versions as they load.
    lineage: Option<(SecretMeta, Slot<Versions>)>,
    /// MCP secret bindings (#26).
    mcp: McpTab,
    /// Connector secrets and what opened each.
    connectors: Loader<Vec<connectors::Connector>>,
    connector_form: Option<ConnectorForm>,
}

/// Expiry, nodes and auto-rotation of one key.
struct AccessForm {
    id: String,
    provider: String,
    /// Preset lifetime in days from now (0: never), unless `custom` is set.
    days: i64,
    /// A custom expiry: a date, a date and time, or 30d.
    custom: String,
    nodes: Vec<String>,
    /// Auto-rotate every N days (0: never); mintable providers only.
    rotate_days: String,
    error: Option<String>,
}

/// The add / replace form of a connector secret. Typed values move into
/// zeroizing buffers on Save; a failed save asks for them again.
#[derive(Default)]
struct ConnectorForm {
    replace_id: Option<String>,
    kind: String,
    name: String,
    label: String,
    /// One (field, typed value) per field of the kind.
    fields: Vec<(String, String)>,
    reveal: bool,
    saving: Option<Slot<SaveResult>>,
    error: Option<String>,
}

impl ConnectorForm {
    fn new(kind: &str, existing: Option<&connectors::Connector>) -> Self {
        let k = connectors::kind(kind);
        Self {
            replace_id: existing.map(|c| c.meta.id.clone()),
            kind: kind.into(),
            name: existing.map(|c| c.name.clone()).unwrap_or_default(),
            fields: k
                .map(|k| {
                    k.fields
                        .iter()
                        .chain(k.optional)
                        .map(|f| (f.to_string(), String::new()))
                        .collect()
                })
                .unwrap_or_default(),
            ..Default::default()
        }
    }

    /// The core check of what is typed (the same `connectors::encode` the
    /// save runs), or `None` when it would save.
    fn rule(&self) -> Option<String> {
        let fields: Vec<(String, SecretString)> = self
            .fields
            .iter()
            .map(|(k, v)| (k.clone(), SecretString::new(v.as_str())))
            .collect();
        connectors::encode(&self.kind, &fields).err()
    }
}

/// The provider's mark (a Phosphor glyph from the embedded font).
pub fn provider_logo(id: &str) -> &'static str {
    match id {
        "openai" => "\u{E7D2}",            // open-ai-logo
        "google" | "gemini" => "\u{E292}", // google-logo
        "anthropic" => "\u{E0AA}",         // asterisk
        "mistral" => "\u{E5D2}",           // wind
        "openrouter" => "\u{E67C}",        // tree-structure
        "meta" | "llama" => "\u{ED02}",    // meta-logo
        "bedrock" | "aws" => "\u{E96C}",   // amazon-logo
        "azure" => "\u{E1AA}",             // cloud
        "kiro" => "\u{E62A}",              // ghost
        "env" => icons::TERMINAL,
        p if v::is_management_provider(p) => icons::FINGERPRINT,
        _ => icons::PLUGS_CONNECTED,
    }
}

const AUTH: [(&str, &str); 3] = [
    ("bearer", "Authorization: Bearer"),
    ("x-api-key", "x-api-key"),
    ("x-goog-api-key", "x-goog-api-key"),
];

impl Screen {
    pub fn new(model: &Model) -> Self {
        let data = Loader::new(model, &["vault.", "budget.provider"], |core| async move {
            v::vault_view(&core).await
        });
        Self {
            data,
            tab: Tab::default(),
            filter: Filter::default(),
            key_form: None,
            provider_form: None,
            confirm: None,
            focus_key: None,
            access: None,
            lineage: None,
            mcp: McpTab::new(model),
            connectors: Loader::new(model, &["vault."], |core| async move {
                connectors::list(&core).await
            }),
            connector_form: None,
        }
    }

    /// `activity`, `grants`, `add`, `add:<provider>`, or a key id.
    pub fn focus(&mut self, id: &str) {
        match id {
            "activity" => self.tab = Tab::Activity,
            "grants" => self.tab = Tab::Grants,
            "mcp" => self.tab = Tab::Mcp,
            "connectors" => self.tab = Tab::Connectors,
            "add" => self.key_form = Some(KeyForm::default()),
            id if id.starts_with("add:") || id.starts_with("key:") => {
                self.tab = Tab::Keys;
                let p = id.split_once(':').map(|(_, p)| p).unwrap_or("");
                self.key_form = Some(KeyForm {
                    provider: p.to_string(),
                    ..Default::default()
                });
            }
            id => {
                self.tab = Tab::Keys;
                self.focus_key = Some(id.to_string());
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let p = theme::palette(ui.ctx());
        let mut add = false;
        let mut new_provider = false;
        let mut test_all = false;
        let mut check_all = false;
        let mut as_code = false;
        kit::page_header(ui, icons::LOCK_SIMPLE, "Vault", |ui| {
            add = theme::primary_button(ui, &icons::label(icons::KEY, "Add key")).clicked();
            new_provider = kit::ghost_button(ui, &icons::label(icons::PLUS, "Provider")).clicked();
            test_all = kit::ghost_button(ui, &icons::label(icons::PLUGS_CONNECTED, "Test all"))
                .on_hover_text("One free request per key, in the background")
                .clicked();
            as_code = kit::ghost_button(ui, &icons::label(icons::CODE, "As code"))
                .on_hover_text("Every entry as YAML, never the values")
                .clicked();
            check_all = kit::ghost_button(ui, &icons::label(icons::SHIELD_CHECK, "Run checks"))
                .on_hover_text("Each key opens with the master key and matches its fingerprint")
                .clicked();
            ui.add_space(8.0);
            for (t, icon, label) in [
                (Tab::Activity, icons::SCROLL, "Activity"),
                (Tab::Connectors, icons::PLUGS_CONNECTED, "Connectors"),
                (Tab::Mcp, icons::PLUG, "MCP"),
                (Tab::Grants, icons::FINGERPRINT, "Grants"),
                (Tab::Keys, icons::KEY, "Keys"),
            ] {
                if kit::tab(ui, &icons::label(icon, label), self.tab == t).clicked() {
                    self.tab = t;
                }
            }
        });
        if add {
            self.key_form = Some(KeyForm::default());
        }
        if new_provider {
            self.provider_form = Some((
                Provider {
                    factory: true,
                    probe_auth: "bearer".into(),
                    ..Default::default()
                },
                String::new(),
            ));
        }
        if test_all {
            model.act(|m| async move {
                let n = probe::test_all(m.core()).await?;
                Ok(Some(Notice::Info(format!("Tested {n} keys"))))
            });
        }
        if as_code {
            model.act(|m| async move {
                let yaml = v::entries_as_code(m.core()).await?;
                let dir = m.core().data_dir().join("exports");
                std::fs::create_dir_all(&dir)?;
                let path = dir.join("vault-entries.yaml");
                std::fs::write(&path, yaml)?;
                Ok(Some(Notice::Info(format!(
                    "Entries (no values) in {}",
                    path.display()
                ))))
            });
        }
        if check_all {
            model.act(|m| async move {
                let (ok, bad) = v::verify_all(m.core()).await?;
                Ok(Some(if bad == 0 {
                    Notice::Info(format!("{ok} keys verified"))
                } else {
                    Notice::Error(format!("{bad} keys failed the check, {ok} verified"))
                }))
            });
        }
        let view = match self.data.get() {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
                return;
            }
            Some(Ok(v)) => v,
        };
        if let Some(w) = &view.vault_warning {
            kit::chip(ui, &icons::label(icons::WARNING, w), p.amber);
            ui.add_space(6.0);
        }
        if let Some(e) = &view.vault_error {
            kit::chip(ui, &icons::label(icons::X_CIRCLE, e), p.red);
            ui.add_space(6.0);
        }
        stats(ui, &view);
        ui.add_space(12.0);
        egui::ScrollArea::vertical()
            .id_salt("vault_page")
            .auto_shrink([false, false])
            .show(ui, |ui| match self.tab {
                Tab::Keys => self.keys(ui, model, &view),
                Tab::Grants => grants(ui, model, &view),
                Tab::Mcp => self.mcp.ui(ui, model, &view),
                Tab::Connectors => self.connectors_tab(ui),
                Tab::Activity => self.activity(ui, &view),
            });
        let ctx = ui.ctx().clone();
        self.key_dialog(&ctx, model, &view);
        self.provider_dialog(&ctx, model);
        self.confirm_dialog(&ctx, model);
        self.access_dialog(&ctx, model);
        self.lineage_dialog(&ctx, model);
        self.connector_dialog(&ctx, model);
        // A test under way shows a spinner; repaint until it settles.
        let now = kls_store::now_ms();
        if view
            .secrets
            .iter()
            .any(|s| probe::effective(s, now).state == ProbeState::Testing)
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    // ---- keys --------------------------------------------------------------

    fn keys(&mut self, ui: &mut egui::Ui, model: &Model, view: &VaultView) {
        let p = theme::palette(ui.ctx());
        let mut act: Option<KeyAct> = None;
        for pr in &view.providers {
            let keys: Vec<_> = view
                .secrets
                .iter()
                .filter(|s| s.provider == pr.id)
                .collect();
            kit::section(
                ui,
                provider_logo(&pr.id),
                &pr.name,
                false,
                |ui| {
                    ui.menu_button(icons::DOTS_THREE, |ui| {
                        if ui
                            .button(icons::label(icons::PENCIL_SIMPLE, "Edit"))
                            .clicked()
                        {
                            self.provider_form = Some((pr.clone(), pr.models.join(", ")));
                            ui.close();
                        }
                        if ui.button(icons::label(icons::TRASH, "Delete")).clicked() {
                            self.confirm = Some(Confirm::Provider(pr.id.clone()));
                            ui.close();
                        }
                    })
                    .response
                    .on_hover_text("More");
                    if kit::ghost_button(ui, &icons::label(icons::KEY, "Add key")).clicked() {
                        self.key_form = Some(KeyForm {
                            provider: pr.id.clone(),
                            env: pr.env.clone(),
                            ..Default::default()
                        });
                    }
                    if pr.factory {
                        kit::chip(ui, &icons::label(icons::CHECK, "Factory"), p.green)
                            .on_hover_text("Factory work may use this provider");
                    } else {
                        kit::chip(ui, &icons::label(icons::PROHIBIT, "Factory off"), p.amber);
                    }
                    if probe::target(pr).is_none() {
                        kit::chip(ui, &icons::label(icons::MINUS, "No test"), p.muted)
                            .on_hover_text("Set a test address on the provider");
                    }
                    ui.label(kit::mono(&pr.id, 12.0, p.muted));
                },
                |ui| {
                    if keys.is_empty() {
                        ui.label(
                            egui::RichText::new(icons::label(icons::KEY, "No key")).color(p.muted),
                        )
                        .on_hover_text("Nodes use their own configuration");
                    }
                    for k in keys {
                        kit::hline_full(ui, p.line);
                        let focused = self.focus_key.as_deref() == Some(k.id.as_str());
                        if let Some(a) = key_row(ui, k, &pr.name, focused) {
                            act = Some(a);
                        }
                    }
                },
            );
            ui.add_space(12.0);
        }
        let orphans: Vec<_> = view
            .secrets
            .iter()
            .filter(|s| !view.providers.iter().any(|p| p.id == s.provider))
            .filter(|s| connectors::kind_of(&s.provider).is_none())
            .collect();
        if !orphans.is_empty() {
            kit::section(
                ui,
                icons::KEY,
                "Keys without a provider",
                false,
                |_| {},
                |ui| {
                    for (i, k) in orphans.into_iter().enumerate() {
                        if i > 0 {
                            kit::hline_full(ui, p.line);
                        }
                        if let Some(a) = key_row(ui, k, &k.provider, false) {
                            act = Some(a);
                        }
                    }
                },
            );
        }
        match act {
            Some(KeyAct::Test(k)) => {
                let id = k.id.clone();
                model.act(move |m| async move {
                    probe::probe_stored(m.core(), &id, "owner").await?;
                    Ok(None)
                });
            }
            Some(KeyAct::Check(k)) => {
                let id = k.id.clone();
                model.act(move |m| async move {
                    let e = v::verify_secret(m.core(), &id).await?;
                    Ok(Some(if e.is_empty() {
                        Notice::Info("Key verified".into())
                    } else {
                        Notice::Error(format!("Check failed: {e}"))
                    }))
                });
            }
            Some(KeyAct::Access(k)) => {
                let days = k
                    .expires_at
                    .map(|e| ((e - kls_store::now_ms()).max(0) + 86_399_999) / 86_400_000)
                    .unwrap_or(0);
                self.access = Some(AccessForm {
                    id: k.id.clone(),
                    provider: k.provider.clone(),
                    days,
                    custom: String::new(),
                    nodes: k.nodes.clone(),
                    rotate_days: k.rotate_every_days.to_string(),
                    error: None,
                });
            }
            Some(KeyAct::Lineage(k)) => {
                let slot: Slot<Versions> = Default::default();
                let (s2, id) = (slot.clone(), k.id.clone());
                model.act(move |m| async move {
                    let r = v::lineage::versions(m.core(), &id)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    *s2.lock().unwrap() = Some(r);
                    Ok(None)
                });
                self.lineage = Some((k, slot));
            }
            Some(KeyAct::ApplyModels(k)) => {
                let id = k.id.clone();
                model.act(move |m| async move {
                    let added = probe::apply_discovered_models(m.core(), &id).await?;
                    Ok(Some(Notice::Info(if added.is_empty() {
                        "Models already listed".into()
                    } else {
                        format!("{} models added", added.len())
                    })))
                });
            }
            Some(KeyAct::Label(k)) => {
                self.key_form = Some(KeyForm {
                    label_only: Some(k.id.clone()),
                    provider: k.provider.clone(),
                    env: k.env.clone(),
                    label: k.label.clone(),
                    label_touched: true,
                    ..Default::default()
                });
            }
            Some(KeyAct::Rotate(k)) => {
                self.key_form = Some(KeyForm {
                    rotate_id: Some(k.id.clone()),
                    provider: k.provider.clone(),
                    env: k.env.clone(),
                    label: k.label.clone(),
                    label_touched: true,
                    ..Default::default()
                });
            }
            Some(KeyAct::Delete(k)) => {
                self.confirm = Some(Confirm::Secret(
                    k.id.clone(),
                    format!("{} {}", k.provider, k.env),
                ));
            }
            None => {}
        }
    }

    // ---- activity ------------------------------------------------------------

    fn activity(&mut self, ui: &mut egui::Ui, view: &VaultView) {
        let p = theme::palette(ui.ctx());
        ui.horizontal(|ui| {
            for (f, label) in [
                (Filter::All, "All"),
                (Filter::Changes, "Changes"),
                (Filter::Denied, "Denied"),
            ] {
                if kit::pill(ui, label, self.filter == f).clicked() {
                    self.filter = f;
                }
            }
        });
        ui.add_space(6.0);
        let rows: Vec<&VaultActivity> = view
            .activity
            .iter()
            .filter(|a| match self.filter {
                Filter::All => true,
                Filter::Changes => a.change,
                Filter::Denied => a.denied,
            })
            .collect();
        if rows.is_empty() {
            kit::empty(ui, icons::SCROLL, "No activity");
            return;
        }
        let mut lines = Vec::new();
        TableBuilder::new(ui)
            .id_salt("vault_activity")
            .striped(false)
            .vscroll(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::auto().at_least(140.0))
            .column(Column::auto().at_least(90.0))
            .column(Column::auto().at_least(110.0))
            .column(Column::remainder().at_least(160.0))
            .column(Column::auto().at_least(80.0))
            .header(28.0, |mut h| {
                for t in ["When", "What", "Provider", "Result", "By"] {
                    lines.push(h.col(|ui| kit::th(ui, t)).0);
                }
            })
            .body(|mut body| {
                for a in rows {
                    body.row(28.0, |mut row| {
                        row.col(|ui| {
                            ui.label(egui::RichText::new(fmt::when(a.at)).color(p.muted));
                        });
                        row.col(|ui| {
                            let c = if a.denied {
                                p.red
                            } else if a.change {
                                p.text
                            } else {
                                p.muted
                            };
                            ui.label(egui::RichText::new(&a.what).color(c));
                        });
                        row.col(|ui| {
                            ui.label(&a.provider);
                        });
                        row.col(|ui| {
                            ui.add(
                                egui::Label::new(egui::RichText::new(&a.result).color(p.muted))
                                    .truncate(),
                            );
                        });
                        row.col(|ui| {
                            ui.label(egui::RichText::new(&a.by).color(p.muted));
                        });
                        lines.push(row.response().rect);
                    });
                }
            });
        kit::row_lines(ui, &lines, p.line);
    }

    // ---- dialogs -------------------------------------------------------------

    fn key_dialog(&mut self, ctx: &egui::Context, model: &Model, view: &VaultView) {
        let Some(form) = &mut self.key_form else {
            return;
        };
        let p = theme::palette(ctx);
        // Settle a save that finished.
        if let Some(slot) = &form.saving {
            let done = slot.lock().unwrap().take();
            match done {
                Some(Ok(())) => {
                    self.key_form = None;
                    return;
                }
                Some(Err((e, value))) => {
                    form.error = Some(e);
                    if let Some(v) = value {
                        form.value = v.expose().to_string();
                    }
                    form.saving = None;
                }
                None => {}
            }
        }
        let finished = form.test.as_ref().and_then(|s| s.lock().unwrap().take());
        if let Some(r) = finished {
            form.tested = Some(r);
            form.test = None;
        }
        // A change of provider or value drops the last test.
        let now_for = (form.provider.clone(), form.value.trim().len());
        if form.tested.is_some() && form.tested_for != now_for {
            form.tested = None;
        }
        let saving = form.saving.is_some();
        let label_only = form.label_only.is_some();
        let rotating = form.rotate_id.is_some();
        let mut open = true;
        let mut save = false;
        let mut test = false;
        let kiro_login = kiro::is_login(&form.provider, &form.env);
        // A sign-in file opened from the path field or dropped on the window.
        let mut open_file: Option<std::path::PathBuf> = None;
        if kiro_login && !label_only {
            open_file = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        }
        let title = if label_only {
            icons::label(icons::PENCIL_SIMPLE, "Edit label")
        } else if rotating {
            icons::label(icons::ARROWS_CLOCKWISE, "Rotate key")
        } else {
            icons::label(icons::KEY, "Add key")
        };
        let existing = (!rotating && !label_only)
            .then(|| {
                view.secrets
                    .iter()
                    .find(|s| s.provider == form.provider && s.env == form.env.trim())
                    .cloned()
            })
            .flatten();
        let provider = view
            .providers
            .iter()
            .find(|p| p.id == form.provider)
            .cloned();
        let nodes: Vec<(String, String)> = model
            .snapshot()
            .nodes
            .iter()
            .filter(|n| n.revoked_at.is_none())
            .map(|n| (n.id.clone(), n.name.clone()))
            .collect();
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.add_enabled_ui(!saving, |ui| {
                    if !rotating && !label_only {
                        ui.horizontal(|ui| {
                            if kit::pill(
                                ui,
                                &icons::label(icons::KEY, "Provider key"),
                                !form.env_kind,
                            )
                            .clicked()
                            {
                                form.env_kind = false;
                                form.kiro = false;
                                form.provider.clear();
                            }
                            if kit::pill(
                                ui,
                                &icons::label(icons::TERMINAL, "Env var"),
                                form.env_kind,
                            )
                            .on_hover_text("A token for a tool or MCP server, not a model provider")
                            .clicked()
                            {
                                form.env_kind = true;
                                form.kiro = false;
                            }
                            if kit::pill(
                                ui,
                                &icons::label(provider_logo(kiro::PROVIDER), "Kiro"),
                                form.kiro,
                            )
                            .on_hover_text("A Kiro sign-in or a kiro-cli API key")
                            .clicked()
                            {
                                form.kiro = true;
                                form.env_kind = false;
                            }
                        });
                        if form.env_kind {
                            form.provider = v::ENV_PROVIDER.to_string();
                        }
                        if form.kiro {
                            ui.horizontal(|ui| {
                                if kit::pill(
                                    ui,
                                    &icons::label(icons::FILE_TEXT, "Sign-in (JSON)"),
                                    !form.kiro_api,
                                )
                                .on_hover_text(kiro::LOGIN_HINT)
                                .clicked()
                                {
                                    form.kiro_api = false;
                                }
                                if kit::pill(
                                    ui,
                                    &icons::label(icons::KEY, "API key"),
                                    form.kiro_api,
                                )
                                .on_hover_text("kiro-cli headless (ksk_…)")
                                .clicked()
                                {
                                    form.kiro_api = true;
                                }
                            });
                            form.provider = kiro::PROVIDER.to_string();
                            form.env = if form.kiro_api {
                                kiro::API_KEY_ENV
                            } else {
                                kiro::LOGIN_ENV
                            }
                            .to_string();
                            if !form.label_touched {
                                form.label = if form.kiro_api {
                                    "kiro-cli"
                                } else {
                                    "kiro-signin"
                                }
                                .into();
                            }
                        }
                        ui.add_space(4.0);
                    }
                    egui::Grid::new("vault_key_form")
                        .num_columns(2)
                        .show(ui, |ui| {
                            ui.label("Provider");
                            if rotating || label_only || form.env_kind || form.kiro {
                                ui.label(&form.provider);
                            } else {
                                let before = form.provider.clone();
                                egui::ComboBox::from_id_salt("vault_key_provider")
                                    .selected_text(if form.provider.is_empty() {
                                        "choose"
                                    } else {
                                        form.provider.as_str()
                                    })
                                    .show_ui(ui, |ui| {
                                        for p in &view.providers {
                                            ui.selectable_value(
                                                &mut form.provider,
                                                p.id.clone(),
                                                &p.name,
                                            );
                                        }
                                    });
                                if before != form.provider
                                    && let Some(p) =
                                        view.providers.iter().find(|p| p.id == form.provider)
                                {
                                    form.env = p.env.clone();
                                    if !form.label_touched {
                                        form.label = format!("{}-prod", p.id);
                                    }
                                }
                            }
                            ui.end_row();
                            ui.label("Env var");
                            if rotating || label_only || form.kiro {
                                ui.monospace(&form.env);
                            } else {
                                ui.add(
                                    egui::TextEdit::singleline(&mut form.env)
                                        .hint_text("ANTHROPIC_API_KEY"),
                                );
                            }
                            ui.end_row();
                            if !rotating {
                                ui.label("Label");
                                if ui.text_edit_singleline(&mut form.label).changed() {
                                    form.label_touched = true;
                                }
                                ui.end_row();
                            }
                            if !label_only {
                                ui.label(if kiro_login { "Sign-in" } else { "Key" });
                                ui.horizontal(|ui| {
                                    let edit = if kiro_login {
                                        egui::TextEdit::multiline(&mut form.value)
                                            .desired_rows(4)
                                            .code_editor()
                                    } else {
                                        egui::TextEdit::singleline(&mut form.value)
                                    };
                                    if ui
                                        .add(edit.password(!form.reveal).desired_width(300.0))
                                        .changed()
                                    {
                                        form.kiro_source = "paste";
                                    }
                                    let eye = if form.reveal {
                                        icons::EYE_SLASH
                                    } else {
                                        icons::EYE
                                    };
                                    if kit::ghost_button(ui, eye)
                                        .on_hover_text(if form.reveal { "Hide" } else { "Show" })
                                        .clicked()
                                    {
                                        form.reveal = !form.reveal;
                                    }
                                });
                                ui.end_row();
                                if kiro_login {
                                    ui.label("");
                                    ui.horizontal(|ui| {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut form.kiro_path)
                                                .hint_text("/path/to/kiro-signin.json")
                                                .desired_width(240.0),
                                        );
                                        if kit::ghost_button(
                                            ui,
                                            &icons::label(icons::FOLDER, "Open"),
                                        )
                                        .on_hover_text("Or drop the file on the window")
                                        .clicked()
                                        {
                                            open_file = Some(form.kiro_path.trim().into());
                                        }
                                    });
                                    ui.end_row();
                                }
                            }
                        });
                    if kiro_login && !label_only && !form.value.trim().is_empty() {
                        kiro_status(ui, &form.value);
                    }
                    if !rotating && !label_only {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(icons::label(icons::CLOCK, "Expires"))
                                    .color(p.muted),
                            );
                            egui::ComboBox::from_id_salt("vault_key_expiry")
                                .selected_text(expiry_label(form.expiry_days))
                                .show_ui(ui, |ui| {
                                    for d in [0, 1, 7, 30, 90, 365] {
                                        ui.selectable_value(
                                            &mut form.expiry_days,
                                            d,
                                            expiry_label(d),
                                        );
                                    }
                                });
                        });
                        if !nodes.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new(icons::label(icons::HARD_DRIVES, "Nodes"))
                                        .color(p.muted),
                                )
                                .on_hover_text("None ticked: every node");
                                for (id, name) in &nodes {
                                    let mut on = form.nodes.contains(id);
                                    if ui.checkbox(&mut on, name).changed() {
                                        if on {
                                            form.nodes.push(id.clone());
                                        } else {
                                            form.nodes.retain(|n| n != id);
                                        }
                                    }
                                }
                            });
                        }
                    }
                });
                if let Some(e) = &existing {
                    kit::chip(
                        ui,
                        &icons::label(
                            icons::WARNING,
                            &format!(
                                "Replaces the key added {}",
                                fmt::ago(kls_store::now_ms(), e.created_at)
                            ),
                        ),
                        p.amber,
                    );
                }
                if !label_only {
                    ui.horizontal(|ui| {
                        let can_test = !form.value.trim().is_empty()
                            && (kiro_login
                                || provider
                                    .as_ref()
                                    .is_some_and(|p| probe::target(p).is_some()));
                        ui.add_enabled_ui(can_test && form.test.is_none(), |ui| {
                            if kit::ghost_button(ui, &icons::label(icons::PLUGS_CONNECTED, "Test"))
                                .on_hover_text("Optional. Saving never waits for it.")
                                .clicked()
                            {
                                test = true;
                            }
                        });
                        if form.test.is_some() {
                            ui.spinner();
                        } else if let Some(r) = &form.tested {
                            test_chip(ui, r);
                        } else if !kiro_login
                            && provider
                                .as_ref()
                                .is_some_and(|p| probe::target(p).is_none())
                        {
                            kit::chip(ui, &icons::label(icons::MINUS, "No test address"), p.muted);
                        }
                    });
                }
                ui.weak(icons::label(icons::LOCK_SIMPLE, "Sealed, write-only"))
                    .on_hover_text("Not shown again after you save");
                if let Some(e) = &form.error {
                    ui.colored_label(p.red, icons::label(icons::X_CIRCLE, e));
                }
                ui.horizontal(|ui| {
                    // Save depends on the form only: never on the test.
                    // A sign-in must parse (a form rule, like a required
                    // field); the AWS test still never gates the save.
                    let ready = !saving
                        && !form.provider.is_empty()
                        && (label_only
                            || (!form.value.trim().is_empty()
                                && (!kiro_login || kiro::login_rule(&form.value).is_empty())));
                    ui.add_enabled_ui(ready, |ui| {
                        let label = if label_only {
                            icons::label(icons::FLOPPY_DISK, "Save")
                        } else if rotating {
                            icons::label(icons::ARROWS_CLOCKWISE, "Rotate")
                        } else {
                            icons::label(icons::FLOPPY_DISK, "Save")
                        };
                        if theme::primary_button(ui, &label).clicked() {
                            save = true;
                        }
                    });
                    if saving {
                        ui.spinner();
                    }
                });
            });
        if let Some(path) = open_file.filter(|p| !p.as_os_str().is_empty()) {
            match kiro::read_login_file(&path) {
                Ok(v) => {
                    form.value = v.expose().to_string();
                    form.kiro_source = "file";
                    form.error = None;
                }
                Err(e) => form.error = Some(format!("{e:#}")),
            }
        }
        if test && kiro_login {
            // Checked here only: a mint could rotate the typed refresh token.
            form.tested_for = now_for.clone();
            form.tested = Some(kiro::check_typed(&form.value));
        } else if test && let Some(pr) = provider.clone() {
            let slot: Slot<ProbeResult> = Arc::default();
            let s2 = slot.clone();
            let value = SecretString::new(form.value.trim());
            let ctx2 = ctx.clone();
            form.tested_for = now_for.clone();
            model.rt.spawn(async move {
                let r = probe::probe_key(&pr, &value).await;
                *s2.lock().unwrap() = Some(r);
                ctx2.request_repaint();
            });
            form.test = Some(slot);
        }
        if save {
            form.error = None;
            let slot: Slot<SaveResult> = Arc::default();
            let s2 = slot.clone();
            let ctx2 = ctx.clone();
            // Move the buffer into a zeroizing SecretString; it comes back
            // only if the save fails.
            let value = SecretString::new(std::mem::take(&mut form.value));
            let (rotate, label_id, provider, env, label) = (
                form.rotate_id.clone(),
                form.label_only.clone(),
                form.provider.clone(),
                form.env.clone(),
                form.label.clone(),
            );
            let m = model.clone();
            let (env_kind, days, nodes) = (form.env_kind, form.expiry_days, form.nodes.clone());
            let source = if form.kiro_source.is_empty() {
                "paste"
            } else {
                form.kiro_source
            };
            model.rt.spawn(async move {
                let core = m.core();
                let res = match (label_id, rotate) {
                    (Some(id), _) => v::set_secret_label(core, &id, &label).await.map(|_| ()),
                    (None, Some(id)) => {
                        v::rotate_secret(core, &id, value.clone()).await.map(|_| ())
                    }
                    (None, None) => {
                        let saved = if env_kind {
                            v::put_env_secret(core, &env, &label, value.clone()).await
                        } else if kiro::is_login(&provider, &env) {
                            kiro::put_login(core, &label, value.clone(), source)
                                .await
                                .map(|(m, _)| m)
                        } else if provider == kiro::PROVIDER && env == kiro::API_KEY_ENV {
                            kiro::put_api_key(core, &label, value.clone()).await
                        } else {
                            v::put_secret(core, &provider, &env, &label, value.clone()).await
                        };
                        match saved {
                            Ok(k) if days > 0 || !nodes.is_empty() => {
                                let exp =
                                    (days > 0).then(|| kls_store::now_ms() + days * 86_400_000);
                                v::set_secret_scope(core, &k.id, exp, &nodes)
                                    .await
                                    .map(|_| ())
                            }
                            Ok(_) => Ok(()),
                            Err(e) => Err(e),
                        }
                    }
                };
                let out = match res {
                    Ok(()) => {
                        m.notify(Notice::Info("Saved".into()));
                        Ok(())
                    }
                    Err(e) => Err((format!("{e:#}"), Some(value))),
                };
                *s2.lock().unwrap() = Some(out);
                ctx2.request_repaint();
            });
            form.saving = Some(slot);
        }
        if !open && !saving {
            self.key_form = None;
        }
    }

    fn provider_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some((p, models)) = &mut self.provider_form else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let new = p.created_at == 0 && p.name.is_empty();
        egui::Window::new(icons::label(icons::PLUGS_CONNECTED, "Provider"))
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("vault_provider_form")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Id");
                        if new {
                            ui.add(egui::TextEdit::singleline(&mut p.id).hint_text("slug"));
                        } else {
                            ui.monospace(&p.id);
                        }
                        ui.end_row();
                        ui.label("Name");
                        ui.text_edit_singleline(&mut p.name);
                        ui.end_row();
                        ui.label("Key variable");
                        ui.text_edit_singleline(&mut p.env);
                        ui.end_row();
                        ui.label("Default model");
                        ui.text_edit_singleline(&mut p.default_model);
                        ui.end_row();
                        ui.label("Fast model");
                        ui.text_edit_singleline(&mut p.fast_model);
                        ui.end_row();
                        ui.label("Models");
                        ui.add(egui::TextEdit::singleline(models).hint_text("a, b"));
                        ui.end_row();
                        ui.label("Test address");
                        ui.add(
                            egui::TextEdit::singleline(&mut p.probe_url)
                                .hint_text("https://…/v1/models")
                                .desired_width(300.0),
                        )
                        .on_hover_text("A free request that checks a key, such as a models list");
                        ui.end_row();
                        ui.label("Key header");
                        egui::ComboBox::from_id_salt("vault_probe_auth")
                            .selected_text(
                                AUTH.iter()
                                    .find(|(k, _)| *k == p.probe_auth)
                                    .map(|(_, l)| *l)
                                    .unwrap_or("Authorization: Bearer"),
                            )
                            .show_ui(ui, |ui| {
                                for (k, l) in AUTH {
                                    ui.selectable_value(&mut p.probe_auth, k.to_string(), l);
                                }
                            });
                        ui.end_row();
                        ui.label("Factory");
                        ui.checkbox(&mut p.factory, "Allowed")
                            .on_hover_text("Factory work may use this provider");
                        ui.end_row();
                    });
                if theme::primary_button(ui, &icons::label(icons::FLOPPY_DISK, "Save")).clicked() {
                    save = true;
                }
            });
        if save {
            let mut p = p.clone();
            if p.name.trim().is_empty() {
                p.name = p.id.clone();
            }
            p.models = models
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            model.act(move |m| async move {
                v::save_provider(m.core(), &p).await?;
                Ok(None)
            });
            open = false;
        }
        if !open {
            self.provider_form = None;
        }
    }

    /// Expiry and node scope of one key (its value is untouched).
    fn access_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(form) = &mut self.access else {
            return;
        };
        let AccessForm {
            id,
            provider,
            days,
            custom,
            nodes,
            rotate_days,
            error,
        } = form;
        let p = theme::palette(ctx);
        let mintable = v::lineage::MINTABLE.contains(&provider.as_str());
        let all: Vec<(String, String)> = model
            .snapshot()
            .nodes
            .iter()
            .filter(|n| n.revoked_at.is_none())
            .map(|n| (n.id.clone(), n.name.clone()))
            .collect();
        let mut open = true;
        let mut save = false;
        egui::Window::new(icons::label(icons::HARD_DRIVES, "Access"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(icons::label(icons::CLOCK, "Expires in"))
                            .color(p.muted),
                    );
                    ui.add_enabled_ui(custom.trim().is_empty(), |ui| {
                        egui::ComboBox::from_id_salt("vault_access_expiry")
                            .selected_text(expiry_label(*days))
                            .show_ui(ui, |ui| {
                                for d in [0, 1, 7, 30, 90, 365] {
                                    ui.selectable_value(days, d, expiry_label(d));
                                }
                            });
                    });
                });
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(icons::label(icons::CALENDAR, "Custom")).color(p.muted),
                    )
                    .on_hover_text("A date (UTC end of day), a date and time, or 30d. Overrides the preset.");
                    ui.add(
                        egui::TextEdit::singleline(custom)
                            .hint_text("2027-01-31")
                            .desired_width(160.0),
                    );
                });
                if mintable {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(icons::label(icons::ARROWS_CLOCKWISE, "Auto-rotate"))
                                .color(p.muted),
                        )
                        .on_hover_text(format!(
                            "Days between rotations (0: off). Mints a key with the management key filed under {}.",
                            v::lineage::management_slug(provider)
                        ));
                        ui.add(egui::TextEdit::singleline(rotate_days).desired_width(60.0));
                        ui.label(egui::RichText::new("days").color(p.muted));
                    });
                }
                if let Some(e) = error.as_ref() {
                    ui.colored_label(p.red, icons::label(icons::X_CIRCLE, e));
                }
                ui.label(
                    egui::RichText::new(icons::label(icons::HARD_DRIVES, "Nodes")).color(p.muted),
                )
                .on_hover_text("None ticked: every node");
                if all.is_empty() {
                    ui.weak("No nodes joined");
                }
                for (nid, name) in &all {
                    let mut on = nodes.contains(nid);
                    if ui.checkbox(&mut on, name).changed() {
                        if on {
                            nodes.push(nid.clone());
                        } else {
                            nodes.retain(|n| n != nid);
                        }
                    }
                }
                ui.add_space(6.0);
                if theme::primary_button(ui, &icons::label(icons::FLOPPY_DISK, "Save")).clicked() {
                    save = true;
                }
            });
        if save {
            let now = kls_store::now_ms();
            let exp = if custom.trim().is_empty() {
                Ok((*days > 0).then(|| now + *days * 86_400_000))
            } else {
                v::lineage::parse_expiry(custom, now).map_err(|e| format!("{e:#}"))
            };
            let rot = if mintable {
                rotate_days
                    .trim()
                    .parse::<i64>()
                    .map_err(|_| "auto-rotate: whole days".to_string())
            } else {
                Ok(0)
            };
            match (exp, rot) {
                (Ok(exp), Ok(rot)) => {
                    let (id, nodes) = (id.clone(), nodes.clone());
                    model.act(move |m| async move {
                        v::set_secret_scope(m.core(), &id, exp, &nodes).await?;
                        if mintable {
                            v::lineage::set_rotation(m.core(), &id, rot).await?;
                        }
                        Ok(Some(Notice::Info("Access saved".into())))
                    });
                    open = false;
                }
                (Err(e), _) | (_, Err(e)) => *error = Some(e),
            }
        }
        if !open {
            self.access = None;
        }
    }

    /// A key's versions: fingerprint, origin, dates and grace; roll back
    /// while the previous value is still sealed.
    fn lineage_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some((k, slot)) = &self.lineage else {
            return;
        };
        let p = theme::palette(ctx);
        let now = kls_store::now_ms();
        let got = slot.lock().unwrap().clone();
        let mut open = true;
        let mut roll = false;
        egui::Window::new(icons::label(
            icons::ARROW_COUNTER_CLOCKWISE,
            &format!("Lineage · {} {}", k.provider, k.env),
        ))
        .collapsible(false)
        .resizable(true)
        .default_width(560.0)
        .open(&mut open)
        .show(ctx, |ui| match &got {
            None => {
                ui.spinner();
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
            }
            Some(Ok(vs)) => {
                if vs.is_empty() {
                    ui.weak("No versions recorded");
                }
                for x in vs {
                    ui.horizontal_wrapped(|ui| {
                        let live = x.retired_at.is_none();
                        kit::chip(
                            ui,
                            &format!("v{}", x.version),
                            if live { p.green } else { p.muted },
                        );
                        ui.label(kit::mono(&x.fingerprint, 12.0, p.text))
                            .on_hover_text("sha256 of the value, first 16 hex");
                        if !x.origin.is_empty() {
                            ui.label(
                                egui::RichText::new(icons::label(icons::USER, &x.origin))
                                    .size(12.0)
                                    .color(p.muted),
                            );
                        }
                        ui.label(
                            egui::RichText::new(fmt::when(x.created_at))
                                .size(12.0)
                                .color(p.muted),
                        );
                        if live {
                            kit::chip(ui, &icons::label(icons::CHECK, "Live"), p.green);
                        } else if x.sealed && x.grace_until.is_some_and(|g| g > now) {
                            kit::chip(ui, &icons::label(icons::HOURGLASS, "In grace"), p.amber)
                                .on_hover_text(format!(
                                    "Sealed until {}",
                                    x.grace_until.map(fmt::when).unwrap_or_default()
                                ));
                        } else {
                            kit::chip(ui, &icons::label(icons::TRASH, "Shredded"), p.muted)
                                .on_hover_text(
                                    x.retired_at
                                        .map(|r| format!("Retired {}", fmt::when(r)))
                                        .unwrap_or_default(),
                                );
                        }
                        if !x.remote_id.is_empty() {
                            ui.label(kit::mono(&x.remote_id, 11.0, p.muted))
                                .on_hover_text("The provider's handle of a minted key");
                        }
                    });
                }
                ui.add_space(6.0);
                let can = v::lineage::can_roll_back(vs, now);
                ui.add_enabled_ui(can, |ui| {
                    if kit::ghost_button(ui, &icons::label(icons::ARROW_U_UP_LEFT, "Roll back"))
                        .on_hover_text("Restore the newest value still in its grace")
                        .clicked()
                    {
                        roll = true;
                    }
                });
            }
        });
        if roll {
            let id = k.id.clone();
            model.act(move |m| async move {
                let r = v::lineage::rollback(m.core(), &id).await?;
                Ok(Some(Notice::Info(format!("Rolled back to v{}", r.version))))
            });
            open = false;
        }
        if !open {
            self.lineage = None;
        }
    }

    // ---- connectors ------------------------------------------------------------

    /// Connector secrets: what Keleustes itself uses to reach a tool. Never
    /// granted to a node; replaced or revoked here.
    fn connectors_tab(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        ui.horizontal(|ui| {
            if theme::primary_button(ui, &icons::label(icons::PLUS, "Connector")).clicked() {
                self.connector_form = Some(ConnectorForm::new("slack", None));
            }
            ui.weak(icons::label(icons::PROHIBIT, "Never granted to a node"));
        });
        ui.add_space(8.0);
        let list = match self.connectors.get() {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
                return;
            }
            Some(Ok(l)) => l,
        };
        if list.is_empty() {
            kit::empty(ui, icons::PLUGS_CONNECTED, "No connector secrets");
            return;
        }
        let mut act: Option<(bool, connectors::Connector)> = None;
        TableBuilder::new(ui)
            .id_salt("vault_connectors")
            .striped(false)
            .column(Column::remainder().at_least(200.0))
            .column(Column::exact(60.0))
            .column(Column::remainder().at_least(140.0))
            .column(Column::exact(200.0))
            .header(22.0, |mut h| {
                for t in ["Secret", "Version", "Used by", ""] {
                    h.col(|ui| kit::th(ui, t));
                }
            })
            .body(|mut b| {
                for c in &list {
                    b.row(34.0, |mut r| {
                        r.col(|ui| {
                            ui.vertical(|ui| {
                                ui.label(if c.meta.label.is_empty() {
                                    &c.name
                                } else {
                                    &c.meta.label
                                });
                                ui.label(kit::mono(
                                    &format!("{} · {}", c.kind_label, c.name),
                                    11.0,
                                    p.muted,
                                ));
                            });
                        });
                        r.col(|ui| {
                            ui.label(format!("v{}", c.meta.version.max(1)));
                        });
                        r.col(|ui| {
                            if c.used_by.is_empty() {
                                ui.weak("Nothing");
                            } else {
                                ui.label(c.used_by.join(", "));
                            }
                        });
                        r.col(|ui| {
                            if kit::ghost_button(
                                ui,
                                &icons::label(icons::ARROWS_CLOCKWISE, "Replace"),
                            )
                            .clicked()
                            {
                                act = Some((true, c.clone()));
                            }
                            if kit::danger_button(ui, &icons::label(icons::TRASH, "Revoke"))
                                .clicked()
                            {
                                act = Some((false, c.clone()));
                            }
                        });
                    });
                }
            });
        match act {
            Some((true, c)) => {
                self.connector_form = Some(ConnectorForm::new(&c.kind, Some(&c)));
            }
            Some((false, c)) => {
                self.confirm = Some(Confirm::Connector(
                    c.meta.id.clone(),
                    format!("{} {}", c.kind_label, c.name),
                ));
            }
            None => {}
        }
    }

    fn connector_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(form) = &mut self.connector_form else {
            return;
        };
        let p = theme::palette(ctx);
        if let Some(slot) = &form.saving {
            let done = slot.lock().unwrap().take();
            match done {
                Some(Ok(())) => {
                    self.connector_form = None;
                    self.connectors.reload();
                    return;
                }
                Some(Err((e, _))) => {
                    form.error = Some(e);
                    form.saving = None;
                }
                None => {}
            }
        }
        let saving = form.saving.is_some();
        let replacing = form.replace_id.is_some();
        let mut open = true;
        let mut save = false;
        let title = if replacing {
            icons::label(icons::ARROWS_CLOCKWISE, "Replace value")
        } else {
            icons::label(icons::PLUGS_CONNECTED, "Add connector")
        };
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.add_enabled_ui(!saving, |ui| {
                    egui::Grid::new("vault_connector_form")
                        .num_columns(2)
                        .show(ui, |ui| {
                            ui.label("Kind");
                            if replacing {
                                ui.label(connectors::kind(&form.kind).map_or("", |k| k.label));
                            } else {
                                let before = form.kind.clone();
                                egui::ComboBox::from_id_salt("vault_connector_kind")
                                    .selected_text(
                                        connectors::kind(&form.kind).map_or("choose", |k| k.label),
                                    )
                                    .show_ui(ui, |ui| {
                                        for k in &connectors::KINDS {
                                            ui.selectable_value(
                                                &mut form.kind,
                                                k.id.to_string(),
                                                k.label,
                                            );
                                        }
                                    });
                                if before != form.kind {
                                    *form = ConnectorForm::new(&form.kind.clone(), None);
                                }
                            }
                            ui.end_row();
                            if !replacing {
                                ui.label("Name");
                                ui.add(
                                    egui::TextEdit::singleline(&mut form.name)
                                        .hint_text("ops")
                                        .desired_width(240.0),
                                );
                                ui.end_row();
                                ui.label("Label");
                                ui.add(
                                    egui::TextEdit::singleline(&mut form.label)
                                        .desired_width(240.0),
                                );
                                ui.end_row();
                            }
                            let required =
                                connectors::kind(&form.kind).map_or(&[][..], |k| k.fields);
                            for (name, value) in &mut form.fields {
                                let opt = !required.contains(&name.as_str());
                                ui.label(if opt {
                                    format!("{name} (optional)")
                                } else {
                                    name.clone()
                                });
                                ui.add(
                                    egui::TextEdit::singleline(value)
                                        .password(
                                            !form.reveal && name != "email" && name != "username",
                                        )
                                        .desired_width(240.0),
                                );
                                ui.end_row();
                            }
                        });
                    let eye = if form.reveal {
                        icons::label(icons::EYE_SLASH, "Hide")
                    } else {
                        icons::label(icons::EYE, "Show")
                    };
                    if kit::ghost_button(ui, &eye).clicked() {
                        form.reveal = !form.reveal;
                    }
                });
                ui.weak(icons::label(
                    icons::LOCK_SIMPLE,
                    "Sealed, write-only, never granted",
                ));
                let rule = form.rule();
                if let Some(e) = form.error.as_ref().or(rule.as_ref()) {
                    ui.colored_label(p.red, icons::label(icons::X_CIRCLE, e));
                }
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!saving && rule.is_none(), |ui| {
                        if theme::primary_button(ui, &icons::label(icons::FLOPPY_DISK, "Save"))
                            .clicked()
                        {
                            save = true;
                        }
                    });
                    if saving {
                        ui.spinner();
                    }
                });
            });
        if save {
            form.error = None;
            let slot: Slot<SaveResult> = Arc::default();
            let s2 = slot.clone();
            let ctx2 = ctx.clone();
            // The typed values move into zeroizing buffers now.
            let fields: Vec<(String, SecretString)> = form
                .fields
                .iter_mut()
                .map(|(k, v)| (k.clone(), SecretString::new(std::mem::take(v))))
                .collect();
            let (id, kind, name, label) = (
                form.replace_id.clone(),
                form.kind.clone(),
                form.name.clone(),
                form.label.clone(),
            );
            let m = model.clone();
            model.rt.spawn(async move {
                let core = m.core();
                let res = match id {
                    Some(id) => connectors::replace(core, &id, &fields).await.map(|_| ()),
                    None => connectors::put(core, &kind, &name, &label, &fields, "vault")
                        .await
                        .map(|_| ()),
                };
                let out = match res {
                    Ok(()) => {
                        m.notify(Notice::Info("Saved".into()));
                        Ok(())
                    }
                    Err(e) => Err((format!("{e:#}"), None)),
                };
                *s2.lock().unwrap() = Some(out);
                ctx2.request_repaint();
            });
            form.saving = Some(slot);
        }
        if !open && !saving {
            self.connector_form = None;
        }
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(c) = &self.confirm else {
            return;
        };
        let text = match c {
            Confirm::Secret(_, name) => format!("Delete key {name}?"),
            Confirm::Provider(id) => format!("Remove provider {id}? Keys stay."),
            Confirm::Connector(_, name) => format!("Revoke {name}? What uses it stops working."),
        };
        let mut done = false;
        egui::Window::new(icons::label(icons::WARNING, "Confirm"))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(text);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if kit::danger_button(ui, &icons::label(icons::TRASH, "Delete")).clicked() {
                        let c = self.confirm.take();
                        model.act(move |m| async move {
                            match c {
                                Some(Confirm::Secret(id, _)) => {
                                    v::delete_secret(m.core(), &id).await?;
                                }
                                Some(Confirm::Provider(id)) => {
                                    v::delete_provider(m.core(), &id).await?;
                                }
                                Some(Confirm::Connector(id, _)) => {
                                    connectors::revoke(m.core(), &id).await?;
                                }
                                None => {}
                            }
                            Ok(None)
                        });
                        done = true;
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        done = true;
                    }
                });
            });
        if done {
            self.confirm = None;
        }
    }
}

/// The four stat tiles: keys, accepted, refused or unreachable, verified.
fn stats(ui: &mut egui::Ui, view: &VaultView) {
    let p = theme::palette(ui.ctx());
    let now = kls_store::now_ms();
    let states: Vec<ProbeState> = view
        .secrets
        .iter()
        .map(|s| probe::effective(s, now).state)
        .collect();
    let accepted = states
        .iter()
        .filter(|s| **s == ProbeState::Accepted)
        .count();
    let bad = states
        .iter()
        .filter(|s| matches!(s, ProbeState::Refused | ProbeState::Unreachable))
        .count();
    let verified = view
        .secrets
        .iter()
        .filter(|s| s.verified_at.is_some() && s.verify_error.is_empty())
        .count();
    let failed = view
        .secrets
        .iter()
        .filter(|s| !s.verify_error.is_empty())
        .count();
    let last = view.secrets.iter().filter_map(|s| s.verified_at).max();
    let n = view.secrets.len();
    let expiring = view
        .secrets
        .iter()
        .filter(|s| v::lifetime(s, now) == v::Lifetime::Expiring)
        .count();
    let expired = view
        .secrets
        .iter()
        .filter(|s| v::lifetime(s, now) == v::Lifetime::Expired)
        .count();
    let tiles: [(&str, &str, String, String, Option<egui::Color32>); 5] = [
        (
            icons::CLOCK,
            "Expiring",
            expiring.to_string(),
            format!("14 days · {expired} expired"),
            (expired > 0)
                .then_some(p.red)
                .or((expiring > 0).then_some(p.amber)),
        ),
        (
            icons::KEY,
            "Keys",
            n.to_string(),
            format!("{} providers", view.providers.len()),
            None,
        ),
        (
            icons::CHECK_CIRCLE,
            "Accepted",
            accepted.to_string(),
            "last test".into(),
            Some(p.green),
        ),
        (
            icons::WARNING_CIRCLE,
            "Refused",
            bad.to_string(),
            "refused or unreachable".into(),
            (bad > 0).then_some(p.red),
        ),
        (
            icons::SHIELD_CHECK,
            "Verified",
            format!("{verified}/{n}"),
            match last {
                Some(t) => format!("checked {}", fmt::ago(now, t)),
                None => "not checked".into(),
            },
            (failed > 0).then_some(p.red),
        ),
    ];
    kit::Grid::new(180.0).show(ui, tiles.len(), |ui, i, w| {
        let (icon, label, value, note, color) = &tiles[i];
        kit::kpi_tile(ui, w, icon, label, value, Some(note), *color);
    });
}

/// The test chip for a result: accepted N ms / refused HTTP N / unreachable
/// / skipped, the provider's reason on hover.
/// What a typed Kiro sign-in is: the rule it breaks, or its region, kind
/// and registration expiry. Never a token.
fn kiro_status(ui: &mut egui::Ui, raw: &str) {
    let p = theme::palette(ui.ctx());
    let now = kls_store::now_ms();
    match kiro::parse_login(raw, now) {
        Err(why) => {
            ui.colored_label(p.red, icons::label(icons::WARNING, &why));
        }
        Ok((_, s)) => {
            ui.horizontal_wrapped(|ui| {
                kit::chip(ui, &icons::label(icons::CHECK, "Sign-in"), p.green);
                kit::chip(ui, &icons::label(icons::GLOBE, &s.region), p.muted);
                let kind = if s.identity_center {
                    "Identity Center"
                } else {
                    "Builder ID"
                };
                kit::chip(ui, &icons::label(icons::USER, kind), p.muted);
                if let Some(e) = s.registration_expires_at {
                    kit::chip(
                        ui,
                        &icons::label(
                            icons::CLOCK,
                            &format!("Registered until {}", fmt::when(e).get(..10).unwrap_or("?")),
                        ),
                        if e - now < v::EXPIRING_MS {
                            p.amber
                        } else {
                            p.muted
                        },
                    );
                }
            });
        }
    }
}

pub fn test_chip(ui: &mut egui::Ui, r: &ProbeResult) -> egui::Response {
    let p = theme::palette(ui.ctx());
    let now = kls_store::now_ms();
    let when = if r.at > 0 {
        format!(" · {}", fmt::ago(now, r.at))
    } else {
        String::new()
    };
    let (icon, text, color) = match r.state {
        ProbeState::Untested => (icons::CIRCLE, "Untested".to_string(), p.muted),
        ProbeState::Testing => (icons::CIRCLE_NOTCH, "Testing".to_string(), p.cyan),
        ProbeState::Accepted => (
            icons::CHECK_CIRCLE,
            match r.ms {
                Some(ms) => format!("Accepted · {ms} ms"),
                None => "Accepted".into(),
            },
            p.green,
        ),
        ProbeState::Refused => (
            icons::X_CIRCLE,
            match r.status {
                Some(s) => format!("Refused · HTTP {s}"),
                None => "Refused".into(),
            },
            p.red,
        ),
        ProbeState::Unreachable => (
            icons::WARNING,
            match r.status {
                Some(s) => format!("Unreachable · HTTP {s}"),
                None => "Unreachable".into(),
            },
            p.amber,
        ),
        ProbeState::Skipped => (icons::MINUS, "Not testable".to_string(), p.muted),
    };
    let resp = kit::chip(ui, &icons::label(icon, &text), color);
    let mut hover = String::new();
    if !r.detail.is_empty() {
        hover.push_str(&r.detail);
    }
    if r.at > 0 {
        if !hover.is_empty() {
            hover.push('\n');
        }
        hover.push_str(&format!("Tested {}{when}", fmt::when(r.at)));
    }
    if hover.is_empty() {
        hover = "A test only shows whether the provider accepts the key".into();
    }
    resp.on_hover_text(hover)
}

enum KeyAct {
    Test(SecretMeta),
    Check(SecretMeta),
    Label(SecretMeta),
    Rotate(SecretMeta),
    Delete(SecretMeta),
    Access(SecretMeta),
    Lineage(SecretMeta),
    ApplyModels(SecretMeta),
}

/// One sealed key: provider, variable, label and a fixed mask; its test chip
/// and integrity; re-test, check, label, rotate and delete.
fn key_row(ui: &mut egui::Ui, k: &SecretMeta, provider: &str, focused: bool) -> Option<KeyAct> {
    let p = theme::palette(ui.ctx());
    let now = kls_store::now_ms();
    let mut act = None;
    ui.add_space(4.0);
    let r = ui.horizontal_wrapped(|ui| {
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(icons::label(provider_logo(&k.provider), provider)).strong(),
            );
            let mut s = format!("{} · ••••••••", k.env);
            if !k.label.is_empty() {
                s.push_str(&format!(" · {}", k.label));
            }
            ui.label(kit::mono(&s, 12.0, p.muted));
        });
        let t = probe::effective(k, now);
        if t.state == ProbeState::Testing {
            ui.spinner();
        }
        test_chip(ui, &t);
        match v::lifetime(k, now) {
            v::Lifetime::Expired => {
                kit::chip(ui, &icons::label(icons::CLOCK, "Expired"), p.red).on_hover_text(
                    "Refused at grant time; the value is kept so it can be extended",
                );
            }
            v::Lifetime::Expiring => {
                kit::chip(ui, &icons::label(icons::CLOCK, "Expiring"), p.amber)
                    .on_hover_text(k.expires_at.map(fmt::when).unwrap_or_default());
            }
            v::Lifetime::Serving => {}
        }
        if !k.nodes.is_empty() {
            kit::chip(
                ui,
                &icons::label(icons::HARD_DRIVES, &k.nodes.len().to_string()),
                p.muted,
            )
            .on_hover_text(format!("Only for {}", k.nodes.join(", ")));
        }
        if k.kind == "env" {
            kit::chip(ui, &icons::label(icons::TERMINAL, "env"), p.cyan);
        }
        if k.version > 1 {
            kit::chip(ui, &format!("v{}", k.version), p.muted)
                .on_hover_text(format!("Fingerprint {}", k.fingerprint));
        }
        if k.rotate_every_days > 0 {
            kit::chip(
                ui,
                &icons::label(
                    icons::ARROWS_CLOCKWISE,
                    &format!("{}d", k.rotate_every_days),
                ),
                p.cyan,
            )
            .on_hover_text("Auto-rotates with the management key");
        }
        if !k.models.is_empty() {
            kit::chip(
                ui,
                &icons::label(icons::SPARKLE, &format!("{} models", k.models.len())),
                p.cyan,
            )
            .on_hover_text(
                k.models
                    .iter()
                    .take(20)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            if kit::ghost_button(ui, &icons::label(icons::PLUS, "Models"))
                .on_hover_text("Add the models this key lists to the provider")
                .clicked()
            {
                act = Some(KeyAct::ApplyModels(k.clone()));
            }
        }
        if !k.verify_error.is_empty() {
            kit::chip(
                ui,
                &icons::label(icons::SHIELD_WARNING, "Check failed"),
                p.red,
            )
            .on_hover_text(&k.verify_error);
        } else if let Some(at) = k.verified_at {
            kit::chip(ui, &icons::label(icons::SHIELD_CHECK, "Verified"), p.green).on_hover_text(
                format!(
                    "Opens with the master key · fingerprint {} · {}",
                    k.fingerprint,
                    fmt::when(at)
                ),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if kit::danger_button(ui, icons::TRASH)
                .on_hover_text(format!("Delete {provider} key"))
                .clicked()
            {
                act = Some(KeyAct::Delete(k.clone()));
            }
            if kit::ghost_button(ui, &icons::label(icons::ARROWS_CLOCKWISE, "Rotate")).clicked() {
                act = Some(KeyAct::Rotate(k.clone()));
            }
            if kit::ghost_button(ui, icons::ARROW_COUNTER_CLOCKWISE)
                .on_hover_text("Lineage and roll back")
                .clicked()
            {
                act = Some(KeyAct::Lineage(k.clone()));
            }
            if kit::ghost_button(ui, icons::HARD_DRIVES)
                .on_hover_text("Expiry, nodes, auto-rotate")
                .clicked()
            {
                act = Some(KeyAct::Access(k.clone()));
            }
            if kit::ghost_button(ui, icons::PENCIL_SIMPLE)
                .on_hover_text("Edit label")
                .clicked()
            {
                act = Some(KeyAct::Label(k.clone()));
            }
            if kit::ghost_button(ui, icons::SHIELD_CHECK)
                .on_hover_text("Run check")
                .clicked()
            {
                act = Some(KeyAct::Check(k.clone()));
            }
            ui.add_enabled_ui(t.state != ProbeState::Testing, |ui| {
                if kit::ghost_button(ui, icons::PLUGS_CONNECTED)
                    .on_hover_text("Test again")
                    .clicked()
                {
                    act = Some(KeyAct::Test(k.clone()));
                }
            });
            let when = match k.rotated_at {
                Some(r) => format!("rotated {}", fmt::ago(now, r)),
                None => format!("added {}", fmt::ago(now, k.created_at)),
            };
            ui.label(egui::RichText::new(when).size(12.0).color(p.muted))
                .on_hover_text(format!(
                    "Added {}{}{}",
                    fmt::when(k.created_at),
                    k.rotated_at
                        .map(|r| format!(" · rotated {}", fmt::when(r)))
                        .unwrap_or_default(),
                    k.last_used_at
                        .map(|r| format!(" · last granted {}", fmt::when(r)))
                        .unwrap_or_default(),
                ));
        });
    });
    if focused {
        r.response.scroll_to_me(Some(egui::Align::Center));
        ui.painter().rect_stroke(
            r.response.rect,
            0.0,
            egui::Stroke::new(1.0, p.gold_edge),
            egui::StrokeKind::Outside,
        );
    }
    ui.add_space(4.0);
    act
}

/// The grant log with revoke.
fn grants(ui: &mut egui::Ui, model: &Model, view: &VaultView) {
    let p = theme::palette(ui.ctx());
    let now = kls_store::now_ms();
    if view.grants.is_empty() {
        kit::empty(ui, icons::FINGERPRINT, "No grants");
        return;
    }
    let mut revoke: Option<String> = None;
    let mut lines = Vec::new();
    TableBuilder::new(ui)
        .id_salt("vault_grants")
        .striped(false)
        .vscroll(false)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::auto().at_least(140.0))
        .column(Column::auto().at_least(120.0))
        .column(Column::remainder().at_least(160.0))
        .column(Column::auto().at_least(90.0))
        .column(Column::auto().at_least(80.0))
        .header(28.0, |mut h| {
            for t in ["When", "Providers", "Purpose", "State", ""] {
                lines.push(h.col(|ui| kit::th(ui, t)).0);
            }
        })
        .body(|mut body| {
            for g in &view.grants {
                body.row(30.0, |mut row| {
                    row.col(|ui| {
                        ui.label(egui::RichText::new(fmt::when(g.created_at)).color(p.muted));
                    });
                    row.col(|ui| {
                        ui.label(g.providers.join(", "));
                    });
                    row.col(|ui| {
                        ui.label(egui::RichText::new(&g.purpose).color(p.muted));
                        if !g.missing.is_empty() {
                            kit::chip(
                                ui,
                                &icons::label(icons::WARNING, &g.missing.join(", ")),
                                p.amber,
                            )
                            .on_hover_text("No key");
                        }
                    });
                    let state = if g.revoked_at.is_some() {
                        "revoked"
                    } else if g.expires_at <= now {
                        "expired"
                    } else {
                        "live"
                    };
                    row.col(|ui| {
                        let c = kit::status_color(&p, state);
                        kit::dot(ui, c);
                        ui.label(egui::RichText::new(state).color(c));
                    });
                    row.col(|ui| {
                        if state == "live"
                            && kit::danger_button(ui, &icons::label(icons::PROHIBIT, "Revoke"))
                                .clicked()
                        {
                            revoke = Some(g.id.clone());
                        }
                    });
                    lines.push(row.response().rect);
                });
            }
        });
    kit::row_lines(ui, &lines, p.line);
    if let Some(id) = revoke {
        model.act(move |m| async move {
            v::revoke_grant(m.core(), &id).await?;
            Ok(Some(Notice::Info("Grant revoked".into())))
        });
    }
}

/// "Never", "1 day", "30 days".
fn expiry_label(days: i64) -> String {
    match days {
        0 => "Never".into(),
        1 => "1 day".into(),
        d => format!("{d} days"),
    }
}

// ---- MCP bindings ---------------------------------------------------------------

/// MCP secret bindings (#26): a vault env secret bound to the (server, key)
/// it feeds on the host. Only names are shown or stored; a dispatch that
/// picks a binding gets the value in its `VaultGrant.mcp` at send time.
struct McpTab {
    data: Loader<Vec<kls_core::sessions::start::StoredBinding>>,
    server: String,
    key: String,
    secret: String,
}

impl McpTab {
    fn new(model: &Model) -> Self {
        Self {
            data: Loader::new(model, &["vault."], |core| async move {
                kls_core::sessions::start::mcp_bindings(&core).await
            }),
            server: String::new(),
            key: String::new(),
            secret: String::new(),
        }
    }

    fn origin() -> kls_core::hosts::Origin {
        kls_core::hosts::Origin {
            actor: "owner".into(),
            surface: "vault/mcp".into(),
            ..Default::default()
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, model: &Model, view: &VaultView) {
        let p = theme::palette(ui.ctx());
        let env: Vec<&SecretMeta> = view
            .secrets
            .iter()
            .filter(|s| s.provider == v::ENV_PROVIDER)
            .collect();
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(icons::label(icons::PLUS, "Bind")).strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.server)
                    .hint_text("server")
                    .desired_width(120.0),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.key)
                    .hint_text("key")
                    .desired_width(100.0),
            );
            egui::ComboBox::from_id_salt("mcp_secret")
                .selected_text(icons::label(
                    icons::KEY,
                    if self.secret.is_empty() {
                        "env secret"
                    } else {
                        &self.secret
                    },
                ))
                .show_ui(ui, |ui| {
                    for s in &env {
                        ui.selectable_value(&mut self.secret, s.env.clone(), &s.env);
                    }
                });
            let b = kls_core::gateway::McpBinding {
                server: self.server.trim().into(),
                key: self.key.trim().into(),
                secret: self.secret.clone(),
            };
            let ok = kls_core::sessions::start::check_binding(&b);
            let r = ui.add_enabled_ui(ok.is_ok(), |ui| {
                theme::primary_button(ui, &icons::label(icons::LINK, "Bind"))
            });
            let r = match &ok {
                Err(why) if !self.server.is_empty() => r.inner.on_disabled_hover_text(why),
                _ => r.inner,
            };
            if r.clicked() {
                self.server.clear();
                self.key.clear();
                model.act(move |m| async move {
                    kls_core::sessions::start::mcp_bind(m.core(), b, &Self::origin()).await?;
                    Ok(Some(Notice::Info("Bound".into())))
                });
            }
        });
        if env.is_empty() {
            ui.weak(icons::label(icons::KEY, "No env secrets"))
                .on_hover_text("Add key › Env var");
        }
        ui.add_space(8.0);
        let rows = match self.data.get() {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
                return;
            }
            Some(Ok(rows)) => rows,
        };
        if rows.is_empty() {
            kit::empty(ui, icons::PLUG, "No bindings");
            return;
        }
        let mut unbind: Option<(String, String)> = None;
        let mut lines = Vec::new();
        TableBuilder::new(ui)
            .id_salt("vault_mcp")
            .striped(false)
            .vscroll(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::auto().at_least(140.0))
            .column(Column::auto().at_least(160.0))
            .column(Column::remainder().at_least(160.0))
            .column(Column::auto().at_least(80.0))
            .header(28.0, |mut h| {
                for t in ["Server:key", "Secret", "Bound", ""] {
                    lines.push(h.col(|ui| kit::th(ui, t)).0);
                }
            })
            .body(|mut body| {
                for s in rows.iter() {
                    let b = &s.binding;
                    let missing = !env.iter().any(|e| e.env.eq_ignore_ascii_case(&b.secret));
                    body.row(30.0, |mut row| {
                        row.col(|ui| {
                            ui.label(kit::mono(&format!("{}:{}", b.server, b.key), 12.0, p.text));
                        });
                        row.col(|ui| {
                            ui.label(icons::label(icons::KEY, &b.secret));
                            if missing {
                                kit::chip(ui, &icons::label(icons::WARNING, "gone"), p.amber)
                                    .on_hover_text("No such env secret: nothing is sent");
                            }
                        });
                        row.col(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} · {}",
                                    fmt::when(s.at),
                                    if s.surface.is_empty() {
                                        s.by.as_str()
                                    } else {
                                        s.surface.as_str()
                                    }
                                ))
                                .color(p.muted),
                            )
                            .on_hover_text(format!("by {} from {}", s.by, s.surface));
                        });
                        row.col(|ui| {
                            if kit::danger_button(ui, &icons::label(icons::TRASH, "Unbind"))
                                .clicked()
                            {
                                unbind = Some((b.server.clone(), b.key.clone()));
                            }
                        });
                        lines.push(row.response().rect);
                    });
                }
            });
        kit::row_lines(ui, &lines, p.line);
        if let Some((server, key)) = unbind {
            model.act(move |m| async move {
                kls_core::sessions::start::mcp_unbind(m.core(), &server, &key, &Self::origin())
                    .await?;
                Ok(Some(Notice::Info("Unbound".into())))
            });
        }
    }
}

#[cfg(test)]
mod kiro_connector_ui_tests {
    use super::*;

    #[test]
    fn connector_form_lists_the_kind_fields_and_checks_like_the_save() {
        let mut f = ConnectorForm::new("webhook", None);
        let names: Vec<&str> = f.fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["url", "signingSecret", "previousSecret"]);
        assert!(f.rule().unwrap().contains("url"));
        f.fields[0].1 = "https://hooks.example.com/abc".into();
        f.fields[1].1 = "whsec-0123456789".into();
        assert!(f.rule().is_none(), "the optional field may stay empty");
        f.fields[0].1 = "http://hooks.example.com/abc".into();
        assert!(f.rule().unwrap().contains("https"));
        assert!(ConnectorForm::new("nope", None).rule().is_some());
    }

    #[test]
    fn kiro_status_renders_a_summary_or_the_rule() {
        let ctx = egui::Context::default();
        let good = serde_json::json!({"refresh_token":"r-0123456789","client_id":"c",
            "client_secret":"s-0123456789","region":"eu-central-1"})
        .to_string();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                kiro_status(ui, &good);
                kiro_status(ui, "{}");
            });
        });
        out.textures_delta.clear();
        assert!(kiro::login_rule(&good).is_empty());
        assert!(!kiro::login_rule("{}").is_empty());
        assert_eq!(provider_logo(kiro::PROVIDER), "\u{E62A}");
    }
}

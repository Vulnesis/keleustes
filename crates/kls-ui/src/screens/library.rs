//! Library screen. Owner: M5 vault and workforce workstream.
//!
//! Prompts, skills, commands and processes. Every save is a new version;
//! older versions can be opened and restored. Items import from and export
//! to Markdown files (front matter: kind, name, description).

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme;
use egui::{Align, Layout, Margin, RichText, Stroke, Ui, vec2};
use egui_extras::{Column, TableBuilder};
use kls_core::vault::{self as v, LIBRARY_KINDS, LibraryItem, LibraryVersion};
use std::path::PathBuf;

pub const READY: bool = true;

/// Space between panels.
const GAP: f32 = 16.0;

#[derive(Default)]
struct Editor {
    id: String,
    kind: String,
    name: String,
    description: String,
    body: String,
    note: String,
    /// Version the body was loaded from (0 for new or edited).
    loaded: i64,
}

pub struct Screen {
    items: Loader<Vec<LibraryItem>>,
    versions: Option<(String, Loader<Vec<LibraryVersion>>)>,
    kind: String,
    filter: String,
    selected: Option<String>,
    editor: Option<Editor>,
    /// Item id whose latest body should be loaded into the editor once its
    /// versions arrive.
    load_latest: Option<String>,
    io_path: String,
    confirm_delete: Option<(String, String)>,
    /// A new item just saved: select it by (kind, name) when it appears.
    pending_name: Option<(String, String)>,
    /// The Discover tab (library search, `library_search.rs`).
    discover: super::library_search::Discover,
    discover_on: bool,
    /// Where imported items came from (library search provenance).
    origins: Loader<
        Vec<(
            kls_store::libsearch::Provenance,
            Option<kls_store::libsearch::UpdateState>,
        )>,
    >,
    /// Hosts that take a library install.
    nodes: Loader<Vec<kls_store::Node>>,
    /// The install row: chosen node and how far the confirmations went.
    install: Install,
}

/// Installing the open item on a host. A hook or MCP server runs code, so
/// it needs a second click; a hook that carries files then needs the files
/// confirmed one by one (`Core::confirm_bundle_files`) before it is sent.
#[derive(Default)]
pub(crate) struct Install {
    node: String,
    /// 0 idle, 1 armed (runs code), 2 the files window is open.
    step: u8,
    files: Option<(BundleKey, Loader<Vec<kls_store::libsearch::BundleEntry>>)>,
}

/// The library item and version whose bundle files are being confirmed.
type BundleKey = (String, i64);

/// What the install button does next for an item of `kind` carrying
/// `files` bundle files at confirmation `step`: `Some(next step)` to ask
/// again, `None` to send.
pub(crate) fn install_next(kind: &str, files: usize, step: u8) -> Option<u8> {
    let code = matches!(kind, "hook" | "mcp");
    match step {
        0 if code => Some(1),
        1 if kind == "hook" && files > 0 => Some(2),
        _ => None,
    }
}

/// The second confirmation for a hook's bundle files, shared by the Host
/// screen's Library tab and the Fleet Library row: the files (path, size,
/// hash) are listed, and "Confirm files" records
/// `Core::confirm_bundle_files` before `hosts::library_dispatch`.
pub(crate) struct FilesConfirm {
    pub(crate) node: String,
    pub(crate) item: LibraryItem,
    files: Loader<Vec<kls_store::libsearch::BundleEntry>>,
}

/// What a [`FilesConfirm`] window decided this frame.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FilesStep {
    /// The file list is loading, or the person has not answered yet.
    Waiting,
    /// Send: `confirm` says whether files were confirmed first.
    Send {
        confirm: bool,
    },
    Cancel,
}

/// The step for a hook whose file list is `loaded` (`None` while loading)
/// and the person's answer (`Some(true)` Confirm, `Some(false)` Cancel).
pub(crate) fn files_step(loaded: Option<usize>, answer: Option<bool>) -> FilesStep {
    match (loaded, answer) {
        (_, Some(false)) => FilesStep::Cancel,
        (Some(0), _) => FilesStep::Send { confirm: false },
        (Some(_), Some(true)) => FilesStep::Send { confirm: true },
        _ => FilesStep::Waiting,
    }
}

impl FilesConfirm {
    pub(crate) fn open(model: &Model, node: &str, item: LibraryItem) -> Self {
        let (id, v) = (item.id.clone(), item.latest_version);
        let files = Loader::new(model, &["library."], move |core| {
            let id = id.clone();
            async move { core.store().bundle_files(&id, v).await }
        });
        FilesConfirm {
            node: node.to_string(),
            item,
            files,
        }
    }

    /// Shows the window (when the hook has files) and returns the decision.
    pub(crate) fn show(&mut self, ctx: &egui::Context) -> FilesStep {
        let files = self.files.get().and_then(|r| r.ok());
        let mut answer = None;
        if let Some(files) = files.as_ref().filter(|f| !f.is_empty()) {
            egui::Window::new(icons::label(icons::WARNING, "Confirm files"))
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(icons::label(
                        icons::FOLDER,
                        &format!("{} v{}", self.item.name, self.item.latest_version),
                    ));
                    for f in files {
                        ui.label(
                            RichText::new(format!(
                                "{}  {} B  {}",
                                f.path,
                                f.size_bytes,
                                f.sha256.chars().take(12).collect::<String>()
                            ))
                            .monospace()
                            .size(11.0),
                        )
                        .on_hover_text(&f.sha256);
                    }
                    ui.horizontal(|ui| {
                        if kit::danger_button(ui, &icons::label(icons::CHECK, "Confirm files"))
                            .clicked()
                        {
                            answer = Some(true);
                        }
                        if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                            answer = Some(false);
                        }
                    });
                });
        }
        files_step(files.map(|f| f.len()), answer)
    }

    /// Sends the install: the files' confirmation first when `confirm`.
    pub(crate) fn send(self, model: &Model, confirm: bool, origin: kls_core::hosts::Origin) {
        send_install(model, &self.node, &self.item, confirm, origin);
    }
}

/// Puts a library item on `node`: a hook's files are confirmed first when
/// `confirm` (the owner saw them), then `hosts::library_dispatch` sends it.
pub(crate) fn send_install(
    model: &Model,
    node: &str,
    it: &LibraryItem,
    confirm: bool,
    origin: kls_core::hosts::Origin,
) {
    let (node, it) = (node.to_string(), it.clone());
    model.act(move |m| async move {
        if confirm {
            m.core()
                .confirm_bundle_files(&node, &it.id, it.latest_version, &origin.label())
                .await?;
        }
        let kind = if it.kind == v::AGENT_KIND {
            "profile".to_string()
        } else {
            it.kind.clone()
        };
        kls_core::hosts::library_dispatch(
            m.core(),
            &node,
            "library_install",
            vec![kls_core::gateway::LibraryRef {
                kind,
                id: it.id.clone(),
                name: it.name.clone(),
                version: it.latest_version,
                ..Default::default()
            }],
            matches!(it.kind.as_str(), "hook" | "mcp"),
            origin,
        )
        .await?;
        Ok(Some(Notice::Info(format!("Sent {} {}", it.kind, it.name))))
    });
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        Self {
            items: Loader::new(model, &["library."], |core| async move {
                core.store().library_items("").await
            }),
            origins: Loader::new(model, &["library."], |core| async move {
                kls_core::libsearch::origins(&core).await
            }),
            versions: None,
            kind: String::new(),
            filter: String::new(),
            selected: None,
            editor: None,
            load_latest: None,
            io_path: default_dir(),
            confirm_delete: None,
            pending_name: None,
            discover: super::library_search::Discover::new(model),
            discover_on: false,
            nodes: Loader::new(model, &["node."], |core| async move {
                Ok(core
                    .store()
                    .nodes()
                    .await?
                    .into_iter()
                    .filter(|n| {
                        n.revoked_at.is_none()
                            && n.host.capabilities.takes_dispatch("library_install")
                    })
                    .collect())
            }),
            install: Install::default(),
        }
    }

    /// An item id, or a kind (`prompt`, `skill`, `command`, `process`).
    pub fn focus(&mut self, id: &str) {
        let k = id.trim_end_matches('s');
        if LIBRARY_KINDS.contains(&k) {
            self.kind = k.to_string();
        } else {
            self.selected = Some(id.to_string());
            self.load_latest = Some(id.to_string());
        }
    }

    fn versions_for(&mut self, model: &Model, id: &str) -> Option<Vec<LibraryVersion>> {
        if self.versions.as_ref().is_none_or(|(vid, _)| vid != id) {
            let item = id.to_string();
            let l = Loader::new(model, &["library."], move |core| {
                let item = item.clone();
                async move { core.store().library_versions(&item).await }
            });
            self.versions = Some((id.to_string(), l));
        }
        match self.versions.as_ref().and_then(|(_, l)| l.get()) {
            Some(Ok(v)) => Some(v),
            _ => None,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        kit::page_header(ui, icons::BOOKS, "Library", |ui| {
            if self.discover_on {
                self.discover.header_actions(ui, model);
            }
            if kit::tab(
                ui,
                &icons::label(icons::MAGNIFYING_GLASS, "Discover"),
                self.discover_on,
            )
            .clicked()
            {
                self.discover_on = true;
            }
            if kit::tab(ui, &icons::label(icons::BOOKS, "Items"), !self.discover_on).clicked() {
                self.discover_on = false;
            }
            if self.discover_on {
                return;
            }
            if theme::primary_button(ui, &icons::label(icons::PLUS, "New item")).clicked() {
                self.selected = None;
                self.editor = Some(Editor {
                    kind: if self.kind.is_empty() {
                        "prompt".into()
                    } else {
                        self.kind.clone()
                    },
                    ..Default::default()
                });
            }
            self.io_bar(ui, model);
        });
        if self.discover_on {
            self.discover.ui(ui, model);
            return;
        }
        let items = match self.items.get() {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                return;
            }
            Some(Ok(i)) => i,
        };
        if let Some((kind, name)) = &self.pending_name
            && let Some(it) = items.iter().find(|i| &i.kind == kind && &i.name == name)
        {
            self.selected = Some(it.id.clone());
            self.load_latest = Some(it.id.clone());
            self.pending_name = None;
        }
        ui.horizontal_wrapped(|ui| {
            if kit::pill(ui, &format!("All  {}", items.len()), self.kind.is_empty()).clicked() {
                self.kind.clear();
            }
            for k in LIBRARY_KINDS {
                let n = items.iter().filter(|i| i.kind == k).count();
                if kit::pill(ui, &format!("{}  {n}", kind_plural(k)), self.kind == k).clicked() {
                    self.kind = k.to_string();
                }
            }
            ui.add_space(8.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(icons::label(icons::FUNNEL, "Filter"))
                    .desired_width(180.0),
            );
        });
        ui.add_space(14.0);

        // Fill the editor from the latest version once it has loaded.
        if let Some(id) = self.load_latest.clone()
            && let Some(item) = items.iter().find(|i| i.id == id).cloned()
            && let Some(vs) = self.versions_for(model, &id)
        {
            let latest = vs.first().cloned().unwrap_or_default();
            self.editor = Some(Editor {
                id: item.id.clone(),
                kind: item.kind.clone(),
                name: item.name.clone(),
                description: item.description.clone(),
                body: latest.body,
                note: String::new(),
                loaded: latest.version,
            });
            self.load_latest = None;
        }
        let versions = self
            .editor
            .as_ref()
            .filter(|e| !e.id.is_empty())
            .map(|e| e.id.clone())
            .and_then(|id| self.versions_for(model, &id))
            .unwrap_or_default();

        egui::ScrollArea::vertical()
            .id_salt("library_page")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let w = ui.available_width();
                if w >= 460.0 + GAP + 380.0 {
                    let wa = (w - GAP) * 0.5;
                    let wb = w - GAP - wa;
                    let h = ui.available_height();
                    ui.horizontal_top(|ui| {
                        let sp = ui.spacing().item_spacing.x;
                        ui.allocate_ui_with_layout(
                            vec2(wa, h),
                            Layout::top_down(Align::Min),
                            |ui| {
                                ui.set_width(wa);
                                self.list_ui(ui, &items);
                            },
                        );
                        ui.add_space(GAP - sp);
                        ui.allocate_ui_with_layout(
                            vec2(wb, h),
                            Layout::top_down(Align::Min),
                            |ui| {
                                ui.set_width(wb);
                                self.aside_ui(ui, model, &versions);
                            },
                        );
                    });
                } else {
                    self.list_ui(ui, &items);
                    ui.add_space(GAP);
                    self.aside_ui(ui, model, &versions);
                }
            });
        self.confirm_ui(ui.ctx(), model);
    }

    /// The items table: name, type, version, last saved.
    fn list_ui(&mut self, ui: &mut Ui, items: &[LibraryItem]) {
        let pal = theme::palette(ui.ctx());
        let f = self.filter.to_lowercase();
        let rows: Vec<&LibraryItem> = items
            .iter()
            .filter(|i| {
                (self.kind.is_empty() || i.kind == self.kind)
                    && (f.is_empty()
                        || i.name.contains(&f)
                        || i.description.to_lowercase().contains(&f))
            })
            .collect();
        let now = kls_store::now_ms();
        let mut picked = None;
        kit::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if rows.is_empty() {
                egui::Frame::new()
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.weak(icons::label(icons::BOOKS, "No items"));
                    });
                return;
            }
            let origins = self.origins.get().and_then(|r| r.ok()).unwrap_or_default();
            TableBuilder::new(ui)
                .id_salt("library_items")
                .striped(false)
                .resizable(true)
                .sense(egui::Sense::click())
                .max_scroll_height(560.0)
                .column(Column::remainder().at_least(160.0).clip(true))
                .column(Column::initial(100.0))
                .column(Column::initial(80.0))
                .column(Column::initial(110.0))
                .column(Column::initial(150.0).clip(true))
                .column(Column::initial(90.0))
                .header(32.0, |mut h| {
                    for t in ["Name", "Type", "Version", "Saved", "Origin", "Pinned"] {
                        h.col(|ui| {
                            kit::rule(ui);
                            kit::th(ui, t);
                        });
                    }
                })
                .body(|body| {
                    body.rows(36.0, rows.len(), |mut row| {
                        let it = rows[row.index()];
                        row.set_selected(self.selected.as_deref() == Some(it.id.as_str()));
                        row.col(|ui| {
                            kit::rule(ui);
                            let r = ui
                                .add(egui::Label::new(RichText::new(&it.name).strong()).truncate());
                            if !it.description.is_empty() {
                                r.on_hover_text(&it.description);
                            }
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.label(RichText::new(cap(&it.kind)).color(pal.muted));
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.label(RichText::new(format!("v{}", it.latest_version)).monospace());
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.label(RichText::new(fmt::ago(now, it.updated_at)).color(pal.muted))
                                .on_hover_text(fmt::when(it.updated_at));
                        });
                        let origin = origins
                            .iter()
                            .find(|(p, _)| p.item_kind != "agent" && p.item_id == it.id);
                        row.col(|ui| {
                            kit::rule(ui);
                            match origin {
                                Some((p, upd)) => {
                                    ui.label(
                                        RichText::new(icons::label(icons::GLOBE, &p.source))
                                            .color(pal.muted),
                                    )
                                    .on_hover_text(format!("{} · {}", p.ecosystem, p.licence));
                                    if upd.as_ref().is_some_and(|u| u.state == "behind") {
                                        kit::chip(
                                            ui,
                                            &icons::label(icons::ARROW_CLOCKWISE, "update"),
                                            pal.amber,
                                        );
                                    }
                                }
                                None => {
                                    ui.label(
                                        RichText::new(icons::label(icons::USER, "local"))
                                            .color(pal.muted),
                                    );
                                }
                            }
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            if let Some((p, _)) = origin {
                                let pin = if !p.upstream_commit.is_empty() {
                                    p.upstream_commit.chars().take(10).collect::<String>()
                                } else {
                                    p.upstream_version.clone()
                                };
                                ui.label(RichText::new(pin).monospace().color(pal.muted))
                                    .on_hover_text(&p.upstream_sha256);
                            }
                        });
                        if row.response().clicked() {
                            picked = Some(it.id.clone());
                        }
                    });
                });
        });
        if let Some(id) = picked {
            self.selected = Some(id.clone());
            self.load_latest = Some(id);
            self.editor = None;
        }
    }

    /// Path field, Import, Export all and (with an item open) Export, laid
    /// right to left inside the page header.
    fn io_bar(&mut self, ui: &mut egui::Ui, model: &Model) {
        let sel = self
            .editor
            .as_ref()
            .filter(|e| !e.id.is_empty())
            .map(|e| (e.id.clone(), e.name.clone()));
        if let Some((id, name)) = sel
            && kit::ghost_button(ui, &icons::label(icons::EXPORT, "Export"))
                .on_hover_text("Latest version to path")
                .clicked()
        {
            let mut p = PathBuf::from(self.io_path.trim());
            if p.is_dir() || p.extension().is_none() {
                p = p.join(format!("{name}.md"));
            }
            model.act(move |m| async move {
                v::export_library_item(m.core(), &id, &p).await?;
                Ok(Some(Notice::Info(format!("Wrote {}", p.display()))))
            });
        }
        if kit::ghost_button(ui, &icons::label(icons::EXPORT, "Export all"))
            .on_hover_text("<folder>/<kind>/<name>.md")
            .clicked()
        {
            let p = PathBuf::from(self.io_path.trim());
            model.act(move |m| async move {
                let n = v::export_library(m.core(), &p).await?;
                Ok(Some(Notice::Info(format!(
                    "Exported {n} item(s) to {}",
                    p.display()
                ))))
            });
        }
        if kit::ghost_button(ui, &icons::label(icons::DOWNLOAD_SIMPLE, "Import file"))
            .on_hover_text("Read .md file or folder")
            .clicked()
        {
            let p = PathBuf::from(self.io_path.trim());
            model.act(move |m| async move {
                let n = v::import_library(m.core(), &p).await?;
                Ok(Some(Notice::Info(format!("Imported {n} item(s)"))))
            });
        }
        ui.add(
            egui::TextEdit::singleline(&mut self.io_path)
                .hint_text(icons::label(icons::FOLDER, ".md file or folder"))
                .desired_width(260.0),
        )
        .on_hover_text("Files");
    }

    /// The selected item in a glowing panel, or a placeholder.
    fn aside_ui(&mut self, ui: &mut Ui, model: &Model, versions: &[LibraryVersion]) {
        let pal = theme::palette(ui.ctx());
        kit::glow(ui).inner_margin(Margin::same(16)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if self.editor.is_none() {
                ui.label(
                    RichText::new(icons::label(icons::BOOK_OPEN, "No item selected"))
                        .color(pal.muted),
                );
                return;
            }
            self.editor_ui(ui, model, versions);
            self.install_ui(ui, model);
        });
    }

    /// Send the open item to a host (`hosts::library_dispatch`). A hook or
    /// MCP server needs a second click; a hook with files then opens a
    /// window listing each file and its hash, and only "Confirm files"
    /// records the confirmation and sends it.
    fn install_ui(&mut self, ui: &mut Ui, model: &Model) {
        let Some(e) = self
            .editor
            .as_ref()
            .filter(|e| !e.id.is_empty() && e.loaded > 0)
        else {
            return;
        };
        let (id, kind, name, version) = (e.id.clone(), e.kind.clone(), e.name.clone(), e.loaded);
        let nodes = self.nodes.get().and_then(|r| r.ok()).unwrap_or_default();
        if nodes.is_empty() {
            return;
        }
        let key = (id.clone(), version);
        if kind == "hook" && self.install.files.as_ref().is_none_or(|(k, _)| *k != key) {
            let (item, v) = key.clone();
            let l = Loader::new(model, &["library."], move |core| {
                let item = item.clone();
                async move { core.store().bundle_files(&item, v).await }
            });
            self.install.files = Some((key, l));
        }
        let files = match kind.as_str() {
            "hook" => self
                .install
                .files
                .as_ref()
                .and_then(|(_, l)| l.get())
                .and_then(|r| r.ok())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let pal = theme::palette(ui.ctx());
        ui.add_space(10.0);
        let mut send = false;
        ui.horizontal_wrapped(|ui| {
            let chosen = nodes.iter().find(|n| n.id == self.install.node);
            egui::ComboBox::from_id_salt("lib_install_node")
                .selected_text(icons::label(
                    icons::HARD_DRIVES,
                    chosen.map(|n| n.name.as_str()).unwrap_or("Host"),
                ))
                .show_ui(ui, |ui| {
                    for n in &nodes {
                        if ui
                            .selectable_value(&mut self.install.node, n.id.clone(), &n.name)
                            .changed()
                        {
                            self.install.step = 0;
                        }
                    }
                });
            let label = match self.install.step {
                1 => icons::label(icons::WARNING, "Install (runs code)"),
                _ => icons::label(icons::DOWNLOAD_SIMPLE, "Install"),
            };
            if ui
                .add_enabled_ui(chosen.is_some(), |ui| theme::primary_button(ui, &label))
                .inner
                .clicked()
            {
                match install_next(&kind, files.len(), self.install.step) {
                    Some(s) => self.install.step = s,
                    None => send = true,
                }
            }
            if self.install.step > 0
                && kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked()
            {
                self.install.step = 0;
            }
            if kind == "hook" && !files.is_empty() {
                ui.label(
                    RichText::new(icons::label(icons::FOLDER, &files.len().to_string()))
                        .color(pal.muted),
                )
                .on_hover_text("Files to confirm");
            }
        });
        if self.install.step == 2 {
            let mut close = false;
            egui::Window::new(icons::label(icons::WARNING, "Confirm files"))
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    ui.label(icons::label(icons::FOLDER, &format!("{name} v{version}")));
                    for f in &files {
                        ui.label(
                            RichText::new(format!(
                                "{}  {} B  {}",
                                f.path,
                                f.size_bytes,
                                f.sha256.chars().take(12).collect::<String>()
                            ))
                            .monospace()
                            .size(11.0),
                        )
                        .on_hover_text(&f.sha256);
                    }
                    ui.horizontal(|ui| {
                        if kit::danger_button(ui, &icons::label(icons::CHECK, "Confirm files"))
                            .clicked()
                        {
                            send = true;
                            close = true;
                        }
                        if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                            close = true;
                        }
                    });
                });
            if close {
                self.install.step = 0;
            }
        }
        if !send {
            return;
        }
        self.install.step = 0;
        let node = self.install.node.clone();
        let confirm_files = kind == "hook" && !files.is_empty();
        model.act(move |m| async move {
            let origin = kls_core::hosts::Origin::fleet("library");
            if confirm_files {
                m.core()
                    .confirm_bundle_files(&node, &id, version, &origin.label())
                    .await?;
            }
            let ref_kind = if kind == v::AGENT_KIND {
                "profile".to_string()
            } else {
                kind.clone()
            };
            kls_core::hosts::library_dispatch(
                m.core(),
                &node,
                "library_install",
                vec![kls_core::gateway::LibraryRef {
                    kind: ref_kind,
                    id: id.clone(),
                    name: name.clone(),
                    version,
                    ..Default::default()
                }],
                matches!(kind.as_str(), "hook" | "mcp"),
                origin,
            )
            .await?;
            Ok(Some(Notice::Info(format!("Sent {kind} {name}"))))
        });
    }

    fn editor_ui(&mut self, ui: &mut egui::Ui, model: &Model, versions: &[LibraryVersion]) {
        let pal = theme::palette(ui.ctx());
        let Some(e) = &mut self.editor else {
            return;
        };
        let mut delete = None;
        let mut restore: Option<LibraryVersion> = None;
        let ver = if e.id.is_empty() {
            "new".to_string()
        } else {
            format!("v{}", e.loaded)
        };
        ui.label(kit::caps(&format!("{} · {ver}", e.kind), pal.gold_text));
        self.discover.origin_chip(ui, &e.kind, &e.id);
        ui.add_space(2.0);
        ui.label(
            RichText::new(if e.name.is_empty() {
                "Untitled"
            } else {
                &e.name
            })
            .size(20.0)
            .strong(),
        );
        ui.add_space(8.0);
        egui::Grid::new("lib_form")
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Kind").size(12.0).color(pal.muted));
                egui::ComboBox::from_id_salt("lib_kind")
                    .selected_text(cap(&e.kind))
                    .show_ui(ui, |ui| {
                        for k in LIBRARY_KINDS {
                            ui.selectable_value(&mut e.kind, k.to_string(), cap(k));
                        }
                    });
                ui.end_row();
                ui.label(RichText::new("Name").size(12.0).color(pal.muted));
                ui.add(
                    egui::TextEdit::singleline(&mut e.name)
                        .hint_text("review-pr")
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
                ui.label(RichText::new("Description").size(12.0).color(pal.muted));
                ui.add(egui::TextEdit::singleline(&mut e.description).desired_width(f32::INFINITY));
                ui.end_row();
            });
        ui.add_space(8.0);
        egui::Frame::new()
            .fill(pal.bg)
            .stroke(Stroke::new(1.0, pal.line))
            .inner_margin(Margin::same(4))
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut e.body)
                        .font(egui::TextStyle::Monospace)
                        .frame(egui::Frame::NONE)
                        .background_color(pal.bg)
                        .desired_rows(16)
                        .desired_width(f32::INFINITY),
                );
            });
        if e.kind == "hook" {
            self.discover.bundle_ui(ui, model, &e.id, e.loaded);
        }
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(icons::label(icons::CLOCK, "Versions"))
                    .size(12.0)
                    .color(pal.muted),
            );
            if versions.is_empty() {
                ui.label(RichText::new("Unsaved").size(12.0).color(pal.muted));
            }
            for ver in versions {
                let cur = ver.version == e.loaded;
                let text = if cur {
                    format!("v{} (current)", ver.version)
                } else {
                    format!("v{}", ver.version)
                };
                let r = if cur {
                    kit::chip_filled(ui, &text)
                } else {
                    kit::chip(ui, &text, pal.text)
                };
                let mut hover = fmt::when(ver.created_at);
                if !ver.note.is_empty() {
                    hover.push_str(&format!(" · {}", ver.note));
                }
                if ui
                    .interact(r.rect, r.id.with("ver"), egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(hover)
                    .clicked()
                {
                    restore = Some(ver.clone());
                }
            }
        });
        if let Some(ver) = restore {
            e.body = ver.body;
            e.loaded = ver.version;
            e.note = format!("restored from v{}", ver.version);
        }
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            let valid = v::valid_name(e.name.trim());
            if ui
                .add_enabled_ui(valid, |ui| {
                    theme::primary_button(ui, &icons::label(icons::FLOPPY_DISK, "Save"))
                })
                .inner
                .clicked()
            {
                let (id, kind, name, desc, body, note) = (
                    e.id.clone(),
                    e.kind.clone(),
                    e.name.trim().to_string(),
                    e.description.clone(),
                    e.body.clone(),
                    e.note.clone(),
                );
                model.act(move |m| async move {
                    let (_, ver) =
                        v::save_library(m.core(), &id, &kind, &name, &desc, &body, &note).await?;
                    Ok(Some(Notice::Info(format!("Saved {name} v{ver}"))))
                });
                e.note.clear();
                if e.id.is_empty() {
                    // Reopen by name once the list reloads.
                    self.pending_name = Some((e.kind.clone(), e.name.trim().to_string()));
                }
            }
            ui.add(
                egui::TextEdit::singleline(&mut e.note)
                    .hint_text(icons::label(icons::NOTE, "Note"))
                    .desired_width(200.0),
            );
            if !valid && !e.name.is_empty() {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    icons::label(icons::WARNING, "a-z 0-9 - _ : ."),
                );
            }
            if !e.id.is_empty()
                && kit::danger_button(ui, &icons::label(icons::TRASH, "Delete")).clicked()
            {
                delete = Some((e.id.clone(), e.name.clone()));
            }
        });
        if delete.is_some() {
            self.confirm_delete = delete;
        }
    }

    fn confirm_ui(&mut self, ctx: &egui::Context, model: &Model) {
        let Some((id, name)) = self.confirm_delete.clone() else {
            return;
        };
        let mut done = false;
        egui::Window::new(icons::label(icons::TRASH, "Delete item"))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(icons::label(icons::WARNING, &format!("Delete {name}?")))
                    .on_hover_text("Removes all versions");
                ui.horizontal(|ui| {
                    if kit::danger_button(ui, &icons::label(icons::TRASH, "Delete")).clicked() {
                        model.act(move |m| async move {
                            v::delete_library(m.core(), &id).await?;
                            Ok(None)
                        });
                        self.editor = None;
                        self.selected = None;
                        done = true;
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        done = true;
                    }
                });
            });
        if done {
            self.confirm_delete = None;
        }
    }
}

/// The filter label for a library kind ("Processes", "MCP servers").
fn kind_plural(kind: &str) -> String {
    match kind {
        "process" => "Processes".into(),
        "mcp" => "MCP servers".into(),
        k => format!("{}s", cap(k)),
    }
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn default_dir() -> String {
    std::env::var("HOME")
        .map(|h| format!("{h}/keleustes-library"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{FilesStep, files_step, install_next};

    /// The Host and Fleet hook windows (P5 item 1): nothing is sent while the
    /// file list loads or the person has not answered; a hook with files is
    /// sent only after "Confirm files" (which records the confirmation), and
    /// one with none is sent at once without it.
    #[test]
    fn host_and_fleet_hook_files_wait_for_confirm_files() {
        assert_eq!(files_step(None, None), FilesStep::Waiting);
        assert_eq!(files_step(None, Some(true)), FilesStep::Waiting);
        assert_eq!(files_step(Some(2), None), FilesStep::Waiting);
        assert_eq!(
            files_step(Some(2), Some(true)),
            FilesStep::Send { confirm: true }
        );
        assert_eq!(files_step(Some(2), Some(false)), FilesStep::Cancel);
        assert_eq!(
            files_step(Some(0), None),
            FilesStep::Send { confirm: false }
        );
    }

    /// A hook with files installs only after the second confirmation: the
    /// first click arms, the second opens the files window, and only its
    /// "Confirm files" sends. Without files a hook or MCP server needs two
    /// clicks; a prompt one.
    #[test]
    fn a_hook_with_files_needs_the_second_confirmation() {
        assert_eq!(install_next("hook", 2, 0), Some(1));
        assert_eq!(install_next("hook", 2, 1), Some(2));
        assert_eq!(install_next("hook", 2, 2), None);
        assert_eq!(install_next("hook", 0, 0), Some(1));
        assert_eq!(install_next("hook", 0, 1), None);
        assert_eq!(install_next("mcp", 0, 0), Some(1));
        assert_eq!(install_next("mcp", 0, 1), None);
        assert_eq!(install_next("prompt", 0, 0), None);
    }
}

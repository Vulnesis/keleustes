//! Memory: the shared repository memory agents keep. Scopes on the left,
//! notes newest first on the right; add, edit, delete, clear a scope.
//! Notes are untrusted text and are only ever shown as plain text.

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme::{palette, primary_button};
use egui::{Align, Layout, Margin, RichText, Stroke, Ui};
use kls_core::board::{NOTE_TAGS_MAX, NOTE_TEXT_MAX, OWNER};
use kls_store::board::{MemoryNote, MemoryScope};

pub const READY: bool = true;

/// Space between the scope list and the notes.
const GAP: f32 = 16.0;

/// What the notes list shows.
#[derive(Clone, Debug, Default, PartialEq)]
struct Query {
    scope: Option<String>,
    q: String,
    deleted: bool,
}

#[derive(Clone, Default)]
struct NoteForm {
    /// None for a new note.
    id: Option<String>,
    scope: String,
    text: String,
    tags: String,
}

/// Notes per page.
const PAGE: usize = 50;
/// A scope group in the list: icon, title, and which scopes belong.
type ScopeGroup = (&'static str, &'static str, fn(&str) -> bool);

pub struct Screen {
    scopes: Loader<Vec<MemoryScope>>,
    notes: Loader<Vec<MemoryNote>>,
    query: Query,
    loaded: Query,
    form: Option<NoteForm>,
    /// The new-note panel's text.
    compose: NoteForm,
    confirm_delete: Option<MemoryNote>,
    confirm_clear: Option<String>,
    /// Notes shown (50 more per "More").
    shown: usize,
    /// Node id to name, for the host a note came from.
    nodes: Loader<Vec<(String, String)>>,
    /// Search and reindex across hosts (row 49).
    knowledge: super::knowledge::Window,
}

fn notes_loader(model: &Model, q: &Query) -> Loader<Vec<MemoryNote>> {
    let q = q.clone();
    Loader::new(model, &["memory."], move |core| {
        let q = q.clone();
        async move { core.notes(q.scope.as_deref(), &q.q, q.deleted).await }
    })
}

fn split_tags(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        let query = Query::default();
        Self {
            scopes: Loader::new(model, &["memory."], |core| async move {
                core.memory_scopes().await
            }),
            notes: notes_loader(model, &query),
            loaded: query.clone(),
            query,
            form: None,
            compose: NoteForm::default(),
            confirm_delete: None,
            confirm_clear: None,
            shown: PAGE,
            nodes: Loader::new(model, &["node."], |core| async move {
                Ok(core
                    .store()
                    .nodes()
                    .await?
                    .into_iter()
                    .map(|n| (n.id, n.name))
                    .collect())
            }),
            knowledge: super::knowledge::Window::new(model),
        }
    }

    /// A host's name, or its id when unknown.
    fn host_name(&self, id: &str) -> String {
        match self.nodes.get() {
            Some(Ok(v)) => v
                .iter()
                .find(|(n, _)| n == id)
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| id.chars().take(12).collect()),
            _ => id.chars().take(12).collect(),
        }
    }

    /// Opens a scope.
    pub fn focus(&mut self, scope: &str) {
        self.query.scope = Some(scope.to_string());
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        if self.query != self.loaded {
            self.notes = notes_loader(model, &self.query);
            self.loaded = self.query.clone();
        }
        kit::page_header(ui, icons::BRAIN, "Memory", |ui| {
            if kit::ghost_button(ui, &icons::label(icons::BOOKS, "Knowledge"))
                .on_hover_text("Search every host's index")
                .clicked()
            {
                self.knowledge.open = true;
            }
            if let Some(scope) = self.query.scope.clone()
                && kit::danger_button(ui, &icons::label(icons::TRASH, "Clear scope")).clicked()
            {
                self.confirm_clear = Some(scope);
            }
            ui.checkbox(
                &mut self.query.deleted,
                icons::label(icons::TRASH, "Deleted"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.query.q)
                    .hint_text(icons::label(icons::MAGNIFYING_GLASS, "Search"))
                    .desired_width(220.0),
            )
            .on_hover_text("Text or tags");
        });
        let w = ui.available_width();
        if w >= 240.0 + GAP + 520.0 {
            let left = (w * 0.24).clamp(240.0, 300.0);
            let h = ui.available_height();
            ui.horizontal_top(|ui| {
                let sp = ui.spacing().item_spacing.x;
                ui.allocate_ui_with_layout(
                    egui::vec2(left, h),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_width(left);
                        egui::ScrollArea::vertical()
                            .id_salt("memory_scopes")
                            .show(ui, |ui| self.scopes_ui(ui));
                    },
                );
                ui.add_space(GAP - sp);
                let right = ui.available_width();
                ui.allocate_ui_with_layout(
                    egui::vec2(right, h),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_width(right);
                        egui::ScrollArea::vertical()
                            .id_salt("memory_notes")
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.notes_ui(ui, model));
                    },
                );
            });
        } else {
            egui::ScrollArea::vertical()
                .id_salt("memory_stack")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.scopes_ui(ui);
                    ui.add_space(GAP);
                    self.notes_ui(ui, model);
                });
        }
        self.form_dialog(ui.ctx(), model);
        self.delete_dialog(ui.ctx(), model);
        self.clear_dialog(ui.ctx(), model);
        self.knowledge.ui(ui.ctx(), model);
    }

    fn scopes_ui(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.ctx());
        kit::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            let all = match self.scopes.get() {
                Some(Ok(s)) => s.iter().map(|x| x.live).sum::<i64>().to_string(),
                _ => String::new(),
            };
            if scope_row(
                ui,
                &icons::label(icons::BRAIN, "All"),
                &all,
                self.query.scope.is_none(),
            )
            .clicked()
            {
                self.query.scope = None;
            }
            match self.scopes.get() {
                None => {
                    ui.add_space(8.0);
                    ui.spinner();
                }
                Some(Err(e)) => {
                    ui.colored_label(p.red, e);
                }
                Some(Ok(scopes)) if scopes.is_empty() => {
                    egui::Frame::new()
                        .inner_margin(Margin::same(14))
                        .show(ui, |ui| {
                            ui.weak(icons::label(icons::NOTE, "No notes"));
                        });
                }
                Some(Ok(scopes)) => {
                    // Repositories first, then agents (profile lessons), then the rest.
                    let groups: [ScopeGroup; 3] = [
                        (icons::GIT_BRANCH, "Repositories", |s| {
                            s.contains('/') && !s.starts_with("profile:")
                        }),
                        (icons::ROBOT, "Agents", |s| s.starts_with("profile:")),
                        (icons::NOTE, "Other", |s| {
                            !s.contains('/') && !s.starts_with("profile:")
                        }),
                    ];
                    for (icon, title, member) in groups {
                        let mine: Vec<_> = scopes.iter().filter(|s| member(&s.scope)).collect();
                        if mine.is_empty() {
                            continue;
                        }
                        ui.add_space(6.0);
                        ui.label(kit::caps(&icons::label(icon, title), p.muted));
                        for s in mine {
                            let sel = self.query.scope.as_deref() == Some(s.scope.as_str());
                            let name = s.scope.strip_prefix("profile:").unwrap_or(&s.scope);
                            if scope_row(ui, name, &format!("{}/{}", s.live, s.total), sel)
                                .on_hover_text(format!(
                                    "{} · live / total · last write {}",
                                    s.scope,
                                    fmt::when(s.last_write_at)
                                ))
                                .clicked()
                            {
                                self.query.scope = Some(s.scope.clone());
                            }
                        }
                    }
                }
            }
        });
    }

    fn notes_ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        self.composer_ui(ui, model);
        ui.add_space(12.0);
        let notes = match self.notes.get() {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                return;
            }
            Some(Ok(n)) => n,
        };
        if notes.is_empty() {
            ui.weak(icons::label(icons::NOTE, "No notes"));
            return;
        }
        let now = kls_store::now_ms();
        let p = palette(ui.ctx());
        let all = self.query.scope.is_none();
        for n in notes.iter().take(self.shown) {
            kit::card(ui)
                .inner_margin(Margin::symmetric(16, 14))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if all || n.deleted {
                        ui.horizontal_wrapped(|ui| {
                            if all {
                                ui.label(
                                    RichText::new(icons::label(icons::FOLDER, &n.scope))
                                        .monospace()
                                        .size(12.0)
                                        .color(p.gold_text),
                                );
                            }
                            if n.deleted {
                                kit::chip(ui, &icons::label(icons::TRASH, "Deleted"), p.red);
                            }
                        });
                        ui.add_space(4.0);
                    }
                    // Plain text only: notes are untrusted.
                    ui.label(RichText::new(&n.text).size(14.0));
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        for t in &n.tags {
                            kit::chip(ui, t, p.muted);
                        }
                        // Provenance: the host it came from and the A2A
                        // message it was written from.
                        if !n.host.is_empty() {
                            let name = self.host_name(&n.host);
                            ui.label(
                                RichText::new(icons::label(icons::HARD_DRIVES, &name))
                                    .size(12.0)
                                    .color(p.muted),
                            )
                            .on_hover_text(format!("From host {}", n.host));
                        }
                        if !n.a2a_ref.is_empty() {
                            let thread = n.a2a_ref.split('/').next().unwrap_or_default();
                            if ui
                                .link(
                                    RichText::new(icons::label(
                                        icons::CHAT_CIRCLE_DOTS,
                                        &n.a2a_ref.chars().take(18).collect::<String>(),
                                    ))
                                    .size(12.0),
                                )
                                .on_hover_text(format!("A2A thread {thread} · copy"))
                                .clicked()
                            {
                                ui.ctx().copy_text(n.a2a_ref.clone());
                            }
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if !n.deleted {
                                if delete_link(ui).clicked() {
                                    self.confirm_delete = Some(n.clone());
                                }
                                if kit::ghost_button(
                                    ui,
                                    &icons::label(icons::PENCIL_SIMPLE, "Edit"),
                                )
                                .clicked()
                                {
                                    self.form = Some(NoteForm {
                                        id: Some(n.id.clone()),
                                        scope: n.scope.clone(),
                                        text: n.text.clone(),
                                        tags: n.tags.join(", "),
                                    });
                                }
                            }
                            ui.label(
                                RichText::new(fmt::ago(now, n.updated_at))
                                    .size(12.0)
                                    .color(p.muted),
                            )
                            .on_hover_text(fmt::when(n.updated_at));
                            ui.label(RichText::new("·").size(12.0).color(p.muted));
                            if n.author == OWNER {
                                ui.label(
                                    RichText::new(icons::label(icons::USER, "Keleustes"))
                                        .size(12.0)
                                        .color(p.muted),
                                )
                                .on_hover_text("Written in Keleustes");
                            } else {
                                ui.label(
                                    RichText::new(icons::label(icons::ROBOT, &n.author))
                                        .size(12.0)
                                        .color(p.muted),
                                );
                            }
                        });
                    });
                });
            ui.add_space(10.0);
        }
        if notes.len() > self.shown
            && kit::ghost_button(
                ui,
                &icons::label(
                    icons::ARROW_DOWN,
                    &format!("More · {} left", notes.len() - self.shown),
                ),
            )
            .clicked()
        {
            self.shown += PAGE;
        }
    }

    /// The new-note panel above the notes (gold edge).
    fn composer_ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let p = palette(ui.ctx());
        let scope = self.query.scope.clone();
        if let Some(s) = &scope
            && self.compose.scope != *s
        {
            self.compose.scope = s.clone();
        }
        kit::card(ui)
            .stroke(Stroke::new(1.0, p.gold_edge))
            .inner_margin(Margin::symmetric(16, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let f = &mut self.compose;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(icons::label(icons::PLUS, "New note"))
                            .size(12.0)
                            .color(p.muted),
                    );
                    if scope.is_some() {
                        ui.label(
                            RichText::new(format!("· {}", f.scope))
                                .monospace()
                                .size(12.0)
                                .color(p.gold_text),
                        );
                    }
                });
                if scope.is_none() {
                    ui.add(
                        egui::TextEdit::singleline(&mut f.scope)
                            .hint_text("Scope: org/repo or global")
                            .desired_width(f32::INFINITY),
                    )
                    .on_hover_text("github.com/org/repo or global");
                }
                ui.add(
                    egui::TextEdit::multiline(&mut f.text)
                        .hint_text("Note")
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(4.0);
                let chars = f.text.chars().count();
                let tags = split_tags(&f.tags);
                let ok = !f.text.trim().is_empty()
                    && !f.scope.trim().is_empty()
                    && chars <= NOTE_TEXT_MAX
                    && tags.len() <= NOTE_TAGS_MAX;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::TAG).color(p.muted));
                    let tw = (ui.available_width() - 260.0).max(140.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut f.tags)
                            .hint_text("Tags: a, b, c")
                            .desired_width(tw),
                    );
                    ui.label(
                        RichText::new(format!(
                            "{chars}/{NOTE_TEXT_MAX} · {}/{NOTE_TAGS_MAX}",
                            tags.len()
                        ))
                        .monospace()
                        .size(11.0)
                        .color(if ok || f.text.is_empty() {
                            p.muted
                        } else {
                            p.red
                        }),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_enabled_ui(ok, |ui| {
                                primary_button(ui, &icons::label(icons::PLUS, "Add note"))
                            })
                            .inner
                            .clicked()
                        {
                            let f2 = f.clone();
                            model.act(move |m| async move {
                                m.core()
                                    .add_note(
                                        f2.scope.trim(),
                                        &f2.text,
                                        split_tags(&f2.tags),
                                        OWNER,
                                    )
                                    .await?;
                                Ok(Some(Notice::Info("Note saved".into())))
                            });
                            f.text.clear();
                            f.tags.clear();
                        }
                    });
                });
            });
    }

    fn form_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(mut f) = self.form.take() else {
            return;
        };
        let mut open = true;
        let mut done = false;
        let title = if f.id.is_some() {
            icons::label(icons::PENCIL_SIMPLE, "Edit note")
        } else {
            icons::label(icons::PLUS, "Add note")
        };
        egui::Window::new(title)
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("note-form").num_columns(2).show(ui, |ui| {
                    ui.label("Scope");
                    ui.add_enabled(
                        f.id.is_none(),
                        egui::TextEdit::singleline(&mut f.scope).hint_text("org/repo or global"),
                    )
                    .on_hover_text("github.com/org/repo or global");
                    ui.end_row();
                    ui.label("Tags");
                    ui.add(egui::TextEdit::singleline(&mut f.tags).hint_text("a, b, c"));
                    ui.end_row();
                });
                ui.add(
                    egui::TextEdit::multiline(&mut f.text)
                        .desired_rows(8)
                        .desired_width(420.0),
                );
                let chars = f.text.chars().count();
                let tags = split_tags(&f.tags);
                ui.weak(format!(
                    "{}  {chars}/{NOTE_TEXT_MAX}   {}  {}/{NOTE_TAGS_MAX}",
                    icons::FILE_TEXT,
                    icons::TAG,
                    tags.len()
                ));
                let ok = !f.text.trim().is_empty()
                    && !f.scope.trim().is_empty()
                    && chars <= NOTE_TEXT_MAX
                    && tags.len() <= NOTE_TAGS_MAX;
                if ui
                    .add_enabled_ui(ok, |ui| {
                        primary_button(ui, &icons::label(icons::FLOPPY_DISK, "Save"))
                    })
                    .inner
                    .clicked()
                {
                    let f2 = f.clone();
                    model.act(move |m| async move {
                        let core = m.core();
                        match f2.id {
                            Some(id) => {
                                core.edit_note(&id, Some(&f2.text), Some(split_tags(&f2.tags)))
                                    .await?;
                            }
                            None => {
                                core.add_note(
                                    f2.scope.trim(),
                                    &f2.text,
                                    split_tags(&f2.tags),
                                    OWNER,
                                )
                                .await?;
                            }
                        }
                        Ok(Some(Notice::Info("Note saved".into())))
                    });
                    done = true;
                }
            });
        if open && !done {
            self.form = Some(f);
        }
    }

    fn delete_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(n) = self.confirm_delete.clone() else {
            return;
        };
        egui::Window::new(icons::label(icons::TRASH, "Delete note"))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(icons::label(
                    icons::WARNING,
                    &format!("Delete from {}?", n.scope),
                ))
                .on_hover_text("Nodes drop it at their next pull");
                ui.horizontal(|ui| {
                    if kit::danger_button(ui, &icons::label(icons::TRASH, "Delete")).clicked() {
                        let id = n.id.clone();
                        model.act(move |m| async move {
                            m.core().delete_note(&id).await?;
                            Ok(Some(Notice::Info("Note deleted".into())))
                        });
                        self.confirm_delete = None;
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        self.confirm_delete = None;
                    }
                });
            });
    }

    fn clear_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(scope) = self.confirm_clear.clone() else {
            return;
        };
        egui::Window::new(icons::label(icons::TRASH, "Clear scope"))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(icons::label(
                    icons::WARNING,
                    &format!("Delete all notes in {scope}?"),
                ))
                .on_hover_text("Nodes drop these notes at their next sync");
                ui.horizontal(|ui| {
                    if kit::danger_button(ui, &icons::label(icons::TRASH, "Clear")).clicked() {
                        let s = scope.clone();
                        model.act(move |m| async move {
                            let n = m.core().clear_memory_scope(&s).await?;
                            Ok(Some(Notice::Info(format!("Cleared {n} notes"))))
                        });
                        self.confirm_clear = None;
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        self.confirm_clear = None;
                    }
                });
            });
    }
}

/// One scope row: gold left edge and tint when selected, a count on the
/// right, a line below.
fn scope_row(ui: &mut Ui, name: &str, count: &str, selected: bool) -> egui::Response {
    let p = palette(ui.ctx());
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 44.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(rect, 0.0, p.panel2);
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            0.0,
            p.gold,
        );
    } else if resp.hovered() {
        painter.rect_filled(rect, 0.0, p.panel2.gamma_multiply(0.6));
    }
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, p.line),
    );
    let inner = rect.shrink2(egui::vec2(14.0, 0.0));
    let count_font = egui::FontId::monospace(12.0);
    let cw = painter
        .layout_no_wrap(count.to_string(), count_font.clone(), p.muted)
        .size()
        .x;
    let name_w = (inner.width() - cw - 8.0).max(20.0);
    let mut job = egui::text::LayoutJob::simple_singleline(
        name.to_string(),
        egui::FontId::proportional(14.0),
        p.text,
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(name_w);
    let galley = painter.layout_job(job);
    painter.galley(
        egui::pos2(inner.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        p.text,
    );
    painter.text(
        egui::pos2(inner.right(), rect.center().y),
        egui::Align2::RIGHT_CENTER,
        count,
        count_font,
        p.muted,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small outline button with red text (delete one note).
fn delete_link(ui: &mut Ui) -> egui::Response {
    let p = palette(ui.ctx());
    ui.add(
        egui::Button::new(RichText::new(icons::label(icons::TRASH, "Delete")).color(p.red))
            .fill(egui::Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0, p.line)),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn tags_split() {
        assert_eq!(super::split_tags("a, b,,c "), vec!["a", "b", "c"]);
    }
}

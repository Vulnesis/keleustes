//! Conflicts screen. Owner: M4 GitHub workstream.
//!
//! A three-way merge editor: pick a pull request and a file, see the merge
//! base, ours (the pull request's head) and theirs (the base branch) side
//! by side with their changes against the base, take ours, theirs, both or
//! the base per conflict, edit the result, and commit the resolution as a
//! merge commit on the head branch (through the API on GitHub, which signs it;
//! elsewhere pushed unsigned with the forge CLI).

use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme;
use kls_github::conflicts::{self as backend, ConflictSet, How};
use kls_github::merge3::{Choice, Chunk, has_markers};
use kls_store::github::{GhPull, Resolution};
use similar::{ChangeTag, TextDiff};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub const READY: bool = true;

#[derive(Clone)]
struct Data {
    conflicting: Vec<GhPull>,
    open: Vec<GhPull>,
    /// The kill switch is on (paused or stopped).
    stopped: bool,
}

/// The editor state of one file.
#[derive(Clone)]
struct FileEdit {
    choices: Vec<Choice>,
    text: String,
    /// The person typed in the result (a hunk choice then asks first).
    edited: bool,
    /// How the file was resolved (for the commit description).
    how: How,
}

type Slot = Arc<Mutex<Option<Result<ConflictSet, String>>>>;

pub struct Screen {
    data: Loader<Data>,
    history: Loader<Vec<Resolution>>,
    target: Arc<Mutex<Option<(String, i64)>>>,
    repo_input: String,
    number_input: String,
    set: Slot,
    loading: Arc<Mutex<bool>>,
    loaded_key: Option<(String, i64, String)>,
    file: usize,
    edits: BTreeMap<String, FileEdit>,
    message: String,
    /// The person changed the generated description.
    message_edited: bool,
    committing: Arc<Mutex<bool>>,
    pending_load: Option<(String, i64)>,
    /// The set loaded before a reload of the same pull request (head_moved carry-over).
    prev: Option<ConflictSet>,
    /// After a head move: files kept, files dropped.
    kept: Option<(usize, usize)>,
    /// A commit found the head moved: reload, keeping what still holds.
    reload: Arc<Mutex<bool>>,
    resolving: Arc<Mutex<bool>>,
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        let data = Loader::new(
            model,
            &["github.pull.", "github.conflict.", "factory."],
            |core| async move {
                let s = core.store();
                Ok(Data {
                    conflicting: s.gh_conflicting_pulls().await?,
                    open: s.gh_pulls(None, Some("open")).await?,
                    stopped: !kls_core::control::state(&core).await?.mode.running(),
                })
            },
        );
        let target: Arc<Mutex<Option<(String, i64)>>> = Arc::default();
        let t2 = target.clone();
        let history = Loader::new(model, &["github.conflict."], move |core| {
            let t = t2.lock().unwrap().clone();
            async move {
                match t {
                    Some((r, n)) => core.store().gh_resolutions(&r, n).await,
                    None => Ok(Vec::new()),
                }
            }
        });
        Self {
            data,
            history,
            target,
            repo_input: String::new(),
            number_input: String::new(),
            set: Arc::default(),
            loading: Arc::default(),
            loaded_key: None,
            file: 0,
            edits: BTreeMap::new(),
            message: String::new(),
            message_edited: false,
            committing: Arc::default(),
            pending_load: None,
            prev: None,
            kept: None,
            reload: Arc::default(),
            resolving: Arc::default(),
        }
    }

    /// Opens `owner/name#N` and loads its conflicts.
    pub fn focus(&mut self, id: &str) {
        if let Some((r, n)) = id.split_once('#')
            && let Ok(n) = n.parse::<i64>()
        {
            self.repo_input = r.to_string();
            self.number_input = n.to_string();
            self.pending_load = Some((r.to_string(), n));
        }
    }

    fn load(&mut self, model: &Model, repo: String, number: i64) {
        *self.target.lock().unwrap() = Some((repo.clone(), number));
        self.history.reload();
        // Same pull request again (head moved, or Load): keep the old set so
        // resolutions whose three versions did not change carry over.
        let old = self.set.lock().unwrap().take();
        self.prev = match old {
            Some(Ok(s)) if s.repo == repo && s.number == number => Some(s),
            _ => {
                self.edits.clear();
                self.message_edited = false;
                self.message.clear();
                self.file = 0;
                None
            }
        };
        *self.loading.lock().unwrap() = true;
        self.loaded_key = None;
        let slot = self.set.clone();
        let loading = self.loading.clone();
        model.act(move |m| async move {
            let r = kls_github::service(m.core())
                .load_conflicts(&repo, number)
                .await
                .map_err(|e| format!("{e:#}"));
            *slot.lock().unwrap() = Some(r);
            *loading.lock().unwrap() = false;
            Ok(None)
        });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        if let Some((r, n)) = self.pending_load.take() {
            self.load(model, r, n);
        }
        let reload = std::mem::take(&mut *self.reload.lock().unwrap());
        let target = self.target.lock().unwrap().clone();
        if reload && let Some((r, n)) = target {
            self.load(model, r, n);
        }
        let p = theme::palette(ui.ctx());
        let mut pick: Option<(String, i64)> = None;
        kit::page_header(ui, icons::GIT_MERGE, "Conflicts", |ui| {
            if kit::ghost_button(ui, &icons::label(icons::DOWNLOAD_SIMPLE, "Load")).clicked() {
                match self
                    .number_input
                    .trim()
                    .trim_start_matches('#')
                    .parse::<i64>()
                {
                    Ok(n) if kls_github::valid_repo(self.repo_input.trim()) => {
                        pick = Some((self.repo_input.trim().to_string(), n))
                    }
                    _ => model.notify(Notice::Error(
                        "Enter owner/name and a pull request number".into(),
                    )),
                }
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.number_input)
                    .hint_text("#")
                    .desired_width(50.0),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.repo_input)
                    .hint_text("owner/name")
                    .desired_width(150.0),
            );
        });
        egui::ScrollArea::vertical()
            .id_salt("conflicts_page")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(x) = self.pick_bar(ui) {
                    pick = Some(x);
                }
                ui.add_space(12.0);
                if *self.loading.lock().unwrap() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("Loading…").color(p.muted));
                    });
                    return;
                }
                let set = self.set.lock().unwrap().clone();
                match set {
                    None => {
                        ui.label(
                            egui::RichText::new(icons::label(
                                icons::GIT_PULL_REQUEST,
                                "No pull request",
                            ))
                            .color(p.muted),
                        );
                    }
                    Some(Err(e)) => {
                        ui.colored_label(p.red, e);
                    }
                    Some(Ok(set)) => self.editor(ui, model, &set),
                }
            });
        if let Some((r, n)) = pick {
            self.repo_input = r.clone();
            self.number_input = n.to_string();
            self.load(model, r, n);
        }
    }

    /// Conflicting pull requests as chips, other open ones in a list.
    fn pick_bar(&mut self, ui: &mut egui::Ui) -> Option<(String, i64)> {
        let p = theme::palette(ui.ctx());
        let data = match self.data.get() {
            Some(Ok(d)) => d,
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
                return None;
            }
            None => {
                ui.spinner();
                return None;
            }
        };
        let current = self.target.lock().unwrap().clone();
        let mut pick: Option<(String, i64)> = None;
        kit::card(ui)
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(kit::caps(
                        &icons::label(icons::WARNING, "Conflicting"),
                        p.muted,
                    ));
                    if data.conflicting.is_empty() {
                        kit::chip(ui, &icons::label(icons::CHECK, "None"), p.green);
                    }
                    for pr in &data.conflicting {
                        let sel = current.as_ref() == Some(&(pr.repo.clone(), pr.number));
                        if kit::pick_chip(
                            ui,
                            &format!("{} {}#{}", icons::X, pr.repo, pr.number),
                            sel,
                            p.red,
                        )
                        .on_hover_text(&pr.title)
                        .clicked()
                        {
                            pick = Some((pr.repo.clone(), pr.number));
                        }
                    }
                    ui.separator();
                    ui.label(kit::caps(
                        &icons::label(icons::GIT_PULL_REQUEST, "Open"),
                        p.muted,
                    ));
                    let open: Vec<&GhPull> = data
                        .open
                        .iter()
                        .filter(|p| p.mergeable != "conflict")
                        .take(100)
                        .collect();
                    let shown = current
                        .as_ref()
                        .and_then(|(r, n)| {
                            open.iter()
                                .find(|p| &p.repo == r && p.number == *n)
                                .map(|p| format!("{}#{}", p.repo, p.number))
                        })
                        .unwrap_or_else(|| format!("{} pull requests", open.len()));
                    egui::ComboBox::from_id_salt("conflict_open")
                        .selected_text(shown)
                        .width(240.0)
                        .show_ui(ui, |ui| {
                            for pr in &open {
                                let sel = current.as_ref() == Some(&(pr.repo.clone(), pr.number));
                                if ui
                                    .selectable_label(
                                        sel,
                                        format!("{}#{} {}", pr.repo, pr.number, pr.title),
                                    )
                                    .clicked()
                                {
                                    pick = Some((pr.repo.clone(), pr.number));
                                }
                            }
                        });
                });
            });
        pick
    }

    fn edit_for(&mut self, set: &ConflictSet, i: usize) -> FileEdit {
        let f = &set.files[i];
        self.edits
            .entry(f.path.clone())
            .or_insert_with(|| {
                let choices = vec![Choice::Unresolved; f.merge.conflicts()];
                FileEdit {
                    text: f.merge.render(&choices),
                    choices,
                    edited: false,
                    how: How::Merged,
                }
            })
            .clone()
    }

    fn editor(&mut self, ui: &mut egui::Ui, model: &Model, set: &ConflictSet) {
        let p = theme::palette(ui.ctx());
        let key = (set.repo.clone(), set.number, set.head_sha.clone());
        if self.loaded_key.as_ref() != Some(&key) {
            self.loaded_key = Some(key);
            match self.prev.take() {
                Some(old) if old.head_sha != set.head_sha => {
                    // head_moved: keep what still holds.
                    let keep = backend::unchanged_files(&old, set);
                    let before = self.edits.len();
                    self.edits.retain(|p, _| keep.contains(p));
                    self.kept = Some((self.edits.len(), before - self.edits.len()));
                    let path = old.files.get(self.file).map(|f| f.path.clone());
                    self.file = path
                        .and_then(|p| set.files.iter().position(|f| f.path == p))
                        .unwrap_or_else(|| {
                            set.files.iter().position(|f| f.conflicting).unwrap_or(0)
                        });
                }
                Some(_) => {}
                None => {
                    self.edits.clear();
                    self.kept = None;
                    self.file = set.files.iter().position(|f| f.conflicting).unwrap_or(0);
                }
            }
        }
        // J / K: next / previous file (not while typing).
        if !set.files.is_empty() && ui.ctx().memory(|m| m.focused().is_none()) {
            let (next, prev) =
                ui.input(|i| (i.key_pressed(egui::Key::J), i.key_pressed(egui::Key::K)));
            let n = set.files.len();
            if next {
                self.file = (self.file.min(n - 1) + 1) % n;
            }
            if prev {
                self.file = (self.file.min(n - 1) + n - 1) % n;
            }
        }
        if !self.message_edited {
            let how: BTreeMap<String, How> =
                self.edits.iter().map(|(p, e)| (p.clone(), e.how)).collect();
            self.message = backend::describe(set, &how);
        }
        let current = if set.files.is_empty() {
            None
        } else {
            let i = self.file.min(set.files.len() - 1);
            let f = set.files[i].clone();
            let edit = (!f.binary).then(|| self.edit_for(set, i));
            Some((i, f, edit))
        };
        let unresolved: Vec<String> = set
            .files
            .iter()
            .filter(|f| {
                f.binary
                    || match self.edits.get(&f.path) {
                        Some(e) => has_markers(&e.text),
                        None => f.conflicting,
                    }
            })
            .map(|f| f.path.clone())
            .collect();
        let (hunks, done) = set.files.iter().fold((0, 0), |(h, d), f| {
            let n = f.merge.conflicts();
            let r = self
                .edits
                .get(&f.path)
                .map(|e| {
                    e.choices
                        .iter()
                        .filter(|c| **c != Choice::Unresolved)
                        .count()
                })
                .unwrap_or(0);
            (h + n, d + r)
        });

        // Pull request header: title, refs, the signed commit.
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!("{}#{} — {}", set.repo, set.number, set.title))
                    .size(22.0)
                    .strong(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = *self.committing.lock().unwrap();
                let can = unresolved.is_empty() && !set.is_fork() && !busy && !set.files.is_empty();
                if busy {
                    ui.spinner();
                }
                let (glyph, word, hover) = if set.api_signed() {
                    (
                        icons::SEAL_CHECK,
                        "Commit signed resolution",
                        format!(
                            "Committed via the {} API, which signs it",
                            set.forge.label()
                        ),
                    )
                } else {
                    (
                        icons::UPLOAD_SIMPLE,
                        "Commit and push",
                        format!(
                            "Merge commit pushed with {}; {} does not sign it",
                            set.forge.cli().name(),
                            set.forge.label()
                        ),
                    )
                };
                let r = ui
                    .add_enabled_ui(can, |ui| {
                        theme::primary_button(ui, &icons::label(glyph, word))
                    })
                    .inner
                    .on_hover_text(hover);
                if r.clicked() {
                    self.commit(model, set);
                }
                let filing = *self.resolving.lock().unwrap();
                if filing {
                    ui.spinner();
                }
                let why = hand_off_block(
                    set,
                    filing,
                    resolver_host(model, self.data.get().and_then(|d| d.ok()).as_ref(), set),
                );
                let r = ui
                    .add_enabled_ui(why.is_none(), |ui| {
                        kit::ghost_button(
                            ui,
                            &icons::label(icons::ROBOT, "Resolve with belai:conflict-resolver"),
                        )
                    })
                    .inner;
                let r = match &why {
                    Some(w) => r.on_disabled_hover_text(w.as_str()),
                    None => r.on_hover_text("Files a merge-conflict card and starts the worker"),
                };
                if r.clicked() {
                    self.hand_off(model, set);
                }
            });
        });
        ui.label(
            egui::RichText::new(format!(
                "{} ← {} · merge base {} · {} files · {} hunks · {} of {} resolved",
                set.head_ref,
                set.base_ref,
                short(&set.merge_base),
                set.files.len(),
                hunks,
                done.min(hunks),
                hunks
            ))
            .monospace()
            .size(12.0)
            .color(p.muted),
        )
        .on_hover_text(format!(
            "{} shared · {} conflicting · {} base-only",
            set.files.len(),
            set.conflicting().count(),
            set.base_only.len()
        ));
        ui.horizontal_wrapped(|ui| {
            if !set.api_signed() {
                kit::chip(
                    ui,
                    &icons::label(icons::SEAL_WARNING, "Unsigned push"),
                    p.amber,
                )
                .on_hover_text(format!(
                    "{}: the merge commit is pushed by {}, not signed by the forge",
                    set.forge.label(),
                    set.forge.cli().name()
                ));
            }
            if set.is_fork() {
                kit::chip(
                    ui,
                    &icons::label(
                        icons::WARNING,
                        &format!("Fork head {}: push disabled", set.head_repo),
                    ),
                    p.amber,
                );
            }
            if let Some((kept, dropped)) = self.kept {
                kit::chip(
                    ui,
                    &icons::label(
                        icons::ARROWS_CLOCKWISE,
                        &format!("Head moved · {kept} kept · {dropped} reset"),
                    ),
                    if dropped > 0 { p.amber } else { p.cyan },
                );
            }
            let locks = set.conflicting().filter(|f| f.lockfile()).count();
            if locks > 0 {
                kit::chip(
                    ui,
                    &icons::label(
                        icons::LOCK_SIMPLE,
                        &format!("{locks} lockfiles · regenerate"),
                    ),
                    p.amber,
                )
                .on_hover_text("Keep one side; the description lists the commands");
            }
            if !unresolved.is_empty() && !set.files.is_empty() {
                kit::chip(
                    ui,
                    &icons::label(icons::WARNING, &unresolved.join(", ")),
                    p.amber,
                )
                .on_hover_text("Still to resolve");
            }
        });
        ui.add_space(8.0);
        let Some((i, f, edit)) = current else {
            ui.label(icons::label(icons::CHECK, "No shared changes"))
                .on_hover_text("Use Update branch on Repositories");
            return;
        };

        // Files.
        ui.horizontal_wrapped(|ui| {
            ui.label(kit::caps(&icons::label(icons::FILE_TEXT, "Files"), p.muted))
                .on_hover_text("J / K: next / previous");
            for (k, f) in set.files.iter().enumerate() {
                let resolved = self
                    .edits
                    .get(&f.path)
                    .is_some_and(|e| !has_markers(&e.text));
                let (glyph, colour) = if f.binary {
                    (icons::WARNING, p.amber)
                } else if !f.conflicting || resolved {
                    (icons::CHECK, p.green)
                } else {
                    (icons::X, p.red)
                };
                if kit::pick_chip(ui, &format!("{glyph} {}", f.path), i == k, colour).clicked() {
                    self.file = k;
                }
            }
        });
        ui.add_space(10.0);
        let Some(mut edit) = edit else {
            kit::chip(
                ui,
                &icons::label(icons::WARNING, "Binary or >1 MB"),
                p.amber,
            );
            return;
        };
        let base = f.base.clone().unwrap_or_default();
        let ours = f.ours.clone().unwrap_or_default();
        let theirs = f.theirs.clone().unwrap_or_default();

        // Three panes: ours, base, theirs (sides coloured by their changes).
        kit::Grid::new(300.0)
            .tall()
            .show(ui, 3, |ui, k, _| match k {
                0 => pane(
                    ui,
                    &format!("Ours · {}", set.head_ref),
                    short(&set.head_sha),
                    p.cyan,
                    &ours,
                    Some(&base),
                    f.ours.is_none(),
                ),
                1 => pane(
                    ui,
                    "Base",
                    short(&set.merge_base),
                    p.muted,
                    &base,
                    None,
                    f.base.is_none(),
                ),
                _ => pane(
                    ui,
                    &format!("Theirs · {}", set.base_ref),
                    short(&set.base_sha),
                    p.violet,
                    &theirs,
                    Some(&base),
                    f.theirs.is_none(),
                ),
            });
        ui.add_space(12.0);

        // Hunk list and the editable result.
        let history = self.history.get();
        let mut changed = false;
        let mut message = std::mem::take(&mut self.message);
        kit::Split::new(
            260.0,
            420.0,
            kit::Widths::FirstClamped {
                share: 0.28,
                max: 320.0,
            },
        )
        .gap(12.0)
        .show(ui, |ui, side| {
            if side == 0 {
                changed |= hunk_list(ui, &f, &mut edit, &ours, &theirs);
                if let Some(Ok(h)) = &history
                    && !h.is_empty()
                {
                    ui.add_space(12.0);
                    resolutions(ui, h);
                }
            } else {
                if changed {
                    edit.text = f.merge.render(&edit.choices);
                    edit.edited = false;
                    edit.how = How::Hunks;
                    changed = false;
                }
                if result_panel(ui, &mut edit, &mut message) {
                    self.message_edited = true;
                }
            }
        });
        self.message = message;
        if changed {
            edit.text = f.merge.render(&edit.choices);
            edit.edited = false;
            edit.how = How::Hunks;
        }
        self.edits.insert(f.path.clone(), edit);
    }

    fn commit(&mut self, model: &Model, set: &ConflictSet) {
        let resolved: BTreeMap<String, Option<String>> = set
            .files
            .iter()
            .filter_map(|f| {
                self.edits
                    .get(&f.path)
                    .map(|e| (f.path.clone(), Some(e.text.clone())))
            })
            .collect();
        let set = set.clone();
        let msg = self.message.clone();
        let busy = self.committing.clone();
        let slot = self.set.clone();
        let reload = self.reload.clone();
        *busy.lock().unwrap() = true;
        model.act(move |m| async move {
            let r = kls_github::service(m.core())
                .commit_resolution(&set, &resolved, &msg)
                .await;
            *busy.lock().unwrap() = false;
            if let Err(e) = &r
                && backend::head_moved(&format!("{e:#}"))
            {
                // Reload on the new head; unchanged files keep their resolution.
                *reload.lock().unwrap() = true;
            }
            let r = r?;
            *slot.lock().unwrap() = None;
            Ok(Some(Notice::Info(format!(
                "Committed {} to {} ({})",
                short(&r.commit_sha),
                r.branch,
                signing_word(&r, set.forge)
            ))))
        });
    }

    /// Hands the pull request's conflicts to belai:conflict-resolver (a card).
    fn hand_off(&mut self, model: &Model, set: &ConflictSet) {
        let url = self
            .data
            .get()
            .and_then(|d| d.ok())
            .and_then(|d| {
                d.conflicting
                    .iter()
                    .chain(d.open.iter())
                    .find(|p| p.repo == set.repo && p.number == set.number)
                    .map(|p| p.html_url.clone())
            })
            .unwrap_or_default();
        let set = set.clone();
        let busy = self.resolving.clone();
        *busy.lock().unwrap() = true;
        model.act(move |m| async move {
            let r = kls_engine::conflicts::dispatch_resolver(m.core(), &set, &url, "user").await;
            *busy.lock().unwrap() = false;
            let h = r?;
            Ok(Some(Notice::Info(match &h.dispatch {
                None => format!("{} held for fork code", h.key),
                Some(_) if h.existing => format!("{} resumed by {}", h.key, backend::RESOLVER),
                Some(_) => format!("{} sent to {}", h.key, backend::RESOLVER),
            })))
        });
    }
}

/// Where the resolver would run for `set`, or why it cannot.
fn resolver_host(model: &Model, data: Option<&Data>, set: &ConflictSet) -> Result<String, String> {
    let snap = model.snapshot();
    kls_engine::conflicts::resolver_host(
        &snap.nodes,
        &snap.online,
        data.is_none_or(|d| d.stopped),
        &set.repo,
    )
}

/// Why the hand-off button is disabled, if it is.
fn hand_off_block(set: &ConflictSet, busy: bool, host: Result<String, String>) -> Option<String> {
    if busy {
        return Some("Sending".into());
    }
    if set.conflicting().next().is_none() {
        return Some("No conflicts".into());
    }
    host.err()
}

/// A lockfile is never hand-merged: keep one side whole, then regenerate.
fn lockfile_sides(
    ui: &mut egui::Ui,
    f: &kls_github::conflicts::ConflictFile,
    edit: &mut FileEdit,
    ours: &str,
    theirs: &str,
) {
    let p = theme::palette(ui.ctx());
    let cmd = backend::regenerate_command(&f.path).unwrap_or("");
    kit::chip(
        ui,
        &icons::label(icons::LOCK_SIMPLE, "Lockfile · keep one side"),
        p.amber,
    )
    .on_hover_text(format!("Then run `{cmd}`"));
    ui.label(
        egui::RichText::new(cmd)
            .monospace()
            .size(12.0)
            .color(p.muted),
    );
    ui.horizontal_wrapped(|ui| {
        for (label, text, how, colour) in [
            ("Keep ours", ours, How::Ours, p.cyan),
            ("Keep theirs", theirs, How::Theirs, p.violet),
        ] {
            if outline_button(
                ui,
                &icons::label(icons::LOCK_SIMPLE, label),
                colour,
                edit.how == how,
            )
            .clicked()
            {
                edit.text = text.to_string();
                edit.edited = true;
                edit.how = how;
            }
        }
    });
}

/// The hunks of one file with take-ours/theirs/both per hunk. Returns
/// whether a choice changed (the result is then re-rendered).
fn hunk_list(
    ui: &mut egui::Ui,
    f: &kls_github::conflicts::ConflictFile,
    edit: &mut FileEdit,
    ours: &str,
    theirs: &str,
) -> bool {
    let p = theme::palette(ui.ctx());
    let mut changed = false;
    kit::section(
        ui,
        icons::LIST_CHECKS,
        &format!("{} · hunks", file_name(&f.path)),
        false,
        |_| {},
        |ui| {
            if f.lockfile() {
                lockfile_sides(ui, f, edit, ours, theirs);
                return;
            }
            if edit.choices.is_empty() {
                ui.label(egui::RichText::new("No conflicts").color(p.muted));
            } else {
                ui.horizontal_wrapped(|ui| {
                    ui.label(kit::caps("All", p.muted));
                    for (label, c, colour) in [
                        ("Ours", Choice::Ours, p.cyan),
                        ("Theirs", Choice::Theirs, p.violet),
                        ("Both", Choice::Both, p.line),
                    ] {
                        if outline_button(ui, label, colour, false).clicked() {
                            edit.choices.iter_mut().for_each(|x| *x = c);
                            changed = true;
                        }
                    }
                });
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(kit::caps("File", p.muted));
                if outline_button(ui, &icons::label(icons::FILE_TEXT, "Ours"), p.cyan, false)
                    .on_hover_text("Take whole ours file")
                    .clicked()
                {
                    edit.text = ours.to_string();
                    edit.edited = true;
                    edit.how = How::Ours;
                }
                if outline_button(
                    ui,
                    &icons::label(icons::FILE_TEXT, "Theirs"),
                    p.violet,
                    false,
                )
                .on_hover_text("Take whole theirs file")
                .clicked()
                {
                    edit.text = theirs.to_string();
                    edit.edited = true;
                    edit.how = How::Theirs;
                }
            });
            let mut k = 0;
            let mut line = 1usize;
            for c in &f.merge.chunks {
                match c {
                    Chunk::Clean(lines) => line += lines.len(),
                    Chunk::Conflict {
                        ours: o, theirs: t, ..
                    } => {
                        let idx = k;
                        k += 1;
                        let start = line;
                        line += o.len();
                        ui.add_space(6.0);
                        kit::hline_full(ui, p.line);
                        ui.add_space(6.0);
                        let span = match o.len() {
                            0 => format!("At line {start}"),
                            1 => format!("Line {start}"),
                            n => format!("Lines {start}–{}", start + n - 1),
                        };
                        let (state, colour) = match edit.choices[idx] {
                            Choice::Unresolved => ("unresolved", p.red),
                            Choice::Ours => ("took ours", p.green),
                            Choice::Theirs => ("took theirs", p.green),
                            Choice::Both => ("took both", p.green),
                            Choice::Base => ("took base", p.green),
                        };
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!("#{} · {span}", idx + 1)).strong(),
                            )
                            .on_hover_text(format!(
                                "ours ({} lines):\n{}\ntheirs ({} lines):\n{}",
                                o.len(),
                                o.concat(),
                                t.len(),
                                t.concat()
                            ));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(egui::RichText::new(state).size(13.0).color(colour));
                                },
                            );
                        });
                        ui.horizontal_wrapped(|ui| {
                            for (label, ch, colour) in [
                                ("Take ours", Choice::Ours, p.cyan),
                                ("Take theirs", Choice::Theirs, p.violet),
                                ("Both", Choice::Both, p.line),
                                ("Base", Choice::Base, p.line),
                                ("Markers", Choice::Unresolved, p.line),
                            ] {
                                if outline_button(ui, label, colour, edit.choices[idx] == ch)
                                    .clicked()
                                {
                                    edit.choices[idx] = ch;
                                    changed = true;
                                }
                            }
                        });
                    }
                }
            }
        },
    );
    changed
}

/// Past resolutions of this pull request with their signature state.
fn resolutions(ui: &mut egui::Ui, h: &[Resolution]) {
    let p = theme::palette(ui.ctx());
    kit::section(
        ui,
        icons::GIT_COMMIT,
        "Resolutions",
        false,
        |_| {},
        |ui| {
            for r in h.iter().take(5) {
                let (glyph, colour, word) = match r.verified {
                    _ if r.signing == "unsigned_push" => {
                        (icons::SEAL_WARNING, p.muted, "Unsigned push")
                    }
                    Some(true) => (icons::SEAL_CHECK, p.green, "Verified"),
                    Some(false) => (icons::SEAL_WARNING, p.amber, "Unverified"),
                    None => (icons::QUESTION, p.muted, "Signature unknown"),
                };
                ui.horizontal(|ui| {
                    ui.colored_label(colour, glyph).on_hover_text(word);
                    ui.label(
                        egui::RichText::new(format!(
                            "{} · {} files",
                            &r.commit_sha[..r.commit_sha.len().min(12)],
                            r.files.len(),
                        ))
                        .monospace()
                        .color(p.muted),
                    );
                });
            }
        },
    );
}

/// The editable merged result in a highlighted panel, with the commit message.
fn result_panel(ui: &mut egui::Ui, edit: &mut FileEdit, message: &mut String) -> bool {
    let mut msg_changed = false;
    let p = theme::palette(ui.ctx());
    kit::glow(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(icons::label(
                            icons::PENCIL_SIMPLE,
                            "Result — editable",
                        ))
                        .size(13.0)
                        .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if edit.edited {
                            kit::chip(ui, &icons::label(icons::PENCIL_SIMPLE, "Edited"), p.muted);
                        }
                        if has_markers(&edit.text) {
                            kit::chip(ui, &icons::label(icons::X, "Markers left"), p.red);
                        } else {
                            kit::chip(ui, &icons::label(icons::CHECK, "Resolved"), p.green);
                        }
                    });
                });
            });
        strip(ui, p.gold, 2.0);
        let resp = egui::Frame::new()
            .fill(p.bg)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical()
                    .id_salt("result")
                    .max_height(420.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut edit.text)
                                .code_editor()
                                .frame(egui::Frame::NONE)
                                .background_color(p.bg)
                                .margin(egui::Margin::same(12))
                                .desired_width(f32::INFINITY)
                                .desired_rows(16),
                        )
                    })
                    .inner
            })
            .inner;
        if resp.changed() {
            edit.edited = true;
            edit.how = How::Edited;
        }
        kit::hline_full(ui, p.line);
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(kit::caps(
                        &icons::label(icons::CHAT_TEXT, "Commit message"),
                        p.muted,
                    ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        kit::chip(ui, &icons::label(icons::SEAL_CHECK, "Signed"), p.cyan)
                            .on_hover_text("Committed via the GitHub API, which signs it");
                    });
                });
                let r = ui.add(
                    egui::TextEdit::multiline(message)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(6),
                );
                msg_changed = r.changed();
            });
    });
    msg_changed
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

/// How a committed resolution was signed, for the notice.
fn signing_word(r: &Resolution, forge: kls_github::ForgeKind) -> String {
    if r.signing == "unsigned_push" {
        return "pushed unsigned".into();
    }
    match r.verified {
        Some(true) => format!("signed by {}, verified", forge.label()),
        Some(false) => format!("{} did not verify the signature", forge.label()),
        None => "signature not reported".into(),
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// One side of the merge: a titled panel with a coloured strip, its text
/// coloured by its changes against `base`.
fn pane(
    ui: &mut egui::Ui,
    title: &str,
    reference: &str,
    color: egui::Color32,
    text: &str,
    base: Option<&str>,
    missing: bool,
) {
    let p = theme::palette(ui.ctx());
    kit::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(title).size(13.0).strong()).truncate(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(reference)
                                .monospace()
                                .size(11.0)
                                .color(p.muted),
                        );
                    });
                });
            });
        strip(ui, color, 2.0);
        egui::Frame::new()
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                egui::ScrollArea::both()
                    .id_salt(title)
                    .max_height(260.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if missing {
                            ui.label(
                                egui::RichText::new(icons::label(icons::MINUS, "Absent"))
                                    .color(p.muted),
                            );
                            return;
                        }
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let mono = |s: &str| {
                            egui::RichText::new(s.trim_end_matches('\n'))
                                .monospace()
                                .size(12.0)
                        };
                        match base {
                            None => {
                                for l in text.lines() {
                                    ui.label(mono(l));
                                }
                            }
                            Some(b) => {
                                let diff = TextDiff::from_lines(b, text);
                                for ch in diff.iter_all_changes() {
                                    let s = ch.value();
                                    match ch.tag() {
                                        ChangeTag::Equal => {
                                            ui.label(mono(&format!("  {s}")));
                                        }
                                        ChangeTag::Insert => {
                                            ui.label(mono(&format!("+ {s}")).color(p.green));
                                        }
                                        ChangeTag::Delete => {
                                            ui.label(
                                                mono(&format!("- {s}"))
                                                    .color(p.red)
                                                    .strikethrough(),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    });
            });
    });
}

/// A coloured strip across the panel (the pane's side colour).
fn strip(ui: &mut egui::Ui, color: egui::Color32, h: f32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, color);
}

/// An outline button in `color` (text in the colour unless it is the line
/// colour); a picked one gets the second panel fill.
fn outline_button(
    ui: &mut egui::Ui,
    text: &str,
    color: egui::Color32,
    picked: bool,
) -> egui::Response {
    let p = theme::palette(ui.ctx());
    let fg = if color == p.line { p.text } else { color };
    let stroke = if picked { p.gold_text } else { color };
    ui.add(
        egui::Button::new(egui::RichText::new(text).size(12.0).color(fg))
            .fill(if picked {
                p.panel2
            } else {
                egui::Color32::TRANSPARENT
            })
            .stroke(egui::Stroke::new(1.0, stroke)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use kls_github::conflicts::ConflictFile;
    use kls_github::merge3::Merge3;

    fn set(conflicting: bool) -> ConflictSet {
        let merge = if conflicting {
            Merge3::new("a\n", "b\n", "c\n")
        } else {
            Merge3::new("a\n", "b\n", "a\n")
        };
        ConflictSet {
            repo: "acme/api".into(),
            number: 3,
            title: "T".into(),
            head_repo: "acme/api".into(),
            head_ref: "feat".into(),
            head_sha: "h".into(),
            base_ref: "main".into(),
            base_sha: "b".into(),
            merge_base: "m".into(),
            files: vec![ConflictFile {
                path: "src/a.rs".into(),
                previous: None,
                base: Some("a\n".into()),
                ours: Some("b\n".into()),
                theirs: Some("c\n".into()),
                binary: false,
                conflicting: merge.has_conflicts(),
                merge,
            }],
            base_only: Vec::new(),
            forge: kls_github::ForgeKind::Github,
        }
    }

    #[test]
    fn hand_off_says_why_it_is_disabled() {
        let ok = || Ok::<_, String>("n1".to_string());
        assert_eq!(hand_off_block(&set(true), false, ok()), None);
        assert_eq!(
            hand_off_block(&set(true), false, Err("Factory stopped".into())).as_deref(),
            Some("Factory stopped")
        );
        assert_eq!(
            hand_off_block(&set(true), false, Err("no node is online".into())).as_deref(),
            Some("no node is online")
        );
        assert_eq!(
            hand_off_block(&set(false), false, ok()).as_deref(),
            Some("No conflicts")
        );
        assert_eq!(
            hand_off_block(&set(true), true, ok()).as_deref(),
            Some("Sending")
        );
    }
}

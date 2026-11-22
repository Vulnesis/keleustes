//! Worker controls on the Workforce screen: pause, resume, stop, start,
//! back up a worker's profile, crew pause/resume all, a crew's A2A thread
//! and a log tail. Every control goes through
//! `kls_core::vault::workforce::worker_control` (kill switch, A2A blackout,
//! audit with the `fleet/workforce` origin).

use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::screens::harness_a2a::CrewThread;
use crate::theme::{self, palette};
use crate::{icons, kit};
use egui::RichText;
use kls_core::belai::status::NodeStatus;
use kls_core::hosts::Origin;
use kls_core::sessions::Entry;
use kls_core::vault::workforce::{Control, worker_control};
use kls_store::Node;

/// Lines a log tail shows.
const TAIL: i64 = 40;

pub struct LogTail {
    pub session: String,
    data: Loader<Vec<Entry>>,
}

impl LogTail {
    pub fn new(model: &Model, session: &str) -> Self {
        let sid = session.to_string();
        let data = Loader::new(model, &["session."], move |core| {
            let sid = sid.clone();
            async move {
                let next = core
                    .store()
                    .session(&sid)
                    .await?
                    .map(|s| s.next_index)
                    .unwrap_or(0);
                core.store().entries(&sid, (next - TAIL).max(0), TAIL).await
            }
        });
        Self {
            session: session.into(),
            data,
        }
    }
}

/// What the controls keep between frames.
#[derive(Default)]
pub struct ControlsState {
    pub log: Option<LogTail>,
    pub thread: Option<CrewThread>,
    start_node: String,
    start_profile: String,
    start_dir: String,
    /// The One-off worker form (#55), when open.
    adhoc: Option<AdhocForm>,
    /// The crew run whose scope editor is open, and its edits in progress:
    /// weight per directory, replicas per member, the directory to add.
    scope: Option<ScopeEdit>,
}

/// The scope and replicas editor of one crew run (#10). The node reports
/// scope paths and member replicas, not weights, so a weight starts at the
/// minimum until it is set here.
#[derive(Default)]
struct ScopeEdit {
    run: String,
    weights: std::collections::BTreeMap<String, i32>,
    replicas: std::collections::BTreeMap<String, u32>,
    add_dir: String,
    add_weight: i32,
}

/// (profile, replicas) from a crew run's `members_json`.
fn crew_members(json: &str) -> Vec<(String, u32)> {
    serde_json::from_str::<Vec<serde_json::Value>>(json)
        .unwrap_or_default()
        .iter()
        .filter_map(|m| {
            let p = m.get("profile")?.as_str()?.to_string();
            let r = m.get("replicas").and_then(|v| v.as_u64()).unwrap_or(1);
            Some((p, r.min(u64::from(u32::MAX)) as u32))
        })
        .collect()
}

fn origin() -> Origin {
    Origin::fleet("workforce")
}

fn send(model: &Model, node: &str, c: Control) {
    let node = node.to_string();
    model.act(move |m| async move {
        let ids = worker_control(m.core(), &node, c, origin()).await?;
        Ok(Some(Notice::Info(format!("Sent {}", ids.len()))))
    });
}

pub fn ui(
    ui: &mut egui::Ui,
    model: &Model,
    st: &mut ControlsState,
    status: &[(String, i64, NodeStatus)],
    nodes: &[Node],
    profiles: &[String],
) {
    let p = palette(ui.ctx());
    ui.add_space(12.0);
    start_ui(ui, model, st, nodes, profiles);
    for (node_id, _at, ns) in status {
        if ns.workers.is_empty() && ns.crew_runs.is_empty() {
            continue;
        }
        let Some(n) = nodes.iter().find(|n| &n.id == node_id) else {
            continue;
        };
        let caps = &n.host.capabilities;
        ui.add_space(10.0);
        ui.label(kit::caps(
            &icons::label(
                icons::HARD_DRIVES,
                &format!("{} · {} sessions", n.name, ns.running_sessions),
            ),
            p.muted,
        ));
        for w in &ns.workers {
            ui.horizontal(|ui| {
                kit::chip(ui, &w.state, kit::status_color(&p, &w.state));
                ui.label(RichText::new(&w.profile).strong())
                    .on_hover_text(&w.id);
                if !w.crew.is_empty() {
                    ui.label(kit::mono(
                        &icons::label(icons::USERS_THREE, &w.crew),
                        12.0,
                        p.muted,
                    ));
                }
                ui.label(kit::mono(&w.cwd, 12.0, p.muted));
                let paused = w.state == "paused";
                let kind = if paused { "resume" } else { "pause" };
                if caps.takes_dispatch(kind)
                    && kit::ghost_button(
                        ui,
                        &icons::label(
                            if paused { icons::PLAY } else { icons::PAUSE },
                            if paused { "Resume" } else { "Pause" },
                        ),
                    )
                    .clicked()
                {
                    let worker = w.id.clone();
                    send(
                        model,
                        node_id,
                        if paused {
                            Control::Resume { worker }
                        } else {
                            Control::Pause { worker }
                        },
                    );
                }
                if caps.takes_dispatch("library_backup")
                    && kit::ghost_button(ui, &icons::label(icons::ARCHIVE, "Back up"))
                        .on_hover_text("Keep the node's copy of this profile in the library")
                        .clicked()
                {
                    let (node, profile) = (node_id.clone(), w.profile.clone());
                    model.act(move |m| async move {
                        kls_core::hosts::library_dispatch(
                            m.core(),
                            &node,
                            "library_backup",
                            vec![kls_core::gateway::LibraryRef {
                                kind: "profile".into(),
                                name: profile,
                                ..Default::default()
                            }],
                            false,
                            origin(),
                        )
                        .await?;
                        Ok(Some(Notice::Info("Backup asked".into())))
                    });
                }
                if !w.session_id.is_empty() {
                    let on = st.log.as_ref().is_some_and(|l| l.session == w.session_id);
                    if kit::toggle_chip(ui, &icons::label(icons::TERMINAL, "Log"), on).clicked() {
                        st.log = (!on).then(|| LogTail::new(model, &w.session_id));
                    }
                    if caps.takes_dispatch("stop")
                        && kit::danger_button(ui, &icons::label(icons::STOP, "Stop")).clicked()
                    {
                        send(
                            model,
                            node_id,
                            Control::Stop {
                                session: w.session_id.clone(),
                            },
                        );
                    }
                }
            });
            if let Some(l) = &st.log
                && l.session == w.session_id
                && !w.session_id.is_empty()
            {
                log_ui(ui, l);
            }
        }
        for r in &ns.crew_runs {
            let members: Vec<String> = ns
                .workers
                .iter()
                .filter(|w| w.run == r.id || (w.run.is_empty() && w.crew == r.crew))
                .map(|w| w.id.clone())
                .collect();
            ui.horizontal(|ui| {
                kit::chip(ui, &r.state, kit::status_color(&p, &r.state));
                ui.label(RichText::new(icons::label(icons::USERS_THREE, &r.crew)).strong())
                    .on_hover_text(&r.id);
                ui.label(kit::mono(
                    &format!("{} workers", members.len()),
                    12.0,
                    p.muted,
                ));
                if caps.takes_dispatch("crew_control") && !members.is_empty() {
                    for (paused, icon, word) in [
                        (true, icons::PAUSE, "Pause all"),
                        (false, icons::PLAY, "Resume all"),
                    ] {
                        if kit::ghost_button(ui, &icons::label(icon, word)).clicked() {
                            send(
                                model,
                                node_id,
                                Control::Crew {
                                    run: r.id.clone(),
                                    crew: r.crew.clone(),
                                    workers: members.clone(),
                                    paused,
                                },
                            );
                        }
                    }
                }
                if caps.takes_dispatch("crew_control") {
                    let on = st.scope.as_ref().is_some_and(|s| s.run == r.id);
                    if kit::toggle_chip(ui, &icons::label(icons::SLIDERS, "Scope"), on).clicked() {
                        st.scope = (!on).then(|| ScopeEdit {
                            run: r.id.clone(),
                            replicas: crew_members(&r.members_json).into_iter().collect(),
                            add_weight: 1,
                            ..Default::default()
                        });
                    }
                }
                let on = st.thread.as_ref().is_some_and(|t| t.crew() == r.crew);
                if kit::toggle_chip(ui, &icons::label(icons::CHAT_CIRCLE_DOTS, "Thread"), on)
                    .clicked()
                {
                    st.thread = (!on).then(|| CrewThread::new(model, &r.crew));
                }
            });
            if let Some(se) = st.scope.as_mut()
                && se.run == r.id
            {
                kit::card(ui)
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| scope_ui(ui, model, node_id, n, r, se));
            }
            if let Some(t) = st.thread.as_mut()
                && t.crew() == r.crew
            {
                kit::card(ui)
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| t.ui(ui, model));
            }
        }
    }
}

/// A crew run's scope: weight, pause and removal per directory, adding a
/// trusted directory, and replicas per member. Each change is one
/// `crew_control` through `worker_control` (kill switch, A2A, audit).
fn scope_ui(
    ui: &mut egui::Ui,
    model: &Model,
    node_id: &str,
    n: &Node,
    r: &kls_core::belai::status::CrewRun,
    se: &mut ScopeEdit,
) {
    use kls_core::vault::workforce::{CREW_WEIGHT, CrewEdit, MAX_REPLICAS};
    let p = palette(ui.ctx());
    let edit = |e: CrewEdit| Control::CrewEdit {
        run: r.id.clone(),
        crew: r.crew.clone(),
        edit: e,
    };
    ui.label(kit::caps(&icons::label(icons::FOLDER, "Scope"), p.muted));
    egui::Grid::new(("crew_scope", &r.id))
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            for d in &r.scope_dirs {
                ui.label(kit::mono(d, 12.0, p.text));
                ui.horizontal(|ui| {
                    let w = se.weights.entry(d.clone()).or_insert(*CREW_WEIGHT.start());
                    ui.add(egui::Slider::new(w, CREW_WEIGHT).text("weight"));
                    if kit::ghost_button(ui, &icons::label(icons::FLOPPY_DISK, "Set")).clicked() {
                        send(
                            model,
                            node_id,
                            edit(CrewEdit::Weight {
                                dir: d.clone(),
                                weight: *w,
                            }),
                        );
                    }
                    for (paused, icon, word) in [
                        (true, icons::PAUSE, "Pause"),
                        (false, icons::PLAY, "Resume"),
                    ] {
                        if kit::ghost_button(ui, &icons::label(icon, word)).clicked() {
                            send(
                                model,
                                node_id,
                                edit(CrewEdit::PauseDir {
                                    dir: d.clone(),
                                    paused,
                                }),
                            );
                        }
                    }
                    if r.scope_dirs.len() > 1
                        && kit::danger_button(ui, &icons::label(icons::TRASH, "Remove")).clicked()
                    {
                        send(model, node_id, edit(CrewEdit::RemoveDir { dir: d.clone() }));
                    }
                });
                ui.end_row();
            }
            let offer: Vec<&String> = n
                .host
                .trusted_dirs
                .iter()
                .filter(|d| !r.scope_dirs.contains(d))
                .collect();
            if !offer.is_empty() {
                egui::ComboBox::from_id_salt(("crew_scope_add", &r.id))
                    .selected_text(icons::label(
                        icons::FOLDER_PLUS,
                        if se.add_dir.is_empty() {
                            "directory"
                        } else {
                            &se.add_dir
                        },
                    ))
                    .show_ui(ui, |ui| {
                        for d in &offer {
                            ui.selectable_value(&mut se.add_dir, (*d).clone(), d.as_str());
                        }
                    });
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(&mut se.add_weight, CREW_WEIGHT).text("weight"));
                    let can = offer.iter().any(|d| **d == se.add_dir);
                    if ui
                        .add_enabled_ui(can, |ui| {
                            kit::ghost_button(ui, &icons::label(icons::PLUS, "Add"))
                        })
                        .inner
                        .clicked()
                    {
                        send(
                            model,
                            node_id,
                            edit(CrewEdit::AddDir {
                                dir: se.add_dir.clone(),
                                weight: se.add_weight,
                            }),
                        );
                        se.add_dir.clear();
                    }
                });
                ui.end_row();
            }
        });
    if se.replicas.is_empty() {
        return;
    }
    ui.add_space(4.0);
    ui.label(kit::caps(
        &icons::label(icons::USERS_THREE, "Replicas"),
        p.muted,
    ));
    egui::Grid::new(("crew_replicas", &r.id))
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            for (profile, count) in se.replicas.iter_mut() {
                ui.label(icons::label(icons::ROBOT, profile));
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(count, 0..=MAX_REPLICAS as u32));
                    if kit::ghost_button(ui, &icons::label(icons::FLOPPY_DISK, "Set")).clicked() {
                        send(
                            model,
                            node_id,
                            edit(CrewEdit::Replicas {
                                profile: profile.clone(),
                                replicas: *count,
                            }),
                        );
                    }
                });
                ui.end_row();
            }
        });
}

fn log_ui(ui: &mut egui::Ui, l: &LogTail) {
    let p = palette(ui.ctx());
    kit::card(ui)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            match l.data.get() {
                Some(Ok(rows)) if rows.is_empty() => {
                    ui.label(RichText::new(icons::label(icons::MINUS, "No lines")).color(p.muted));
                }
                Some(Ok(rows)) => {
                    egui::ScrollArea::vertical()
                        .id_salt(("log", &l.session))
                        .max_height(220.0)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for e in rows.iter() {
                                let line: String = e
                                    .text
                                    .lines()
                                    .next()
                                    .unwrap_or("")
                                    .chars()
                                    .take(200)
                                    .collect();
                                ui.label(kit::mono(
                                    &format!("{:>10} {line}", e.role),
                                    12.0,
                                    p.text,
                                ));
                            }
                        });
                }
                Some(Err(e)) => {
                    ui.colored_label(p.red, e);
                }
                None => {
                    ui.spinner();
                }
            }
        });
}

/// Start a worker: a profile in one of a node's trusted directories.
fn start_ui(
    ui: &mut egui::Ui,
    model: &Model,
    st: &mut ControlsState,
    nodes: &[Node],
    profiles: &[String],
) {
    let takers: Vec<&Node> = nodes
        .iter()
        .filter(|n| n.revoked_at.is_none() && n.host.capabilities.takes_dispatch("worker"))
        .collect();
    if takers.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(icons::label(icons::PLAY, "Start")).strong());
        let node_name = takers
            .iter()
            .find(|n| n.id == st.start_node)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| "node".into());
        egui::ComboBox::from_id_salt("start_node")
            .selected_text(icons::label(icons::HARD_DRIVES, &node_name))
            .show_ui(ui, |ui| {
                for n in &takers {
                    ui.selectable_value(&mut st.start_node, n.id.clone(), &n.name);
                }
            });
        egui::ComboBox::from_id_salt("start_profile")
            .selected_text(icons::label(
                icons::ROBOT,
                if st.start_profile.is_empty() {
                    "agent"
                } else {
                    &st.start_profile
                },
            ))
            .show_ui(ui, |ui| {
                for a in profiles {
                    ui.selectable_value(&mut st.start_profile, a.clone(), a);
                }
            });
        let dirs: Vec<String> = takers
            .iter()
            .find(|n| n.id == st.start_node)
            .map(|n| n.host.trusted_dirs.clone())
            .unwrap_or_default();
        egui::ComboBox::from_id_salt("start_dir")
            .selected_text(icons::label(
                icons::FOLDER,
                if st.start_dir.is_empty() {
                    "directory"
                } else {
                    &st.start_dir
                },
            ))
            .show_ui(ui, |ui| {
                for d in &dirs {
                    ui.selectable_value(&mut st.start_dir, d.clone(), d);
                }
            });
        let can = !st.start_node.is_empty()
            && !st.start_profile.is_empty()
            && dirs.contains(&st.start_dir);
        if ui
            .add_enabled_ui(can, |ui| {
                theme::primary_button(ui, &icons::label(icons::PLAY, "Start"))
            })
            .inner
            .clicked()
        {
            send(
                model,
                &st.start_node,
                Control::Start {
                    profile: st.start_profile.clone(),
                    cwd: st.start_dir.clone(),
                },
            );
        }
        let open = st.adhoc.is_some();
        let adhoc_ok = takers
            .iter()
            .find(|n| n.id == st.start_node)
            .is_some_and(|n| n.host.capabilities.takes_feature("adhoc"));
        if adhoc_ok
            && kit::toggle_chip(ui, &icons::label(icons::SPARKLE, "One-off worker"), open).clicked()
        {
            st.adhoc = if open {
                None
            } else {
                Some(AdhocForm::default())
            };
        }
    });
    if let Some(f) = st.adhoc.as_mut() {
        adhoc_ui(ui, model, f, &st.start_node, &st.start_dir);
    }
}

/// A one-off worker: a prompt in place of a stored profile (#55).
#[derive(Default)]
struct AdhocForm {
    prompt: String,
    tools: String,
    /// LIST[:LABEL,LABEL].
    claim: String,
    to: String,
    publish: String,
    read_only: bool,
}

impl AdhocForm {
    fn spec(&self) -> kls_core::gateway::AdhocWorker {
        let split = |s: &str| -> Vec<String> {
            s.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        };
        let (list, labels) = self.claim.split_once(':').unwrap_or((&self.claim, ""));
        kls_core::gateway::AdhocWorker {
            prompt: self.prompt.trim().into(),
            tools: split(&self.tools),
            claim_lists: split(list),
            claim_labels: split(labels),
            to: self.to.trim().into(),
            publish: self.publish.clone(),
            read_only: self.read_only,
        }
    }
}

fn adhoc_ui(ui: &mut egui::Ui, model: &Model, f: &mut AdhocForm, node: &str, cwd: &str) {
    egui::Grid::new("adhoc_worker")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            ui.label(icons::label(icons::NOTE, "Prompt"));
            ui.add(
                egui::TextEdit::multiline(&mut f.prompt)
                    .desired_rows(3)
                    .desired_width(360.0),
            );
            ui.end_row();
            ui.label(icons::label(icons::WRENCH, "Tools"));
            ui.add(
                egui::TextEdit::singleline(&mut f.tools)
                    .hint_text("all")
                    .desired_width(240.0),
            );
            ui.end_row();
            ui.label(icons::label(icons::KANBAN, "Claim"));
            ui.add(
                egui::TextEdit::singleline(&mut f.claim)
                    .hint_text("backlog:label")
                    .desired_width(240.0),
            );
            ui.end_row();
            ui.label(icons::label(icons::ARROW_RIGHT, "To"));
            ui.add(
                egui::TextEdit::singleline(&mut f.to)
                    .hint_text("review")
                    .desired_width(160.0),
            );
            ui.end_row();
            ui.label(icons::label(icons::GIT_PULL_REQUEST, "Publish"));
            egui::ComboBox::from_id_salt("adhoc_publish")
                .selected_text(if f.publish.is_empty() {
                    "agent"
                } else {
                    f.publish.as_str()
                })
                .show_ui(ui, |ui| {
                    for m in ["none", "draft_pr", "agent"] {
                        ui.selectable_value(&mut f.publish, m.to_string(), m);
                    }
                });
            ui.end_row();
            ui.label(icons::label(icons::EYE, "Read-only"));
            ui.checkbox(&mut f.read_only, "Change nothing");
            ui.end_row();
        });
    let spec = f.spec();
    let check = kls_core::sessions::start::check_adhoc(&spec);
    let can = check.is_ok() && !node.is_empty() && !cwd.is_empty();
    let r = ui.add_enabled_ui(can, |ui| {
        theme::primary_button(ui, &icons::label(icons::PLAY, "Start one-off"))
    });
    if let Err(why) = &check
        && !f.prompt.trim().is_empty()
    {
        ui.colored_label(palette(ui.ctx()).red, why);
    }
    if r.inner.clicked() {
        let (node, cwd) = (node.to_string(), cwd.to_string());
        model.act(move |m| async move {
            let fields = kls_core::sessions::start::StartFields {
                adhoc: Some(spec),
                ..Default::default()
            };
            let id = kls_core::sessions::start::dispatch(
                m.core(),
                &node,
                kls_core::DispatchSpec {
                    kind: "worker".into(),
                    cwd,
                    ..Default::default()
                },
                &fields,
                &Origin {
                    actor: "owner".into(),
                    surface: "fleet/workforce".into(),
                    ..Default::default()
                },
            )
            .await?;
            Ok(Some(Notice::Info(format!("Sent one-off worker {id}"))))
        });
    }
}

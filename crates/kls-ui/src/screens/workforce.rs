//! Workforce screen. Owner: M5 vault and workforce workstream.
//!
//! Agents and crews tabs (card grids), the agent builder (new or edit; a
//! draft comes from a node, every field stays editable) and the crew
//! drawer as a highlighted side panel, and a live workers table per node.
//!
//! `focus` ids: `new` (builder), `edit=<name>` or an agent name/id,
//! `crew=<name>` (drawer), `agents`, `crews`, `workers`.

use crate::loader::Loader;
use crate::model::{Model, Notice};

use crate::theme::{self, palette};
use crate::{icons, kit};
use egui::{Color32, RichText, Stroke, Vec2};
use egui_extras::{Column, TableBuilder};
use kls_core::vault::{
    self as v, AgentDraft, AgentProfile, Crew, CrewMember, NodeWorkers, Provider,
};
use std::sync::{Arc, Mutex};

mod controls;
mod files;
mod markdown;
mod offers;
mod spec;

pub const READY: bool = true;

/// Draft jobs wait this long for a node.
const DRAFT_TIMEOUT_MS: i64 = 120_000;

#[derive(Clone, Default)]
pub struct Data {
    agents: Vec<AgentProfile>,
    crews: Vec<Crew>,
    workers: Vec<NodeWorkers>,
    providers: Vec<Provider>,
    /// Imported agent profiles' origins (library search).
    origins: Vec<kls_store::libsearch::Provenance>,
    /// What waits on a person (cards held, blocked, lapsed claims, questions).
    needs: Vec<kls_core::needs::Checkpoint>,
    /// The autosaved builder, if any.
    draft: Option<String>,
    /// Agent profiles in the library, with their install state.
    library: Vec<v::LibraryAgent>,
    /// Each node's latest status (workers, crew runs).
    status: Vec<(String, i64, kls_core::belai::status::NodeStatus)>,
    nodes: Vec<kls_store::Node>,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Agents,
    Crews,
    Workers,
}

type DraftSlot = Arc<Mutex<Option<Result<AgentDraft, String>>>>;

struct Builder {
    profile: AgentProfile,
    tools: String,
    brief: String,
    draft_provider: String,
    drafting: bool,
    slot: DraftSlot,
    show_svg: bool,
    spec: spec::SpecForm,
    offers: offers::Offers,
    /// Autosave: the last JSON kept and when.
    saved_json: String,
    saved_at: i64,
    /// Nodes to install on when saved; empty: the nodes that hold it.
    targets: Vec<String>,
    /// The avatar job's answer.
    avatar: Arc<Mutex<Option<Result<String, String>>>>,
    avatar_busy: bool,
    /// Markdown pasted to import.
    import: Option<String>,
    /// The files the agent carries (agent B).
    files: files::FilesBox,
}

impl Builder {
    fn new(profile: AgentProfile) -> Self {
        Self {
            tools: profile.tools.join(", "),
            spec: spec::SpecForm::from_meta(&profile.meta_json),
            profile,
            brief: String::new(),
            draft_provider: String::new(),
            drafting: false,
            slot: Arc::default(),
            show_svg: false,
            offers: offers::Offers::default(),
            saved_json: String::new(),
            saved_at: 0,
            targets: Vec::new(),
            avatar: Arc::default(),
            avatar_busy: false,
            import: None,
            files: Default::default(),
        }
    }

    /// A builder restored from autosave.
    fn restore(s: offers::Saved) -> Self {
        let mut b = Builder::new(s.profile);
        b.tools = s.tools;
        b.spec = spec::SpecForm::from_meta(&s.meta_json);
        b
    }

    /// Starts from a builtin profile: its routing and settings, and its
    /// name and prompt when the form has none yet.
    fn seed(&mut self, b: &kls_engine::catalogue::ProfileSpec) {
        let p = &mut self.profile;
        if p.name.is_empty() {
            p.name = b.name.clone();
        }
        if p.display_name.is_empty() {
            p.display_name = b.display_name.clone();
        }
        if p.description.is_empty() {
            p.description = b.description.clone();
        }
        if p.prompt.is_empty() {
            p.prompt = b.system_prompt.clone();
        }
        if self.tools.trim().is_empty() {
            self.tools = b.tools.join(", ");
        }
        if p.avatar_svg.is_empty()
            && let Some(svg) = kls_engine::catalogue::avatar_svg(b)
        {
            p.avatar_svg = svg.to_string();
        }
        self.spec = spec::SpecForm::from_spec(b.clone());
    }
}

pub struct Screen {
    data: Loader<Data>,
    tab: Tab,
    filter: String,
    /// Agent state filter: `working`, `idle`, `inactive`, `notlib`.
    state: Option<&'static str>,
    /// An agent to back up to the library (run with the model at hand).
    backup: Option<String>,
    /// Wall mode: a full-screen control wall of agents and crews.
    wall: bool,
    builder: Option<Builder>,
    drawer: Option<Crew>,
    pending_focus: Option<String>,
    confirm_delete: Option<(bool, String, String)>,
    controls: controls::ControlsState,
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        let data = Loader::new(
            model,
            &[
                "agent.",
                "crew.",
                "session.",
                "node.",
                "budget.provider",
                "card.",
                "library.",
            ],
            |core| async move {
                Ok(Data {
                    agents: core.store().agent_profiles().await?,
                    crews: core.store().crews().await?,
                    workers: v::workers_summary(&core).await?,
                    providers: core.store().providers().await?,
                    origins: kls_core::libsearch::origins(&core)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(p, _)| p)
                        .filter(|p| p.item_kind == "agent")
                        .collect(),
                    needs: core.needs_you().await.unwrap_or_default(),
                    library: v::library_agents(&core).await.unwrap_or_default(),
                    status: core.node_statuses().await.unwrap_or_default(),
                    nodes: core.store().nodes().await.unwrap_or_default(),
                    draft: core
                        .store()
                        .setting(offers::DRAFT_KEY)
                        .await
                        .ok()
                        .flatten()
                        .filter(|s| !s.trim().is_empty()),
                })
            },
        );
        Self {
            data,
            tab: Tab::default(),
            filter: String::new(),
            state: None,
            backup: None,
            wall: false,
            builder: None,
            drawer: None,
            pending_focus: None,
            confirm_delete: None,
            controls: controls::ControlsState::default(),
        }
    }

    pub fn focus(&mut self, id: &str) {
        match id {
            "agents" => self.tab = Tab::Agents,
            "crews" => self.tab = Tab::Crews,
            "workers" => self.tab = Tab::Workers,
            "new" | "?new" | "new=1" => {
                self.tab = Tab::Agents;
                self.builder = Some(Builder::new(AgentProfile::default()));
            }
            other => self.pending_focus = Some(other.to_string()),
        }
    }

    fn resolve_focus(&mut self, data: &Data) {
        let Some(f) = self.pending_focus.take() else {
            return;
        };
        if let Some(name) = f.strip_prefix("crew=") {
            self.tab = Tab::Crews;
            self.drawer = data
                .crews
                .iter()
                .find(|c| c.name == name || c.id == name)
                .cloned();
            return;
        }
        let key = f.strip_prefix("edit=").unwrap_or(&f);
        if let Some(a) = data.agents.iter().find(|a| a.name == key || a.id == key) {
            self.tab = Tab::Agents;
            self.builder = Some(Builder::new(a.clone()));
        } else if let Some(c) = data.crews.iter().find(|c| c.name == key || c.id == key) {
            self.tab = Tab::Crews;
            self.drawer = Some(c.clone());
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let data = match self.data.get() {
            None => {
                kit::page_header(ui, icons::USERS_THREE, "Workforce", |_| {});
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                kit::page_header(ui, icons::USERS_THREE, "Workforce", |_| {});
                ui.colored_label(ui.visuals().error_fg_color, e);
                return;
            }
            Some(Ok(d)) => d,
        };
        self.resolve_focus(&data);

        let wall = &mut self.wall;
        let (cur, builder, drawer) = (&mut self.tab, &mut self.builder, &mut self.drawer);
        kit::page_header(ui, icons::USERS_THREE, "Workforce", |ui| {
            if kit::toggle_chip(ui, &icons::label(icons::GAUGE, "Wall"), *wall).clicked() {
                *wall = !*wall;
            }
            match *cur {
                Tab::Agents => {
                    if theme::primary_button(ui, &icons::label(icons::PLUS, "New agent")).clicked()
                    {
                        *drawer = None;
                        *builder = Some(Builder::new(AgentProfile::default()));
                    }
                    if builder.is_none()
                        && let Some(s) = data.draft.as_deref().and_then(offers::Saved::parse)
                        && kit::ghost_button(
                            ui,
                            &icons::label(icons::ARROW_U_UP_LEFT, "Continue draft"),
                        )
                        .on_hover_text(if s.profile.name.is_empty() {
                            "unnamed".to_string()
                        } else {
                            s.profile.name.clone()
                        })
                        .clicked()
                    {
                        *drawer = None;
                        *builder = Some(Builder::restore(s));
                    }
                }
                Tab::Crews => {
                    if theme::primary_button(ui, &icons::label(icons::PLUS, "New crew")).clicked() {
                        *builder = None;
                        *drawer = Some(new_crew());
                    }
                }
                Tab::Workers => {}
            }
            ui.add_space(8.0);
            for (t, icon, name, n) in [
                (
                    Tab::Workers,
                    icons::HARD_DRIVES,
                    "Workers",
                    data.workers.len(),
                ),
                (Tab::Crews, icons::USERS, "Crews", data.crews.len()),
                (Tab::Agents, icons::ROBOT, "Agents", data.agents.len()),
            ] {
                if kit::tab_rich(
                    ui,
                    RichText::new(format!("{}  {n}", icons::label(icon, name))),
                    *cur == t,
                    30.0,
                )
                .clicked()
                {
                    *cur = t;
                }
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(icons::label(icons::FUNNEL, "Filter"))
                    .desired_width(220.0),
            );
            let p = palette(ui.ctx());
            let live: usize = data.workers.iter().map(|w| w.live_workers).sum();
            let online = data.workers.iter().filter(|w| w.online).count();
            ui.add_space(8.0);
            kit::chip(
                ui,
                &icons::label(icons::LIGHTNING, &format!("{live} live")),
                if live > 0 { p.green } else { p.muted },
            )
            .on_hover_text("Live workers");
            kit::chip(
                ui,
                &icons::label(
                    icons::HARD_DRIVES,
                    &format!("{online} of {} nodes online", data.workers.len()),
                ),
                if online > 0 { p.green } else { p.muted },
            );
        });
        ui.add_space(14.0);

        if self.wall {
            wall_ui(ui, model, &data);
            return;
        }
        let side_open = self.builder.is_some() || self.drawer.is_some();
        let side_w = (ui.available_width() * 0.4).clamp(380.0, 560.0);
        kit::split_scroll(
            ui,
            self,
            "workforce",
            side_open,
            side_w,
            520.0,
            |ui, s| match s.tab {
                Tab::Agents => s.agents_ui(ui, model, &data),
                Tab::Crews => s.crews_ui(ui, &data),
                Tab::Workers => {
                    workers_ui(ui, &data);
                    let profiles: Vec<String> =
                        data.agents.iter().map(|a| a.name.clone()).collect();
                    controls::ui(
                        ui,
                        model,
                        &mut s.controls,
                        &data.status,
                        &data.nodes,
                        &profiles,
                    );
                }
            },
            |ui, s| {
                if s.builder.is_some() {
                    s.builder_ui(ui, model, &data);
                } else {
                    s.drawer_ui(ui, model, &data);
                }
            },
        );
        self.confirm_ui(ui.ctx(), model);
    }

    fn matches(&self, s: &str) -> bool {
        self.filter.is_empty() || s.to_lowercase().contains(&self.filter.to_lowercase())
    }

    fn agents_ui(&mut self, ui: &mut egui::Ui, model: &Model, data: &Data) {
        needs_strip(ui, model, data);
        if let Some(id) = self.backup.take() {
            model.act(move |m| async move {
                let (_, v) = v::agent_to_library(m.core(), &id, "backed up from Workforce").await?;
                Ok(Some(Notice::Info(format!("In the library (v{v})"))))
            });
        }
        let in_lib = |a: &AgentProfile| data.library.iter().any(|l| l.item.name == a.name);
        let live = |a: &AgentProfile| -> usize {
            data.workers
                .iter()
                .flat_map(|w| w.agents.iter())
                .filter(|(n, _)| n == &a.name)
                .map(|(_, c)| *c)
                .sum()
        };
        let state_of = |a: &AgentProfile| -> &'static str {
            if live(a) > 0 {
                "working"
            } else if data
                .crews
                .iter()
                .any(|c| c.members.iter().any(|m| m.agent == a.name))
            {
                "idle"
            } else {
                "inactive"
            }
        };
        ui.horizontal_wrapped(|ui| {
            if kit::toggle_chip(
                ui,
                &format!("All {}", data.agents.len()),
                self.state.is_none(),
            )
            .clicked()
            {
                self.state = None;
            }
            for (st, label) in [
                ("working", "Working"),
                ("idle", "Idle"),
                ("inactive", "Inactive"),
                ("notlib", "Not in library"),
            ] {
                let n = data
                    .agents
                    .iter()
                    .filter(|a| {
                        if st == "notlib" {
                            !in_lib(a)
                        } else {
                            state_of(a) == st
                        }
                    })
                    .count();
                if kit::toggle_chip(ui, &format!("{label} {n}"), self.state == Some(st)).clicked() {
                    self.state = Some(st);
                }
            }
        });
        let rows: Vec<&AgentProfile> = data
            .agents
            .iter()
            .filter(|a| self.matches(&format!("{} {} {}", a.name, a.display_name, a.description)))
            .filter(|a| {
                self.state.is_none_or(|s| {
                    if s == "notlib" {
                        !in_lib(a)
                    } else {
                        state_of(a) == s
                    }
                })
            })
            .collect();
        if rows.is_empty() {
            kit::empty(ui, icons::ROBOT, "No agents");
            if theme::primary_button(ui, &icons::label(icons::PLUS, "New agent")).clicked() {
                self.drawer = None;
                self.builder = Some(Builder::new(AgentProfile::default()));
            }
            return;
        }
        kit::Grid::new(250.0)
            .keep_width()
            .tight_rows()
            .show(ui, rows.len(), |ui, i, _| {
                let a = rows[i];
                self.agent_card(ui, a, data);
            });
        library_pane(ui, model, data);
    }

    /// One agent: tinted mark, name and model; live state and crews below.
    fn agent_card(&mut self, ui: &mut egui::Ui, a: &AgentProfile, data: &Data) {
        let p = palette(ui.ctx());
        let in_crews: Vec<&str> = data
            .crews
            .iter()
            .filter(|c| c.members.iter().any(|m| m.agent == a.name))
            .map(|c| c.name.as_str())
            .collect();
        let live_on: Vec<(&str, usize)> = data
            .workers
            .iter()
            .filter_map(|w| {
                let n: usize = w
                    .agents
                    .iter()
                    .filter(|(n, _)| n == &a.name)
                    .map(|(_, c)| c)
                    .sum();
                (n > 0).then_some((w.name.as_str(), n))
            })
            .collect();
        let live: usize = live_on.iter().map(|(_, n)| n).sum();
        let editing = self
            .builder
            .as_ref()
            .is_some_and(|b| !a.id.is_empty() && b.profile.id == a.id);
        let mut action = None;
        let r = kit::tile(ui, editing, |ui| {
            ui.set_min_height(92.0);
            ui.horizontal(|ui| {
                avatar_tile(ui, &a.name, 44.0);
                ui.vertical(|ui| {
                    let title = if a.display_name.is_empty() {
                        a.name.clone()
                    } else {
                        a.display_name.clone()
                    };
                    ui.add(egui::Label::new(RichText::new(title).size(15.0).strong()).truncate());
                    let model = if a.provider.is_empty() {
                        "routed".to_string()
                    } else if a.model.is_empty() {
                        a.provider.clone()
                    } else {
                        format!("{} · {}", a.provider, a.model)
                    };
                    ui.add(
                        egui::Label::new(
                            RichText::new(model).monospace().size(12.0).color(p.muted),
                        )
                        .truncate(),
                    );
                    if let Some(o) = data
                        .origins
                        .iter()
                        .find(|o| o.item_id == a.id || o.item_id == a.name)
                    {
                        kit::chip(ui, &icons::label(icons::GLOBE, &o.source), p.muted)
                            .on_hover_text(format!(
                                "{} {}",
                                o.ecosystem,
                                o.upstream_commit.chars().take(10).collect::<String>()
                            ));
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    ui.menu_button(icons::DOTS_THREE, |ui| {
                        if ui
                            .button(icons::label(icons::PENCIL_SIMPLE, "Edit"))
                            .clicked()
                        {
                            action = Some(0);
                            ui.close();
                        }
                        if ui.button(icons::label(icons::COPY, "Duplicate")).clicked() {
                            action = Some(1);
                            ui.close();
                        }
                        if !a.id.is_empty()
                            && ui
                                .button(icons::label(icons::BOOKS, "Back up to library"))
                                .clicked()
                        {
                            action = Some(4);
                            ui.close();
                        }
                        if ui.button(icons::label(icons::TRASH, "Delete")).clicked() {
                            action = Some(2);
                            ui.close();
                        }
                    })
                    .response
                    .on_hover_text("More");
                });
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if live > 0 {
                    kit::dot(ui, p.green);
                    let nodes: Vec<&str> = live_on.iter().map(|(n, _)| *n).collect();
                    ui.label(
                        RichText::new(format!("{live} live on {}", nodes.join(", ")))
                            .size(12.0)
                            .color(p.muted),
                    );
                } else {
                    kit::dot(ui, p.muted);
                    ui.label(RichText::new("idle").size(12.0).color(p.muted));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if in_crews.is_empty() {
                        ui.label(RichText::new("No crew").size(12.0).color(p.muted));
                    } else {
                        for c in in_crews.iter().rev() {
                            if ui
                                .link(RichText::new(*c).size(12.0))
                                .on_hover_text("Open crew")
                                .clicked()
                            {
                                action = Some(3);
                                self.tab = Tab::Crews;
                                self.builder = None;
                                self.drawer = data.crews.iter().find(|x| x.name == *c).cloned();
                            }
                        }
                        ui.label(RichText::new(icons::USERS).size(12.0).color(p.muted));
                    }
                });
            });
        });
        let r = if a.description.is_empty() {
            r
        } else {
            r.on_hover_text(&a.description)
        };
        match action {
            Some(0) => self.edit(a.clone()),
            Some(1) => {
                let mut copy = a.clone();
                copy.id.clear();
                copy.name = format!("{}-copy", a.name);
                self.edit(copy);
            }
            Some(2) => self.confirm_delete = Some((true, a.id.clone(), a.name.clone())),
            Some(4) => self.backup = Some(a.id.clone()),
            Some(_) => {}
            None if r.clicked() => self.edit(a.clone()),
            None => {}
        }
    }

    fn edit(&mut self, a: AgentProfile) {
        self.drawer = None;
        self.builder = Some(Builder::new(a));
    }

    fn crews_ui(&mut self, ui: &mut egui::Ui, data: &Data) {
        let crews: Vec<&Crew> = data
            .crews
            .iter()
            .filter(|c| self.matches(&format!("{} {}", c.name, c.description)))
            .collect();
        if crews.is_empty() {
            kit::empty(ui, icons::USERS, "No crews");
            if theme::primary_button(ui, &icons::label(icons::PLUS, "New crew")).clicked() {
                self.builder = None;
                self.drawer = Some(new_crew());
            }
            return;
        }
        kit::Grid::new(250.0)
            .keep_width()
            .tight_rows()
            .show(ui, crews.len(), |ui, i, _| {
                let c = crews[i];
                let p = palette(ui.ctx());
                let live: usize = data
                    .workers
                    .iter()
                    .flat_map(|w| w.agents.iter())
                    .filter(|(n, _)| c.members.iter().any(|m| &m.agent == n))
                    .map(|(_, k)| k)
                    .sum();
                let open = self
                    .drawer
                    .as_ref()
                    .is_some_and(|d| !c.id.is_empty() && d.id == c.id);
                let r = kit::tile(ui, open, |ui| {
                    ui.set_min_height(92.0);
                    ui.horizontal(|ui| {
                        avatar_tile(ui, &c.name, 44.0);
                        ui.vertical(|ui| {
                            ui.add(
                                egui::Label::new(RichText::new(&c.name).size(15.0).strong())
                                    .truncate(),
                            );
                            let lead = c
                                .members
                                .first()
                                .map(|m| format!(" · lead: {}", m.agent))
                                .unwrap_or_default();
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{} · {} members{lead}",
                                        c.process,
                                        c.members.len()
                                    ))
                                    .monospace()
                                    .size(12.0)
                                    .color(p.muted),
                                )
                                .truncate(),
                            );
                        });
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if live > 0 {
                            kit::dot(ui, p.green);
                            ui.label(
                                RichText::new(format!("{live} working"))
                                    .size(12.0)
                                    .color(p.muted),
                            );
                        } else {
                            kit::dot(ui, p.muted);
                            ui.label(RichText::new("idle").size(12.0).color(p.muted));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!("v{}", c.version))
                                    .monospace()
                                    .size(12.0)
                                    .color(p.muted),
                            );
                            for m in c.members.iter().take(6).rev() {
                                avatar(ui, &m.agent, 16.0);
                            }
                        });
                    });
                });
                let r = if c.description.is_empty() {
                    r
                } else {
                    r.on_hover_text(&c.description)
                };
                if r.clicked() {
                    self.builder = None;
                    self.drawer = Some(c.clone());
                }
            });
    }

    fn drawer_ui(&mut self, ui: &mut egui::Ui, model: &Model, data: &Data) {
        let close = std::cell::Cell::new(false);
        let mut save = false;
        let mut delete = None;
        let Some(c) = &mut self.drawer else {
            return;
        };
        let title = if c.id.is_empty() {
            "New crew".to_string()
        } else {
            format!("Crew {}", c.name)
        };
        let version = c.version;
        kit::section(
            ui,
            icons::USERS,
            &title,
            true,
            |ui| {
                if ui.button(icons::X).on_hover_text("Close").clicked() {
                    close.set(true);
                }
                if version > 0 {
                    ui.label(
                        RichText::new(format!("v{version}"))
                            .monospace()
                            .size(12.0)
                            .color(palette(ui.ctx()).muted),
                    );
                }
            },
            |ui| {
                let p = palette(ui.ctx());
                let full = ui.available_width();
                kit::field_label(ui, "Name");
                ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(full));
                ui.add_space(6.0);
                kit::field_label(ui, "Description");
                ui.add(
                    egui::TextEdit::multiline(&mut c.description)
                        .desired_rows(2)
                        .desired_width(full),
                );
                ui.add_space(6.0);
                kit::field_label(ui, "Process");
                ui.horizontal(|ui| {
                    for pr in ["sequential", "parallel", "hierarchical"] {
                        if kit::pill(ui, pr, c.process == pr).clicked() {
                            c.process = pr.to_string();
                        }
                    }
                });
                ui.add_space(10.0);
                ui.label(kit::caps(&icons::label(icons::USERS, "Members"), p.muted));
                let mut remove = None;
                for (i, m) in c.members.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        avatar(ui, &m.agent, 18.0);
                        egui::ComboBox::from_id_salt(("crew_member", i))
                            .selected_text(if m.agent.is_empty() {
                                "agent"
                            } else {
                                m.agent.as_str()
                            })
                            .show_ui(ui, |ui| {
                                for a in &data.agents {
                                    ui.selectable_value(&mut m.agent, a.name.clone(), &a.name);
                                }
                            });
                        ui.add(
                            egui::TextEdit::singleline(&mut m.role)
                                .hint_text("role")
                                .desired_width(90.0),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut m.model)
                                .hint_text("provider/model")
                                .desired_width(130.0),
                        )
                        .on_hover_text("Model for this member");
                        ui.add(
                            egui::DragValue::new(&mut m.count)
                                .range(1..=v::workforce::MAX_REPLICAS)
                                .prefix("×"),
                        );
                        if ui
                            .small_button(icons::TRASH)
                            .on_hover_text("Remove member")
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    c.members.remove(i);
                }
                if kit::ghost_button(ui, &icons::label(icons::USER_PLUS, "Add")).clicked() {
                    c.members.push(CrewMember {
                        count: 1,
                        ..Default::default()
                    });
                }
                if !c.id.is_empty() {
                    ui.add_space(10.0);
                    ui.label(kit::caps(&icons::label(icons::PLAY, "Working"), p.muted));
                    let mut any = false;
                    for w in &data.workers {
                        for (agent, n) in &w.agents {
                            if c.members.iter().any(|m| &m.agent == agent) {
                                any = true;
                                ui.horizontal(|ui| {
                                    kit::dot(ui, p.green);
                                    ui.label(format!(
                                        "{agent} ×{n} · {} {}",
                                        icons::HARD_DRIVES,
                                        w.name
                                    ));
                                });
                            }
                        }
                    }
                    if !any {
                        ui.label(RichText::new(icons::label(icons::MINUS, "None")).color(p.muted));
                    }
                }
                let members: Vec<(String, i64)> = c
                    .members
                    .iter()
                    .map(|m| (m.agent.clone(), m.count))
                    .collect();
                let resolve = |n: &str| {
                    data.agents
                        .iter()
                        .find(|a| a.name == n)
                        .map(|a| {
                            let mut s = kls_engine::catalogue::ProfileSpec::parse(&a.meta_json);
                            s.name = a.name.clone();
                            s
                        })
                        .filter(|s| s.kanban.is_some())
                        .or_else(|| kls_engine::catalogue::profile(n).cloned())
                };
                let problems = kls_engine::catalogue::crew_problems(&members, &resolve);
                let blocking = problems.iter().any(|x| {
                    [
                        "replicas",
                        "at least one member",
                        "claim the same cards",
                        "no member claims",
                    ]
                    .iter()
                    .any(|k| x.contains(k))
                });
                for pr in &problems {
                    ui.label(RichText::new(icons::label(icons::WARNING, pr)).color(p.amber));
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled_ui(!blocking, |ui| {
                            theme::primary_button(
                                ui,
                                &icons::label(icons::FLOPPY_DISK, "Save crew"),
                            )
                        })
                        .inner
                        .clicked()
                    {
                        save = true;
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        close.set(true);
                    }
                    if !c.id.is_empty()
                        && kit::danger_button(ui, &icons::label(icons::TRASH, "Delete")).clicked()
                    {
                        delete = Some((c.id.clone(), c.name.clone()));
                    }
                });
            },
        );
        if save {
            let crew = c.clone();
            model.act(move |m| async move {
                let (c, p) = v::save_and_publish_crew(
                    m.core(),
                    &crew,
                    None,
                    kls_core::hosts::Origin::fleet("workforce"),
                )
                .await?;
                Ok(Some(publish_notice(&c.name, c.version, &p)))
            });
            close.set(true);
        }
        if let Some((id, name)) = delete {
            self.confirm_delete = Some((false, id, name));
        }
        if close.get() {
            self.drawer = None;
        }
    }

    fn builder_ui(&mut self, ui: &mut egui::Ui, model: &Model, data: &Data) {
        let Some(b) = &mut self.builder else {
            return;
        };
        // A finished draft fills the form; the person edits from there.
        let done = b.slot.lock().unwrap().take();
        if let Some(res) = done {
            b.drafting = false;
            match res {
                Ok(d) => {
                    if b.profile.avatar_svg.is_empty() && !d.avatar_svg.is_empty() {
                        b.profile.avatar_svg = d.avatar_svg.clone();
                    }
                    b.offers = offers::Offers {
                        draft: Some(d),
                        ..Default::default()
                    };
                }
                Err(e) => model.notify(Notice::Error(e)),
            }
        }
        let close = std::cell::Cell::new(false);
        let title = if b.profile.id.is_empty() {
            "Agent builder".to_string()
        } else {
            format!("Edit {}", b.profile.name)
        };
        let version = b.profile.version;
        kit::section(
            ui,
            icons::ROBOT,
            &title,
            true,
            |ui| {
                if ui.button(icons::X).on_hover_text("Close").clicked() {
                    close.set(true);
                }
                if version > 0 {
                    ui.label(
                        RichText::new(format!("v{version}"))
                            .monospace()
                            .size(12.0)
                            .color(palette(ui.ctx()).muted),
                    );
                }
            },
            |ui| {
                let p = palette(ui.ctx());
                let full = ui.available_width();
                kit::field_label(ui, &icons::label(icons::SPARKLE, "Brief"))
                    .on_hover_text("A node drafts the profile; nothing is saved until you save");
                ui.add(
                    egui::TextEdit::multiline(&mut b.brief)
                        .hint_text("Patch vulnerable npm deps")
                        .desired_rows(3)
                        .desired_width(full),
                );
                ui.horizontal_wrapped(|ui| {
                    egui::ComboBox::from_id_salt("draft_provider")
                        .selected_text(if b.draft_provider.is_empty() {
                            "routed provider"
                        } else {
                            b.draft_provider.as_str()
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut b.draft_provider,
                                String::new(),
                                "routed provider",
                            );
                            for pr in data.providers.iter().filter(|x| x.factory) {
                                ui.selectable_value(&mut b.draft_provider, pr.id.clone(), &pr.name);
                            }
                        });
                    let can = !b.drafting && !b.brief.trim().is_empty();
                    let label = if b.profile.name.is_empty() && b.profile.prompt.is_empty() {
                        "Draft from brief"
                    } else {
                        "Redraft from brief"
                    };
                    if ui
                        .add_enabled_ui(can, |ui| {
                            gold_outline_button(ui, &icons::label(icons::SPARKLE, label))
                        })
                        .inner
                        .clicked()
                    {
                        b.drafting = true;
                        let (brief, prov, slot) =
                            (b.brief.clone(), b.draft_provider.clone(), b.slot.clone());
                        model.act(move |m| async move {
                            let pr = (!prov.is_empty()).then_some(prov.as_str());
                            let r = v::draft_agent(m.core(), &brief, pr, DRAFT_TIMEOUT_MS)
                                .await
                                .map_err(|e| format!("{e:#}"));
                            *slot.lock().unwrap() = Some(r);
                            m.egui_ctx().request_repaint();
                            Ok(None)
                        });
                    }
                    if let Some(bp) = spec::builtin_picker(ui) {
                        b.seed(bp);
                    }
                    if b.drafting {
                        ui.spinner();
                        ui.label(
                            RichText::new(icons::label(icons::CLOCK, "Waiting…")).color(p.muted),
                        );
                    }
                });
                ui.add_space(10.0);
                b.offers.ui(ui, &mut b.profile, &mut b.tools);
                let pr = &mut b.profile;
                kit::field_label(ui, "Name");
                ui.add(
                    egui::TextEdit::singleline(&mut pr.name)
                        .hint_text("patcher")
                        .desired_width(full),
                );
                ui.add_space(6.0);
                kit::field_label(ui, "Display name");
                ui.add(egui::TextEdit::singleline(&mut pr.display_name).desired_width(full));
                ui.add_space(6.0);
                kit::field_label(ui, "Description");
                ui.add(
                    egui::TextEdit::multiline(&mut pr.description)
                        .desired_rows(2)
                        .desired_width(full),
                );
                ui.add_space(6.0);
                let models: Vec<String> = data
                    .providers
                    .iter()
                    .find(|x| x.id == pr.provider)
                    .map(|x| x.models.clone())
                    .unwrap_or_default();
                ui.columns(2, |cols| {
                    kit::field_label(&mut cols[0], "Provider");
                    let w0 = cols[0].available_width() - 8.0;
                    egui::ComboBox::from_id_salt("agent_provider")
                        .width(w0)
                        .selected_text(if pr.provider.is_empty() {
                            "routed"
                        } else {
                            pr.provider.as_str()
                        })
                        .show_ui(&mut cols[0], |ui| {
                            ui.selectable_value(&mut pr.provider, String::new(), "routed");
                            for x in &data.providers {
                                ui.selectable_value(&mut pr.provider, x.id.clone(), &x.name);
                            }
                        });
                    kit::field_label(&mut cols[1], "Model");
                    cols[1].horizontal(|ui| {
                        let w = ui.available_width() - if models.is_empty() { 0.0 } else { 32.0 };
                        ui.add(
                            egui::TextEdit::singleline(&mut pr.model)
                                .hint_text("provider default")
                                .desired_width(w),
                        );
                        if !models.is_empty() {
                            ui.menu_button(icons::CARET_DOWN, |ui| {
                                for m in &models {
                                    if ui.button(m).clicked() {
                                        pr.model = m.clone();
                                        ui.close();
                                    }
                                }
                            })
                            .response
                            .on_hover_text("Pick model");
                        }
                    });
                });
                ui.add_space(6.0);
                kit::field_label(ui, "System prompt");
                ui.add(
                    egui::TextEdit::multiline(&mut pr.prompt)
                        .desired_rows(8)
                        .desired_width(full)
                        .font(egui::TextStyle::Monospace),
                );
                ui.add_space(6.0);
                kit::field_label(ui, "Tools");
                ui.add(
                    egui::TextEdit::singleline(&mut b.tools)
                        .hint_text("Bash, Edit, Read")
                        .desired_width(full),
                );
                ui.horizontal_wrapped(|ui| {
                    for (name, group) in markdown::TOOL_GROUPS {
                        if kit::ghost_button(ui, &icons::label(icons::PLUS, name))
                            .on_hover_text(group.join(", "))
                            .clicked()
                        {
                            b.tools = markdown::add_group(&b.tools, group);
                        }
                    }
                });
                b.spec.ui(ui);
                ui.add_space(6.0);
                kit::field_label(ui, &icons::label(icons::SPARKLE, "Palette"));
                ui.horizontal(|ui| {
                    let pal = &mut b.spec.spec.palette;
                    pal.resize(4, String::new());
                    for c in pal.iter_mut() {
                        let mut rgb = hex_color(c)
                            .map(|x| [x.r(), x.g(), x.b()])
                            .unwrap_or([128; 3]);
                        if ui.color_edit_button_srgb(&mut rgb).changed() {
                            *c = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
                        }
                    }
                    if ui.small_button(icons::X).on_hover_text("Clear").clicked() {
                        pal.iter_mut().for_each(String::clear);
                    }
                    pal.retain(|c| !c.is_empty());
                });
                ui.add_space(6.0);
                let agent_id = b.profile.id.clone();
                b.files.ui(ui, model, &agent_id, full);
                let pr = &mut b.profile;
                ui.add_space(6.0);
                kit::field_label(ui, "Avatar");
                ui.horizontal(|ui| {
                    avatar_tile(ui, &pr.name, 32.0);
                    ui.label(
                        RichText::new(if pr.avatar_svg.is_empty() {
                            "initials"
                        } else {
                            "SVG set"
                        })
                        .color(p.muted),
                    );
                    ui.checkbox(&mut b.show_svg, icons::label(icons::CODE, "SVG"));
                    if let Some(r) = b.avatar.lock().unwrap().take() {
                        b.avatar_busy = false;
                        match r {
                            Ok(svg) => pr.avatar_svg = svg,
                            Err(e) => model.notify(Notice::Error(e)),
                        }
                    }
                    if b.avatar_busy {
                        ui.spinner();
                    } else if ui
                        .add_enabled_ui(kls_core::vault::valid_name(&pr.name), |ui| {
                            gold_outline_button(ui, &icons::label(icons::SPARKLE, "Draw"))
                        })
                        .inner
                        .on_hover_text("A node draws an avatar (5 an hour)")
                        .clicked()
                    {
                        b.avatar_busy = true;
                        let brief = kls_core::vault::workforce::AvatarBrief {
                            display_name: if pr.display_name.is_empty() {
                                pr.name.clone()
                            } else {
                                pr.display_name.clone()
                            },
                            palette: b.spec.spec.palette.clone(),
                            focus: pr.description.clone(),
                            ..Default::default()
                        };
                        let (name, provider, slot) =
                            (pr.name.clone(), pr.provider.clone(), b.avatar.clone());
                        model.act(move |m| async move {
                            let r = kls_core::vault::draw_avatar(
                                m.core(),
                                &name,
                                &brief,
                                Some(&provider),
                                "owner",
                                DRAFT_TIMEOUT_MS,
                            )
                            .await
                            .map_err(|e| format!("{e:#}"));
                            *slot.lock().unwrap() = Some(r);
                            m.egui_ctx().request_repaint();
                            Ok(None)
                        });
                    }
                });
                if b.show_svg {
                    ui.add(
                        egui::TextEdit::multiline(&mut pr.avatar_svg)
                            .desired_rows(4)
                            .desired_width(full)
                            .font(egui::TextStyle::Monospace),
                    );
                }
                let mut checks = checks(&b.profile, &b.tools, data);
                checks.extend(b.spec.problems().into_iter().map(|t| (false, t)));
                if !checks.is_empty() {
                    ui.add_space(8.0);
                    for (warn, text) in &checks {
                        let (color, glyph) = if *warn {
                            (p.amber, icons::WARNING)
                        } else {
                            (p.red, icons::X_CIRCLE)
                        };
                        ui.label(RichText::new(icons::label(glyph, text)).color(color));
                    }
                }
                ui.add_space(12.0);
                let installers: Vec<&kls_store::Node> = data
                    .nodes
                    .iter()
                    .filter(|n| {
                        n.revoked_at.is_none()
                            && n.host.capabilities.takes_dispatch("library_install")
                    })
                    .collect();
                if !installers.is_empty() {
                    ui.add_space(8.0);
                    kit::field_label(ui, &icons::label(icons::DOWNLOAD_SIMPLE, "Install on"))
                        .on_hover_text("None picked: the nodes that already hold it");
                    ui.horizontal_wrapped(|ui| {
                        for n in installers {
                            let on = b.targets.contains(&n.id);
                            if kit::toggle_chip(ui, &icons::label(icons::HARD_DRIVES, &n.name), on)
                                .clicked()
                            {
                                if on {
                                    b.targets.retain(|t| t != &n.id);
                                } else {
                                    b.targets.push(n.id.clone());
                                }
                            }
                        }
                    });
                }
                if let Some(text) = b.import.as_mut() {
                    ui.add_space(8.0);
                    kit::field_label(ui, &icons::label(icons::UPLOAD_SIMPLE, "Markdown"));
                    ui.add(
                        egui::TextEdit::multiline(text)
                            .desired_rows(6)
                            .desired_width(full)
                            .font(egui::TextStyle::Monospace),
                    );
                    let mut done = false;
                    ui.horizontal(|ui| {
                        if kit::ghost_button(ui, &icons::label(icons::CHECK, "Import")).clicked() {
                            match markdown::from_markdown(text) {
                                Some((prof, tools, meta)) => {
                                    let keep = (b.profile.id.clone(), b.profile.version);
                                    b.profile = AgentProfile {
                                        id: keep.0,
                                        version: keep.1,
                                        avatar_svg: b.profile.avatar_svg.clone(),
                                        ..prof
                                    };
                                    b.tools = tools;
                                    if !meta.is_empty() {
                                        b.spec = spec::SpecForm::from_meta(&meta);
                                    }
                                    done = true;
                                }
                                None => model.notify(Notice::Error(
                                    "Not an agent: front matter with a name".into(),
                                )),
                            }
                        }
                        if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                            done = true;
                        }
                    });
                    if done {
                        b.import = None;
                    }
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let blocking = checks.iter().any(|(w, _)| !w);
                    let save = ui
                        .add_enabled_ui(!blocking, |ui| {
                            theme::primary_button(
                                ui,
                                &icons::label(icons::FLOPPY_DISK, "Save agent"),
                            )
                        })
                        .inner
                        .on_hover_text("Save, back up to the library, install on its nodes")
                        .clicked();
                    if save {
                        let mut prof = b.profile.clone();
                        prof.meta_json = b.spec.meta_json();
                        prof.tools = b
                            .tools
                            .split([',', '\n'])
                            .map(|t| t.trim().to_string())
                            .filter(|t| !t.is_empty())
                            .collect();
                        let targets = b.targets.clone();
                        model.act(move |m| async move {
                            let t = (!targets.is_empty()).then_some(targets.as_slice());
                            let (a, p) = v::save_and_publish_agent(
                                m.core(),
                                &prof,
                                t,
                                kls_core::hosts::Origin::fleet("workforce"),
                            )
                            .await?;
                            Ok(Some(publish_notice(&a.name, a.version, &p)))
                        });
                        close.set(true);
                    }
                    if kit::ghost_button(ui, icons::FILE_TEXT)
                        .on_hover_text("Copy as Markdown")
                        .clicked()
                    {
                        ui.ctx().copy_text(markdown::to_markdown(
                            &b.profile,
                            &b.tools,
                            &b.spec.meta_json(),
                        ));
                    }
                    if kit::ghost_button(ui, icons::UPLOAD_SIMPLE)
                        .on_hover_text("Import Markdown")
                        .clicked()
                    {
                        b.import = Some(String::new());
                    }
                    if kit::ghost_button(ui, icons::CODE)
                        .on_hover_text("Copy as belai profile JSON")
                        .clicked()
                    {
                        let mut s = b.spec.build();
                        s.name = b.profile.name.clone();
                        s.display_name = b.profile.display_name.clone();
                        s.description = b.profile.description.clone();
                        s.system_prompt = b.profile.prompt.clone();
                        s.provider = b.profile.provider.clone();
                        s.model = b.profile.model.clone();
                        s.tools = b
                            .tools
                            .split([',', '\n'])
                            .map(|t| t.trim().to_string())
                            .filter(|t| !t.is_empty())
                            .collect();
                        ui.ctx()
                            .copy_text(serde_json::to_string_pretty(&s).unwrap_or_default());
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        close.set(true);
                    }
                });
            },
        );
        if close.get() {
            self.builder = None;
            // Saved or cancelled: nothing to continue.
            model.act(|m| async move {
                m.core().store().set_setting(offers::DRAFT_KEY, "").await?;
                m.core()
                    .emit(
                        "agent.draft",
                        "builder",
                        &serde_json::json!({ "cleared": true }),
                    )
                    .await?;
                Ok(None)
            });
        } else if let Some(b) = self.builder.as_mut() {
            // Autosave the unsaved builder (debounced).
            let now = kls_store::now_ms();
            if now - b.saved_at >= offers::AUTOSAVE_MS {
                let s = offers::Saved {
                    profile: b.profile.clone(),
                    tools: b.tools.clone(),
                    meta_json: b.spec.meta_json(),
                    at: 0,
                };
                let json = s.to_json();
                b.saved_at = now;
                if json != b.saved_json
                    && (!s.profile.name.is_empty() || !s.profile.prompt.is_empty())
                {
                    b.saved_json = json.clone();
                    model.act(move |m| async move {
                        m.core()
                            .store()
                            .set_setting(offers::DRAFT_KEY, &json)
                            .await?;
                        Ok(None)
                    });
                }
            }
        }
    }

    fn confirm_ui(&mut self, ctx: &egui::Context, model: &Model) {
        let Some((is_agent, id, name)) = self.confirm_delete.clone() else {
            return;
        };
        let mut done = false;
        egui::Window::new(icons::label(icons::TRASH, "Delete"))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Delete {} {name}?",
                    if is_agent { "agent" } else { "crew" }
                ));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if kit::danger_button(ui, &icons::label(icons::TRASH, "Delete")).clicked() {
                        model.act(move |m| async move {
                            if is_agent {
                                v::delete_agent(m.core(), &id).await?;
                            } else {
                                v::delete_crew(m.core(), &id).await?;
                            }
                            Ok(None)
                        });
                        if !is_agent {
                            self.drawer = None;
                        }
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

/// "Saved x (v3) · 2 nodes · 1 refused".
fn publish_notice(name: &str, version: i64, p: &v::Published) -> Notice {
    let mut t = format!("Saved {name} (v{version})");
    if !p.sent.is_empty() {
        t.push_str(&format!(" · {} nodes", p.sent.len()));
    }
    let refused: Vec<String> = p
        .sent
        .iter()
        .filter(|s| !s.refused.is_empty())
        .map(|s| format!("{}: {}", s.node, s.refused))
        .collect();
    if refused.is_empty() {
        Notice::Info(t)
    } else {
        Notice::Error(format!("{t} · refused {}", refused.join(", ")))
    }
}

fn new_crew() -> Crew {
    Crew {
        process: "sequential".into(),
        ..Default::default()
    }
}

/// Outline button in gold (the builder's draft action).
fn gold_outline_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let p = palette(ui.ctx());
    ui.add(
        egui::Button::new(RichText::new(text).color(p.gold_text).strong())
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0, p.gold)),
    )
}

/// Builder side checks: `(is_warning, text)`; an error blocks saving.
fn checks(p: &AgentProfile, _tools: &str, data: &Data) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let name = p.name.trim();
    if name.is_empty() {
        out.push((false, "Name required".into()));
    } else if !v::valid_name(name) {
        out.push((false, "Name: a-z 0-9 - _ : .".into()));
    } else if data.agents.iter().any(|a| a.name == name && a.id != p.id) {
        out.push((false, format!("{name} already exists")));
    }
    if p.prompt.trim().is_empty() {
        out.push((true, "No prompt: host defaults".into()));
    }
    if !p.meta_json.trim().is_empty()
        && serde_json::from_str::<serde_json::Value>(&p.meta_json).is_err()
    {
        out.push((false, "Routing: invalid JSON".into()));
    }
    if !p.avatar_svg.trim().is_empty() && v::sanitize_svg(&p.avatar_svg).is_empty() {
        out.push((false, "Avatar: not plain SVG".into()));
    }
    if let Some(pr) = data.providers.iter().find(|x| x.id == p.provider)
        && !pr.factory
    {
        out.push((true, format!("{}: factory off", pr.name)));
    }
    out
}

/// Agent profiles in the library: Install, or Update when a newer version
/// is there than the one installed.
fn library_pane(ui: &mut egui::Ui, model: &Model, data: &Data) {
    if data.library.is_empty() {
        return;
    }
    let p = palette(ui.ctx());
    ui.add_space(12.0);
    ui.label(kit::caps(
        &icons::label(icons::BOOKS, "In the library"),
        p.muted,
    ));
    for l in &data.library {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&l.item.name).strong());
            ui.label(
                RichText::new(format!("v{}", l.item.latest_version))
                    .monospace()
                    .color(p.muted),
            );
            let action = match (l.installed, l.installed_version) {
                (false, _) => Some("Install"),
                (true, Some(v)) if v < l.item.latest_version => Some("Update"),
                (true, None) => Some("Update"),
                _ => None,
            };
            match action {
                Some(label) => {
                    if kit::ghost_button(ui, &icons::label(icons::DOWNLOAD_SIMPLE, label)).clicked()
                    {
                        let id = l.item.id.clone();
                        model.act(move |m| async move {
                            let a = v::install_agent_from_library(m.core(), &id, None).await?;
                            Ok(Some(Notice::Info(format!("Installed {}", a.name))))
                        });
                    }
                }
                None => {
                    ui.label(RichText::new(icons::label(icons::CHECK, "installed")).color(p.green));
                }
            }
        });
    }
}

/// Wall mode: one large tile per agent and crew with its live workers and
/// what waits on a person, for a screen on the wall.
fn wall_ui(ui: &mut egui::Ui, model: &Model, data: &Data) {
    let p = palette(ui.ctx());
    needs_strip(ui, model, data);
    let live = |name: &str| -> usize {
        data.workers
            .iter()
            .flat_map(|w| w.agents.iter())
            .filter(|(n, _)| n == name)
            .map(|(_, c)| *c)
            .sum()
    };
    let mut names: Vec<(String, bool)> =
        data.crews.iter().map(|c| (c.name.clone(), true)).collect();
    names.extend(data.agents.iter().map(|a| (a.name.clone(), false)));
    kit::Grid::new(220.0).show(ui, names.len(), |ui, i, w| {
        let (name, is_crew) = &names[i];
        let n = if *is_crew {
            data.crews
                .iter()
                .find(|c| &c.name == name)
                .map(|c| c.members.iter().map(|m| live(&m.agent)).sum())
                .unwrap_or(0)
        } else {
            live(name)
        };
        kit::status_tile(ui, if n > 0 { p.green } else { p.line }, |ui| {
            ui.set_width(w - 24.0);
            ui.horizontal(|ui| {
                avatar_tile(ui, name, 56.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(name).size(18.0).strong());
                    ui.label(
                        RichText::new(icons::label(
                            if *is_crew {
                                icons::USERS_THREE
                            } else {
                                icons::ROBOT
                            },
                            &format!("{n} live"),
                        ))
                        .size(15.0)
                        .color(if n > 0 { p.green } else { p.muted }),
                    );
                });
            });
        });
    });
}

/// What waits on a person across the workforce: held cards, lapsed claims,
/// blocked cards and questions, each opening Checkpoints.
fn needs_strip(ui: &mut egui::Ui, model: &Model, data: &Data) {
    use kls_core::needs::CheckpointKind as K;
    let p = palette(ui.ctx());
    let kinds = [
        (K::CardHeld, icons::HAND_PALM, "Held"),
        (K::ClaimLapsed, icons::LOCK_SIMPLE, "Claims lapsed"),
        (K::CardBlocked, icons::PROHIBIT, "Blocked"),
        (K::Question, icons::CHAT_CIRCLE_DOTS, "Questions"),
    ];
    let counts: Vec<(K, &str, &str, usize)> = kinds
        .iter()
        .map(|(k, i, l)| {
            (
                *k,
                *i,
                *l,
                data.needs.iter().filter(|c| c.kind == *k).count(),
            )
        })
        .filter(|(_, _, _, n)| *n > 0)
        .collect();
    if counts.is_empty() {
        return;
    }
    kit::card(ui).inner_margin(8).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(icons::label(icons::BELL, "Needs you"))
                    .strong()
                    .color(p.gold_text),
            );
            for (k, icon, label, n) in counts {
                if kit::ghost_button(ui, &icons::label(icon, &format!("{label} {n}"))).clicked() {
                    let key = data
                        .needs
                        .iter()
                        .find(|c| c.kind == k)
                        .map(|c| c.key.clone());
                    model.go(crate::Screen::Checkpoints, key);
                }
            }
        });
    });
    ui.add_space(8.0);
}

fn workers_ui(ui: &mut egui::Ui, data: &Data) {
    if data.workers.is_empty() {
        kit::empty(ui, icons::HARD_DRIVES, "No nodes");
        return;
    }
    let p = palette(ui.ctx());
    kit::card(ui)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            TableBuilder::new(ui)
                .id_salt("workers")
                .striped(false)
                .sense(egui::Sense::click())
                .column(Column::auto().at_least(100.0))
                .column(Column::auto().at_least(160.0))
                .column(Column::auto().at_least(80.0))
                .column(Column::remainder().at_least(200.0))
                .column(Column::auto().at_least(160.0))
                .header(30.0, |mut h| {
                    for t in ["Status", "Node", "Workers", "Agents", "Providers"] {
                        h.col(|ui| {
                            kit::th(ui, t);
                            kit::header_rule(ui);
                        });
                    }
                })
                .body(|mut body| {
                    for w in &data.workers {
                        body.row(34.0, |mut row| {
                            row.col(|ui| {
                                let state = if w.online { "online" } else { "offline" };
                                kit::chip(ui, state, kit::status_color(&p, state));
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(&w.name).strong())
                                    .on_hover_text(&w.node_id);
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(w.live_workers.to_string()).monospace());
                            });
                            row.col(|ui| {
                                ui.label(
                                    w.agents
                                        .iter()
                                        .map(|(a, n)| format!("{a} ×{n}"))
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                );
                            });
                            row.col(|ui| {
                                ui.label(
                                    RichText::new(w.providers.join(", "))
                                        .monospace()
                                        .color(p.muted),
                                );
                            });
                        });
                    }
                });
        });
}

fn name_hash(name: &str) -> u32 {
    name.bytes()
        .fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32)
}

/// The tint for a name, from the palette's accents.
fn tint(p: &theme::Palette, name: &str) -> Color32 {
    // An agent's own palette (builtin profiles) themes it everywhere.
    if let Some(c) = kls_engine::catalogue::profile(name)
        .and_then(|s| s.palette.first().cloned())
        .and_then(|h| hex_color(&h))
    {
        return c;
    }
    let tints = [p.gold, p.cyan, p.violet, p.green, p.blue, p.amber];
    tints[(name_hash(name) % tints.len() as u32) as usize]
}

/// `#rrggbb` as a colour.
fn hex_color(h: &str) -> Option<Color32> {
    let h = h.trim().strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// A colour that reads on `bg`: the dark on-gold ink on bright tints, the
/// panel (or text) colour on dark ones.
fn ink(p: &theme::Palette, dark: bool, bg: Color32) -> Color32 {
    let lum = 0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32;
    if lum > 140.0 {
        p.on_gold
    } else if dark {
        p.text
    } else {
        p.panel
    }
}

/// The square agent mark: a tinted tile with the Keleustes triangle.
fn avatar_tile(ui: &mut egui::Ui, name: &str, size: f32) {
    let p = palette(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let bg = tint(&p, name);
    ui.painter().rect_filled(rect, 0.0, bg);
    let k = size / 44.0;
    let pt = |x: f32, y: f32| rect.min + Vec2::new(x * k, y * k);
    ui.painter().add(egui::Shape::closed_line(
        vec![pt(8.0, 32.0), pt(22.0, 10.0), pt(36.0, 32.0)],
        Stroke::new(2.5 * k.max(0.6), ink(&p, ui.visuals().dark_mode, bg)),
    ));
    if !name.is_empty() {
        resp.on_hover_text(name);
    }
}

/// A round initials avatar tinted from the name (SVG avatars are stored
/// and handed to nodes; the desktop draws initials).
fn avatar(ui: &mut egui::Ui, name: &str, size: f32) {
    let p = palette(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let bg = tint(&p, name);
    ui.painter().circle_filled(rect.center(), size / 2.0, bg);
    let initials: String = name
        .split(['-', '_', ':', ' ', '.'])
        .filter_map(|p| p.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase();
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        if initials.is_empty() {
            "?".into()
        } else {
            initials
        },
        egui::FontId::proportional(size * 0.42),
        ink(&p, ui.visuals().dark_mode, bg),
    );
    if !name.is_empty() {
        resp.on_hover_text(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{DARK, LIGHT};

    #[test]
    fn builtin_palette_themes_the_avatar() {
        assert_eq!(
            hex_color("#d9732a"),
            Some(Color32::from_rgb(0xd9, 0x73, 0x2a))
        );
        assert!(hex_color("nope").is_none());
        let p = LIGHT;
        assert_eq!(
            tint(&p, "belai:builder"),
            Color32::from_rgb(0xd9, 0x73, 0x2a)
        );
    }

    #[test]
    fn avatar_ink_reads_on_every_tint() {
        for (p, dark) in [(DARK, true), (LIGHT, false)] {
            for name in ["a", "bb", "test-fixer", "reviewer", "dep-triage", "x:y"] {
                let bg = tint(&p, name);
                assert_ne!(ink(&p, dark, bg), bg);
            }
        }
    }
}

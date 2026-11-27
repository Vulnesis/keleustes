//! Repos screen. Owner: M4 GitHub workstream.
//!
//! Managed orgs and repositories, bulk switches, a repository's defaults
//! and autonomy, its open pull requests with their actions, the route to
//! GitHub (local gh, a node, or why neither) and the rate budget.

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme;
use egui_extras::{Column, TableBuilder};
use kls_core::board::sync::{BACKFILL, BackfillCounts, FIELDS, MirrorRow};
use kls_engine::triggers::RepoProfile;
use kls_github::service::{ConnView, QuotaView};
use kls_github::sync::PullAction;
use kls_github::{Connection, RouteStatus};
use kls_store::github::{AUTONOMY_LEVELS, GhOrg, GhPull, GhRepo, Mirroring, RepoSettings};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

pub const READY: bool = true;

#[derive(Clone)]
struct Data {
    orgs: Vec<GhOrg>,
    repos: Vec<GhRepo>,
    open: BTreeMap<String, usize>,
    route: RouteStatus,
    quota: QuotaView,
    last_poll: Option<kls_github::poller::PollReport>,
    local: kls_github::LocalAuth,
    /// Every connection (github.com first) with its route, sign-in and budget.
    conns: Vec<ConnView>,
    /// Live workflows and the repositories their scope names.
    workflows: Vec<Attached>,
    /// Repo -> security alerts and risk signals seen in the last 90 days.
    findings: BTreeMap<String, i64>,
    /// Repo (lower case) -> what the repo scout found.
    profiles: BTreeMap<String, RepoProfile>,
}

/// A workflow and its scope (`owner/name`, `owner/*`, `*`).
#[derive(Clone)]
struct Attached {
    id: String,
    name: String,
    enabled: bool,
    system: bool,
    scope: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum SortBy {
    #[default]
    Name,
    Prs,
    Findings,
    Synced,
}

#[derive(Clone, Default)]
struct Detail {
    repo: Option<GhRepo>,
    pulls: Vec<GhPull>,
    /// "pull request", or "merge request" on GitLab.
    noun: &'static str,
}

/// Unsaved settings of the open repository.
struct Draft {
    repo: String,
    settings: RepoSettings,
    revision: i64,
    cap: String,
}

pub struct Screen {
    data: Loader<Data>,
    detail: Loader<Detail>,
    focus: Arc<Mutex<Option<String>>>,
    selected: BTreeSet<String>,
    filter: String,
    org: Option<String>,
    managed_only: bool,
    draft: Option<Draft>,
    reviewers: String,
    sort: SortBy,
    desc: bool,
    signin: super::gh_signin::SignIn,
    connect: super::gh_signin::Connect,
    /// AGENTS.md drafts for the open repository (row 54).
    agents_md: super::init_draft::Window,
    /// Board project <-> repository mirror rows (`github.issue_mirror`).
    mirrors: Loader<Mirrors>,
    mirror_draft: Option<MirrorDraft>,
}

/// The mirror rows, backfill counts per repository and the known projects.
#[derive(Clone)]
struct Mirrors {
    rows: Vec<MirrorRow>,
    counts: BTreeMap<String, BackfillCounts>,
    projects: Vec<String>,
}

/// The unsaved mirror row of the open repository.
struct MirrorDraft {
    repo: String,
    row: MirrorRow,
    hours: String,
    /// The stored row the draft started from.
    base: Option<MirrorRow>,
}

impl MirrorDraft {
    fn of(repo: &str, stored: Option<&MirrorRow>) -> Self {
        let row = stored.cloned().unwrap_or_else(|| MirrorRow {
            repo: repo.into(),
            ..Default::default()
        });
        Self {
            repo: repo.into(),
            hours: row.escalate_hours.to_string(),
            row,
            base: stored.cloned(),
        }
    }

    fn dirty(&self) -> bool {
        let base = self.base.clone().unwrap_or_else(|| MirrorRow {
            repo: self.repo.clone(),
            ..Default::default()
        });
        self.base.is_none()
            || self.row != base
            || self.hours.trim() != base.escalate_hours.to_string()
    }
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        let data = Loader::new(
            model,
            &["github.", "node.", "forge.", "workflow."],
            |core| async move {
                let svc = kls_github::service(&core);
                let mut route = svc.route().await;
                let local = svc.local_auth().await;
                // Signed in elsewhere (the CLI, a terminal): take the local route now.
                if local.signed_in() && route.route != kls_github::Route::Local {
                    route = svc.recheck_route().await;
                }
                let store = core.store();
                let mut open = BTreeMap::new();
                for p in store.gh_pulls(None, Some("open")).await? {
                    *open.entry(p.repo).or_insert(0) += 1;
                }
                let repos = store.gh_repos(None).await?;
                let mut profiles = BTreeMap::new();
                for r in &repos {
                    if let Some(s) = store.setting(&RepoProfile::key(&r.full_name)).await?
                        && let Ok(p) = serde_json::from_str::<RepoProfile>(&s)
                    {
                        profiles.insert(r.full_name.to_lowercase(), p);
                    }
                }
                Ok(Data {
                    workflows: attached_workflows(&core).await?,
                    findings: findings_by_repo(&core).await?,
                    profiles,
                    orgs: store.gh_orgs().await?,
                    repos,
                    open,
                    route,
                    quota: svc.quota().await?,
                    last_poll: svc.last_poll(),
                    local,
                    conns: svc.connection_views().await.unwrap_or_default(),
                })
            },
        );
        let focus: Arc<Mutex<Option<String>>> = Arc::default();
        let f2 = focus.clone();
        let detail = Loader::new(model, &["github."], move |core| {
            let f = f2.lock().unwrap().clone();
            async move {
                let Some(repo) = f else {
                    return Ok(Detail::default());
                };
                let store = core.store();
                Ok(Detail {
                    repo: store.gh_repo(&repo).await?,
                    pulls: store.gh_pulls(Some(&repo), Some("open")).await?,
                    noun: kls_github::service(&core)
                        .connection_of(&repo)
                        .await
                        .map(|c| c.kind.pr_noun())
                        .unwrap_or("pull request"),
                })
            }
        });
        Self {
            data,
            detail,
            focus,
            selected: BTreeSet::new(),
            filter: String::new(),
            org: None,
            managed_only: false,
            draft: None,
            reviewers: String::new(),
            sort: SortBy::Name,
            desc: false,
            signin: Default::default(),
            connect: Default::default(),
            agents_md: super::init_draft::Window::new(model),
            mirrors: Loader::new(model, &["audit.appended", "card."], |core| async move {
                let mut projects = BTreeSet::new();
                for c in core.store().cards(false).await? {
                    for l in &c.labels {
                        if let Some(p) = l.strip_prefix(kls_core::board::sync::BOARD_ONLY_PREFIX) {
                            projects.insert(p.to_string());
                        }
                    }
                }
                Ok(Mirrors {
                    rows: core.issue_mirrors().await?,
                    counts: core.backfill_counts().await?,
                    projects: projects.into_iter().collect(),
                })
            }),
            mirror_draft: None,
        }
    }

    /// Opens a repository (`owner/name`) in the detail panel.
    pub fn focus(&mut self, id: &str) {
        // keleustes://repos/signin[:refresh][:browser|:token][@<connection id>]
        if let Some(rest) = id.strip_prefix("signin") {
            let (rest, conn) = match rest.split_once('@') {
                Some((r, c)) => (r, Some(c.to_string())),
                None => (rest, None),
            };
            let parts: Vec<&str> = rest.split(':').filter(|s| !s.is_empty()).collect();
            match conn {
                Some(c) if c != kls_store::github::GITHUB_COM => {
                    self.signin.open_id(&c, parts.contains(&"refresh"))
                }
                _ => self.signin.open(parts.contains(&"refresh")),
            }
            self.signin
                .preset(parts.contains(&"token"), parts.contains(&"browser"));
            return;
        }
        // keleustes://repos/connect[:gitlab|:codeberg|…]
        if let Some(rest) = id.strip_prefix("connect") {
            self.connect.open(rest.trim_start_matches(':'));
            return;
        }
        let repo = id.split('#').next().unwrap_or(id).to_string();
        *self.focus.lock().unwrap() = Some(repo);
        self.draft = None;
        self.detail.reload();
    }

    fn open(&self) -> Option<String> {
        self.focus.lock().unwrap().clone()
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let p = theme::palette(ui.ctx());
        kit::page_header(ui, icons::GIT_BRANCH, "Repositories", |ui| {
            if theme::primary_button(ui, &icons::label(icons::ARROWS_CLOCKWISE, "Refresh orgs"))
                .clicked()
            {
                refresh_orgs(model);
            }
            if kit::ghost_button(ui, &icons::label(icons::DOWNLOAD_SIMPLE, "Poll")).clicked() {
                model.act(|m| async move {
                    let r = kls_github::service(m.core()).poll_once().await?;
                    Ok(Some(Notice::Info(match r.refused {
                        Some(why) => format!("Poll stopped: {why}"),
                        None => format!(
                            "Polled {} repositories: {} pulls, {} issues changed",
                            r.repos, r.pulls_changed, r.issues_changed
                        ),
                    })))
                });
            }
            if kit::ghost_button(ui, &icons::label(icons::PLUGS_CONNECTED, "Recheck"))
                .on_hover_text("Recheck route to GitHub")
                .clicked()
            {
                model.act(|m| async move {
                    let s = kls_github::service(m.core()).recheck_route().await;
                    Ok(Some(Notice::Info(format!("GitHub {}", s.label()))))
                });
            }
        });
        self.agents_md.ui(ui.ctx(), model);
        if self.signin.ui(ui, model) {
            self.data.reload();
        }
        if let Some(conn) = self.connect.ui(ui, model) {
            self.signin.open_conn(conn, false);
            self.data.reload();
        }
        let data = match self.data.get() {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
                return;
            }
            Some(Ok(d)) => d,
        };
        egui::ScrollArea::vertical()
            .id_salt("repos_page")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(refresh) = status_tiles(ui, &data) {
                    self.signin.open(refresh);
                }
                ui.add_space(10.0);
                match connections_strip(ui, model, &data) {
                    Some(StripAction::SignIn(c, refresh)) => self.signin.open_conn(*c, refresh),
                    Some(StripAction::Connect) => self.connect.open(""),
                    None => {}
                }
                ui.add_space(14.0);
                if self.open().is_some() {
                    kit::Split::new(560.0, 340.0, kit::Widths::Second(0.34)).show(
                        ui,
                        |ui, side| {
                            if side == 0 {
                                self.repo_table(ui, model, &data);
                            } else {
                                self.detail_panel(ui, model);
                            }
                        },
                    );
                } else {
                    self.repo_table(ui, model, &data);
                }
            });
    }

    /// The org filter: one chip per org, with its switches in a menu.
    fn orgs_bar(&mut self, ui: &mut egui::Ui, model: &Model, data: &Data) {
        let p = theme::palette(ui.ctx());
        ui.horizontal_wrapped(|ui| {
            ui.label(kit::caps(&icons::label(icons::USERS, "Orgs"), p.muted));
            if data.orgs.is_empty() {
                ui.label(egui::RichText::new("No orgs").color(p.muted));
                return;
            }
            if kit::pick_chip(ui, "All", self.org.is_none(), p.muted).clicked() {
                self.org = None;
            }
            for o in &data.orgs {
                let label = format!(
                    "{}{} {}",
                    if o.kind == "User" {
                        format!("{} ", icons::USER)
                    } else {
                        String::new()
                    },
                    o.login,
                    o.repo_count
                );
                let color = if !o.enabled {
                    p.red
                } else if o.managed {
                    p.cyan
                } else {
                    p.muted
                };
                let hover = match (o.managed, o.enabled) {
                    (_, false) => "Disabled",
                    (true, true) => "Managed",
                    (false, true) => "Not managed",
                };
                if kit::pick_chip(ui, &label, self.org.as_deref() == Some(&o.login), color)
                    .on_hover_text(hover)
                    .clicked()
                {
                    self.org = Some(o.login.clone());
                }
                ui.menu_button(icons::CARET_DOWN, |ui| {
                    let mut managed = o.managed;
                    if ui.checkbox(&mut managed, "Managed").changed() {
                        let login = o.login.clone();
                        model.act(move |m| async move {
                            kls_github::service(m.core())
                                .set_org_managed(&login, managed)
                                .await?;
                            Ok(None)
                        });
                        ui.close();
                    }
                    if ui
                        .button(icons::label(icons::ARROWS_CLOCKWISE, "Refresh"))
                        .clicked()
                    {
                        let login = o.login.clone();
                        model.act(move |m| async move {
                            let r = kls_github::service(m.core())
                                .refresh_org_repos(&login)
                                .await?;
                            Ok(Some(Notice::Info(format!(
                                "{login}: {} repositories",
                                r.repos
                            ))))
                        });
                        ui.close();
                    }
                    let toggle = if o.enabled {
                        icons::label(icons::POWER, "Disable")
                    } else {
                        icons::label(icons::POWER, "Enable")
                    };
                    if ui
                        .button(toggle)
                        .on_hover_text("A switched-off org starts no new work; data stays")
                        .clicked()
                    {
                        let login = o.login.clone();
                        let on = !o.enabled;
                        model.act(move |m| async move {
                            kls_github::service(m.core())
                                .set_org_enabled(&login, on)
                                .await?;
                            Ok(None)
                        });
                        ui.close();
                    }
                })
                .response
                .on_hover_text(format!("{} options", o.login));
            }
        });
    }

    fn repo_table(&mut self, ui: &mut egui::Ui, model: &Model, data: &Data) {
        let p = theme::palette(ui.ctx());
        kit::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Toolbar: org filter, search, bulk actions.
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    self.orgs_bar(ui, model, data);
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.filter)
                                .hint_text(icons::label(icons::FUNNEL, "owner/name"))
                                .desired_width(220.0),
                        );
                        ui.checkbox(&mut self.managed_only, "Managed");
                        if !self.selected.is_empty() {
                            ui.separator();
                            self.bulk_bar(ui, model, &data.repos, &data.workflows);
                        }
                    });
                });
            kit::hline_full(ui, p.line);
            let q = self.filter.to_lowercase();
            let rows: Vec<&GhRepo> = data
                .repos
                .iter()
                .filter(|r| self.org.as_ref().is_none_or(|o| &r.owner == o))
                .filter(|r| !self.managed_only || r.managed)
                .filter(|r| q.is_empty() || r.full_name.to_lowercase().contains(&q))
                .collect();
            let mut rows = rows;
            sort_rows(&mut rows, self.sort, self.desc, data);
            if rows.is_empty() {
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(12, 14))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(icons::label(icons::GIT_BRANCH, "No repositories"))
                                .color(p.muted),
                        )
                        .on_hover_text("Manage an org, or Refresh orgs");
                    });
                return;
            }
            let open_repo = self.open();
            let mut clicked: Option<String> = None;
            let mut lines: Vec<egui::Rect> = Vec::new();
            egui::ScrollArea::horizontal()
                .id_salt("repos_table_h")
                .show(ui, |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(8, 0))
                        .show(ui, |ui| {
                            TableBuilder::new(ui)
                                .id_salt("repos_table")
                                .striped(false)
                                .resizable(true)
                                .vscroll(false)
                                .sense(egui::Sense::click())
                                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                                .column(Column::auto())
                                .column(Column::remainder().at_least(200.0))
                                .column(Column::auto().at_least(70.0))
                                .column(Column::auto().at_least(110.0))
                                .column(Column::auto().at_least(140.0))
                                .column(Column::auto().at_least(80.0))
                                .column(Column::auto().at_least(70.0))
                                .column(Column::auto().at_least(90.0))
                                .column(Column::auto().at_least(70.0))
                                .column(Column::auto().at_least(60.0))
                                .header(34.0, |mut h| {
                                    let (r, _) = h.col(|ui| {
                                        let all = rows
                                            .iter()
                                            .all(|r| self.selected.contains(&r.full_name));
                                        let mut v = all;
                                        if ui
                                            .checkbox(&mut v, "")
                                            .on_hover_text("Select all")
                                            .changed()
                                        {
                                            for r in &rows {
                                                if v {
                                                    self.selected.insert(r.full_name.clone());
                                                } else {
                                                    self.selected.remove(&r.full_name);
                                                }
                                            }
                                        }
                                    });
                                    lines.push(r);
                                    for (t, by) in [
                                        ("Repository", Some(SortBy::Name)),
                                        ("Open PRs", Some(SortBy::Prs)),
                                        ("Autonomy", None),
                                        ("Mirroring", None),
                                        ("Managed", None),
                                        ("Synced", Some(SortBy::Synced)),
                                        ("Signals", None),
                                        ("Findings", Some(SortBy::Findings)),
                                        ("SLSA", None),
                                    ] {
                                        let (r, _) = h.col(|ui| match by {
                                            Some(b) => {
                                                let arrow = match (self.sort == b, self.desc) {
                                                    (true, true) => icons::ARROW_DOWN,
                                                    (true, false) => icons::ARROW_UP,
                                                    _ => "",
                                                };
                                                if ui
                                                    .add(
                                                        egui::Label::new(kit::caps(
                                                            &format!("{t} {arrow}"),
                                                            p.muted,
                                                        ))
                                                        .sense(egui::Sense::click()),
                                                    )
                                                    .on_hover_text("Sort")
                                                    .clicked()
                                                {
                                                    if self.sort == b {
                                                        self.desc = !self.desc;
                                                    } else {
                                                        self.sort = b;
                                                        self.desc = b != SortBy::Name;
                                                    }
                                                }
                                            }
                                            None => kit::th(ui, t),
                                        });
                                        lines.push(r);
                                    }
                                })
                                .body(|mut body| {
                                    let now = kls_store::now_ms();
                                    for r in &rows {
                                        body.row(36.0, |mut row| {
                                            row.set_selected(
                                                open_repo.as_deref() == Some(&r.full_name),
                                            );
                                            row.col(|ui| {
                                                let mut v = self.selected.contains(&r.full_name);
                                                if ui.checkbox(&mut v, "").changed() {
                                                    if v {
                                                        self.selected.insert(r.full_name.clone());
                                                    } else {
                                                        self.selected.remove(&r.full_name);
                                                    }
                                                }
                                            });
                                            row.col(|ui| {
                                                let vis = if r.visibility == "public" {
                                                    icons::GLOBE
                                                } else {
                                                    icons::LOCK_SIMPLE
                                                };
                                                ui.label(egui::RichText::new(vis).color(p.muted))
                                                    .on_hover_text(&r.visibility);
                                                let (host, name) = split_host(&r.full_name);
                                                if let Some(h) = host {
                                                    kit::chip(
                                                        ui,
                                                        &icons::label(host_icon(&data.conns, h), h),
                                                        p.muted,
                                                    );
                                                }
                                                let mut text = egui::RichText::new(name).strong();
                                                if r.archived {
                                                    text = text.color(p.muted);
                                                }
                                                ui.add(egui::Label::new(text).truncate())
                                                    .on_hover_text(&r.description);
                                                if r.archived {
                                                    ui.label(
                                                        egui::RichText::new(icons::ARCHIVE)
                                                            .color(p.muted),
                                                    )
                                                    .on_hover_text("Archived");
                                                }
                                            });
                                            row.col(|ui| {
                                                let n = data
                                                    .open
                                                    .get(&r.full_name)
                                                    .copied()
                                                    .unwrap_or(0);
                                                ui.label(
                                                    egui::RichText::new(n.to_string()).monospace(),
                                                );
                                            });
                                            row.col(|ui| {
                                                let a = &r.settings.autonomy;
                                                kit::chip(
                                                    ui,
                                                    autonomy_word(a),
                                                    autonomy_color(&p, a),
                                                );
                                            });
                                            row.col(|ui| {
                                                ui.label(
                                                    egui::RichText::new(mirroring_word(r))
                                                        .color(p.muted),
                                                );
                                            });
                                            row.col(|ui| {
                                                let (c, w) = if r.managed {
                                                    (p.green, "yes")
                                                } else {
                                                    (p.muted, "no")
                                                };
                                                kit::dot(ui, c);
                                                ui.label(egui::RichText::new(w).color(c));
                                            });
                                            row.col(|ui| {
                                                ui.label(
                                                    egui::RichText::new(if r.synced_at > 0 {
                                                        fmt::ago(now, r.synced_at)
                                                    } else {
                                                        "—".into()
                                                    })
                                                    .color(p.muted),
                                                );
                                            });
                                            row.col(|ui| signal_chips(ui, r, data, &p));
                                            row.col(|ui| {
                                                match data.findings.get(&r.full_name.to_lowercase())
                                                {
                                                    Some(n) => {
                                                        ui.label(
                                                            egui::RichText::new(n.to_string())
                                                                .monospace()
                                                                .color(p.amber),
                                                        )
                                                        .on_hover_text(
                                                            "Alerts and risk signals, 90d",
                                                        );
                                                    }
                                                    None => {
                                                        ui.label(
                                                            egui::RichText::new("-").color(p.muted),
                                                        );
                                                    }
                                                }
                                            });
                                            row.col(|ui| {
                                                let (w, hover) = slsa_word(
                                                    data.profiles.get(&r.full_name.to_lowercase()),
                                                );
                                                ui.label(
                                                    egui::RichText::new(w)
                                                        .monospace()
                                                        .color(p.muted),
                                                )
                                                .on_hover_text(hover);
                                            });
                                            let resp = row.response();
                                            lines.push(resp.rect);
                                            if resp.clicked() {
                                                clicked = Some(r.full_name.clone());
                                            }
                                        });
                                    }
                                });
                        });
                });
            kit::row_lines(ui, &lines, p.line);
            if let Some(r) = clicked {
                self.focus(&r);
            }
        });
    }

    fn bulk_bar(
        &mut self,
        ui: &mut egui::Ui,
        model: &Model,
        repos: &[GhRepo],
        workflows: &[Attached],
    ) {
        let p = theme::palette(ui.ctx());
        let sel: Vec<String> = self.selected.iter().cloned().collect();
        let n = sel.len();
        ui.label(egui::RichText::new(format!("{n} selected")).color(p.muted));
        let run = |sel: Vec<String>, managed: bool| {
            model.act(move |m| async move {
                let k = kls_github::service(m.core())
                    .set_repos_managed(&sel, managed)
                    .await?;
                Ok(Some(Notice::Info(format!(
                    "{k} repositories {}",
                    if managed {
                        "managed"
                    } else {
                        "no longer managed"
                    }
                ))))
            })
        };
        if kit::ghost_button(ui, &icons::label(icons::CHECK, "Manage")).clicked() {
            run(sel.clone(), true);
        }
        if kit::ghost_button(ui, &icons::label(icons::PROHIBIT, "Unmanage")).clicked() {
            run(sel.clone(), false);
        }
        let mirror = |sel: Vec<String>, f: fn(&mut Mirroring, bool), on: bool| {
            // Each repository keeps its other switches.
            let current: Vec<(String, Mirroring)> = repos
                .iter()
                .filter(|r| sel.contains(&r.full_name))
                .map(|r| {
                    let mut m = Mirroring {
                        pulls: r.mirror_pulls,
                        issues: r.mirror_issues,
                        labels: r.sync_labels,
                    };
                    f(&mut m, on);
                    (r.full_name.clone(), m)
                })
                .collect();
            model.act(move |m| async move {
                let svc = kls_github::service(m.core());
                for (r, mir) in current {
                    svc.set_mirroring(&[r], mir).await?;
                }
                Ok(None)
            });
        };
        for (label, f) in [
            (
                "Mirror pulls",
                (|m: &mut Mirroring, on| m.pulls = on) as fn(&mut Mirroring, bool),
            ),
            ("Mirror issues", |m: &mut Mirroring, on| m.issues = on),
            ("Keep labels", |m: &mut Mirroring, on| m.labels = on),
        ] {
            ui.menu_button(icons::label(icons::ARROWS_CLOCKWISE, label), |ui| {
                if ui.button(icons::label(icons::CHECK, "On")).clicked() {
                    mirror(sel.clone(), f, true);
                    ui.close();
                }
                if ui.button(icons::label(icons::MINUS, "Off")).clicked() {
                    mirror(sel.clone(), f, false);
                    ui.close();
                }
            });
        }
        if kit::ghost_button(ui, &icons::label(icons::TAG, "Sync labels"))
            .on_hover_text("Create sf: labels (one call per missing label; 1–20 repos)")
            .clicked()
        {
            let s = sel.clone();
            model.act(move |m| async move {
                let res = kls_github::service(m.core()).sync_labels(&s).await?;
                let created: usize = res.iter().map(|r| r.created.len()).sum();
                let failed: Vec<String> = res
                    .iter()
                    .filter(|r| !r.error.is_empty())
                    .map(|r| format!("{}: {}", r.repo, r.error))
                    .collect();
                Ok(Some(if failed.is_empty() {
                    Notice::Info(format!("Created {created} labels"))
                } else {
                    Notice::Error(failed.join("; "))
                }))
            });
        }
        let watched: Vec<(String, RepoSettings, i64)> = repos
            .iter()
            .filter(|r| sel.contains(&r.full_name) && r.settings.watched)
            .map(|r| {
                let mut s = r.settings.clone();
                s.watched = false;
                (r.full_name.clone(), s, r.revision)
            })
            .collect();
        if !watched.is_empty()
            && kit::ghost_button(ui, &icons::label(icons::EYE_SLASH, "Stop watching"))
                .on_hover_text(format!("{} watched", watched.len()))
                .clicked()
        {
            model.act(move |m| async move {
                let svc = kls_github::service(m.core());
                let n = watched.len();
                for (r, s, rev) in watched {
                    svc.save_repo_settings(&r, &s, rev).await?;
                }
                Ok(Some(Notice::Info(format!("Stopped watching {n}"))))
            });
        }
        if !workflows.is_empty() {
            ui.menu_button(icons::label(icons::FLOW_ARROW, "Attach"), |ui| {
                for w in workflows {
                    if ui
                        .button(icons::label(icons::FLOW_ARROW, &w.name))
                        .on_hover_text("Add to the workflow's scope and publish")
                        .clicked()
                    {
                        let id = w.id.clone();
                        let s = sel.clone();
                        model.act(move |m| async move {
                            let n = attach_repos(m.core(), &id, &s).await?;
                            Ok(Some(Notice::Info(format!("Attached {n}"))))
                        });
                        ui.close();
                    }
                }
            });
        }
        if kit::ghost_button(ui, &icons::label(icons::X, "Clear")).clicked() {
            self.selected.clear();
        }
    }

    fn detail_panel(&mut self, ui: &mut egui::Ui, model: &Model) {
        kit::glow(ui)
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.detail_body(ui, model);
            });
    }

    fn detail_body(&mut self, ui: &mut egui::Ui, model: &Model) {
        let p = theme::palette(ui.ctx());
        let Some(open) = self.open() else { return };
        ui.horizontal(|ui| {
            ui.label(kit::caps("Defaults", p.gold_text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if kit::ghost_button(ui, icons::X)
                    .on_hover_text("Close")
                    .clicked()
                {
                    *self.focus.lock().unwrap() = None;
                    self.draft = None;
                }
                if kit::ghost_button(ui, &icons::label(icons::FILE_TEXT, "Draft AGENTS.md"))
                    .on_hover_text("A host drafts it; you review; Accept opens a pull request")
                    .clicked()
                {
                    self.agents_md.open(&open);
                }
                if kit::ghost_button(ui, icons::ARROWS_CLOCKWISE)
                    .on_hover_text("Refresh repository and pulls")
                    .clicked()
                {
                    let r = open.clone();
                    model.act(move |m| async move {
                        kls_github::service(m.core()).refresh_repo(&r).await?;
                        Ok(None)
                    });
                }
            });
        });
        ui.label(egui::RichText::new(&open).size(20.0).strong());
        let detail = match self.detail.get() {
            Some(Ok(d)) => d,
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        let Some(repo) = detail.repo.clone() else {
            ui.spinner();
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(icons::label(
                    if repo.visibility == "public" {
                        icons::EYE
                    } else {
                        icons::LOCK_SIMPLE
                    },
                    &repo.visibility,
                ))
                .color(p.muted),
            );
            ui.label(
                egui::RichText::new(icons::label(icons::GIT_BRANCH, &repo.default_branch))
                    .color(p.muted),
            )
            .on_hover_text("Default branch");
            if !repo.can_push {
                kit::chip(ui, &icons::label(icons::LOCK_SIMPLE, "read only"), p.amber);
            }
            if !repo.html_url.is_empty() {
                ui.hyperlink_to(
                    icons::label(icons::ARROW_SQUARE_OUT, "GitHub"),
                    &repo.html_url,
                );
            }
        });
        ui.add_space(8.0);

        // Settings: defaults and autonomy.
        if self.draft.as_ref().is_none_or(|d| d.repo != open) {
            self.draft = Some(Draft {
                repo: open.clone(),
                cap: repo
                    .settings
                    .monthly_token_cap
                    .map(|c| c.to_string())
                    .unwrap_or_default(),
                settings: repo.settings.clone(),
                revision: repo.revision,
            });
        }
        let draft = self.draft.as_mut().unwrap();
        fieldset(ui, "Autonomy", |ui| {
            for a in AUTONOMY_LEVELS {
                ui.radio_value(
                    &mut draft.settings.autonomy,
                    a.to_string(),
                    autonomy_word(a),
                );
            }
        });
        ui.add_space(8.0);
        egui::Grid::new("repo_settings")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Board").color(p.muted));
                egui::ComboBox::from_id_salt("kanban_mode")
                    .selected_text(&draft.settings.kanban_mode)
                    .show_ui(ui, |ui| {
                        for k in ["off", "mirror", "all"] {
                            ui.selectable_value(&mut draft.settings.kanban_mode, k.to_string(), k);
                        }
                    });
                ui.end_row();
                ui.label(egui::RichText::new("Merge").color(p.muted));
                egui::ComboBox::from_id_salt("merge_method")
                    .selected_text(&draft.settings.merge_method)
                    .show_ui(ui, |ui| {
                        for k in ["squash", "merge", "rebase"] {
                            ui.selectable_value(&mut draft.settings.merge_method, k.to_string(), k);
                        }
                    });
                ui.end_row();
                ui.label(egui::RichText::new("Token cap").color(p.muted))
                    .on_hover_text("Monthly token cap");
                ui.add(
                    egui::TextEdit::singleline(&mut draft.cap)
                        .hint_text("none")
                        .desired_width(120.0),
                );
                ui.end_row();
            });
        ui.add_space(4.0);
        ui.checkbox(
            &mut draft.settings.protected_branches,
            icons::label(icons::LOCK_SIMPLE, "Protect branches"),
        )
        .on_hover_text("Never push to protected branches");
        if draft.settings.protected_branches {
            let branches = protected_list(&repo, &detail.pulls);
            ui.horizontal_wrapped(|ui| {
                ui.add_space(22.0);
                for b in branches {
                    kit::chip(ui, &icons::label(icons::LOCK_SIMPLE, &b), p.muted);
                }
            })
            .response
            .on_hover_text("Never pushed: default branch and pull request bases");
        }
        ui.checkbox(
            &mut draft.settings.tea_publish,
            icons::label(icons::UPLOAD_SIMPLE, "TEA releases"),
        )
        .on_hover_text("Publish releases to TEA");
        ui.checkbox(
            &mut draft.settings.watched,
            icons::label(icons::EYE, "Watched"),
        );
        let dirty = draft.settings != repo.settings
            || draft.cap
                != repo
                    .settings
                    .monthly_token_cap
                    .map(|c| c.to_string())
                    .unwrap_or_default();
        let mut discard = false;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_enabled_ui(dirty, |ui| {
                if theme::primary_button(ui, &icons::label(icons::FLOPPY_DISK, "Save defaults"))
                    .clicked()
                {
                    let cap = draft.cap.trim();
                    let cap = if cap.is_empty() {
                        Ok(None)
                    } else {
                        cap.parse::<i64>().map(Some)
                    };
                    match cap {
                        Ok(cap) => {
                            let mut s = draft.settings.clone();
                            s.monthly_token_cap = cap;
                            let rev = draft.revision;
                            let r = open.clone();
                            model.act(move |m| async move {
                                kls_github::service(m.core())
                                    .save_repo_settings(&r, &s, rev)
                                    .await?;
                                Ok(Some(Notice::Info(format!("Saved {r}"))))
                            });
                        }
                        Err(_) => {
                            model.notify(Notice::Error("Token cap must be a whole number".into()))
                        }
                    }
                }
                if kit::ghost_button(ui, &icons::label(icons::ARROW_COUNTER_CLOCKWISE, "Discard"))
                    .clicked()
                {
                    discard = true;
                }
            });
            ui.label(
                egui::RichText::new(icons::label(icons::TAG, &format!("r{}", repo.revision)))
                    .color(p.muted),
            )
            .on_hover_text("Revision");
        });
        if discard {
            self.draft = None;
        }

        // Mirroring switches (applied at once).
        ui.add_space(12.0);
        ui.label(kit::caps("Mirroring", p.muted));
        let mut m = Mirroring {
            pulls: repo.mirror_pulls,
            issues: repo.mirror_issues,
            labels: repo.sync_labels,
        };
        let mut managed = repo.managed;
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            if ui.checkbox(&mut managed, "Managed").changed() {
                let r = open.clone();
                model.act(move |mm| async move {
                    kls_github::service(mm.core())
                        .set_repos_managed(&[r], managed)
                        .await?;
                    Ok(None)
                });
            }
            changed |= ui.checkbox(&mut m.pulls, "Mirror pulls").changed();
            changed |= ui
                .checkbox(&mut m.issues, "Mirror issues")
                .on_hover_text("Mirror issues (github.issue.* events feed the board)")
                .changed();
            changed |= ui
                .checkbox(&mut m.labels, "Keep labels")
                .on_hover_text("Keep sf: labels")
                .changed();
        });
        if changed {
            let r = open.clone();
            model.act(move |mm| async move {
                kls_github::service(mm.core())
                    .set_mirroring(&[r], m)
                    .await?;
                Ok(None)
            });
        }

        // The board project mirrored to this repository's issues.
        ui.add_space(12.0);
        self.mirror_section(ui, model, &open);

        // Attached workflows.
        ui.add_space(12.0);
        let attached: Vec<Attached> = self
            .data
            .get()
            .and_then(|d| d.ok())
            .map(|d| d.workflows)
            .unwrap_or_default()
            .into_iter()
            .filter(|w| kls_engine::ops::attached_to(&w.scope, &open))
            .collect();
        ui.label(kit::caps(
            &icons::label(icons::FLOW_ARROW, &format!("Workflows {}", attached.len())),
            p.muted,
        ));
        if attached.is_empty() {
            ui.label(
                egui::RichText::new(icons::label(icons::MINUS, "None attached")).color(p.muted),
            );
        }
        ui.horizontal_wrapped(|ui| {
            for w in &attached {
                let c = if w.enabled { p.cyan } else { p.muted };
                let icon = if w.system {
                    icons::SPARKLE
                } else {
                    icons::FLOW_ARROW
                };
                kit::chip(ui, &icons::label(icon, &w.name), c).on_hover_text(format!(
                    "{} · {}",
                    if w.enabled { "on" } else { "off" },
                    if w.scope.is_empty() {
                        "any repository".to_string()
                    } else {
                        w.scope.join(", ")
                    }
                ));
            }
        });

        // Pull requests.
        ui.add_space(12.0);
        ui.label(kit::caps(
            &icons::label(
                icons::GIT_PULL_REQUEST,
                &format!("{} {}", plural_title(detail.noun), detail.pulls.len()),
            ),
            p.muted,
        ));
        ui.horizontal(|ui| {
            ui.label(icons::USERS).on_hover_text("Reviewers");
            ui.add(
                egui::TextEdit::singleline(&mut self.reviewers)
                    .hint_text("reviewers: ana, acme/ops")
                    .desired_width(ui.available_width().min(240.0)),
            );
        });
        if detail.pulls.is_empty() {
            ui.label(
                egui::RichText::new(if repo.mirror_pulls {
                    icons::label(
                        icons::CHECK,
                        &format!("No open {}s", noun_or_default(detail.noun)),
                    )
                } else {
                    icons::label(icons::MINUS, "Mirroring off")
                })
                .color(p.muted),
            );
        }
        let reviewers: Vec<String> = self
            .reviewers
            .split([',', ' '])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        for pr in &detail.pulls {
            ui.add_space(6.0);
            kit::card(ui)
                .fill(p.panel2)
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    pull_card(
                        ui,
                        model,
                        pr,
                        &repo,
                        &reviewers,
                        noun_or_default(detail.noun),
                    );
                });
        }
    }

    /// The board project <-> repository mirror row of `open`: project,
    /// inbound list, outbound, merge rules, escalation and what happens to
    /// the cards already on the board. Saved as one audited write.
    fn mirror_section(&mut self, ui: &mut egui::Ui, model: &Model, open: &str) {
        let p = theme::palette(ui.ctx());
        ui.label(kit::caps(
            &icons::label(icons::KANBAN, "Board project"),
            p.muted,
        ));
        let m = match self.mirrors.get() {
            Some(Ok(m)) => m,
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        let stored = m.rows.iter().find(|r| r.repo == open).cloned();
        let stale = self.mirror_draft.as_ref().is_none_or(|d| {
            d.repo != open
                || (!d.dirty() && d.base != stored)
                || (d.base.is_none() && stored.is_some())
        });
        if stale {
            self.mirror_draft = Some(MirrorDraft::of(open, stored.as_ref()));
        }
        let d = self.mirror_draft.as_mut().unwrap();
        let counts = m.counts.get(open).copied();
        egui::Grid::new("repo_mirror")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Project").color(p.muted));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut d.row.project)
                            .hint_text("project")
                            .desired_width(140.0),
                    );
                    if !m.projects.is_empty() {
                        ui.menu_button(icons::CARET_DOWN, |ui| {
                            for name in &m.projects {
                                if ui.button(icons::label(icons::TAG, name)).clicked() {
                                    d.row.project = name.clone();
                                    ui.close();
                                }
                            }
                        })
                        .response
                        .on_hover_text("Board projects");
                    }
                });
                ui.end_row();
                ui.label(egui::RichText::new("Inbound").color(p.muted))
                    .on_hover_text("New issues land in");
                egui::ComboBox::from_id_salt("mirror_inbound")
                    .selected_text(inbound_word(&d.row.inbound))
                    .show_ui(ui, |ui| {
                        for k in ["triage", "review", "off"] {
                            ui.selectable_value(&mut d.row.inbound, k.to_string(), inbound_word(k));
                        }
                    });
                ui.end_row();
                ui.label(egui::RichText::new("Outbound").color(p.muted));
                ui.checkbox(
                    &mut d.row.outbound,
                    icons::label(icons::UPLOAD_SIMPLE, "Push card edits"),
                );
                ui.end_row();
                for f in FIELDS {
                    ui.label(egui::RichText::new(icons::label(icons::GIT_MERGE, f)).color(p.muted))
                        .on_hover_text("When both sides changed");
                    let mut v = d.row.rules.get(f).cloned().unwrap_or_else(|| "ask".into());
                    egui::ComboBox::from_id_salt(("mirror_rule", f))
                        .selected_text(rule_word(&v))
                        .show_ui(ui, |ui| {
                            for k in ["ask", "board", "github"] {
                                ui.selectable_value(&mut v, k.to_string(), rule_word(k));
                            }
                        });
                    if v == "ask" {
                        d.row.rules.remove(f);
                    } else {
                        d.row.rules.insert(f.to_string(), v);
                    }
                    ui.end_row();
                }
                ui.label(egui::RichText::new("Escalate").color(p.muted))
                    .on_hover_text("Hours before an open conflict goes to owners; 0 never");
                ui.add(
                    egui::TextEdit::singleline(&mut d.hours)
                        .hint_text("hours")
                        .desired_width(64.0),
                );
                ui.end_row();
            });
        ui.add_space(4.0);
        fieldset(ui, "Existing cards", |ui| {
            ui.horizontal_wrapped(|ui| {
                for b in BACKFILL {
                    ui.radio_value(&mut d.row.backfill, b.to_string(), backfill_word(b, counts));
                }
            });
            match &stored {
                Some(s) if s.backfilled_at > 0 && s.backfill == d.row.backfill => {
                    kit::chip(
                        ui,
                        &icons::label(
                            icons::CHECK,
                            &format!("Opened · {}", clock(s.backfilled_at)),
                        ),
                        p.green,
                    );
                }
                Some(s) if s.backfill_due() && s.backfill == d.row.backfill => {
                    kit::chip(ui, &icons::label(icons::HOURGLASS, "Next poll"), p.amber)
                        .on_hover_text("Opens issues for these cards on the next poll");
                }
                _ => {}
            }
        });
        let dirty = d.dirty();
        let ready = dirty && !d.row.project.trim().is_empty();
        let mut reset = false;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let save = ui
                .add_enabled_ui(ready, |ui| {
                    theme::primary_button(ui, &icons::label(icons::FLOPPY_DISK, "Save mirror"))
                })
                .inner
                .on_disabled_hover_text(if dirty {
                    "Name a project"
                } else {
                    "No changes"
                });
            if save.clicked() {
                match d.hours.trim().parse::<i64>() {
                    Ok(h) if h >= 0 => {
                        let mut row = d.row.clone();
                        row.project = row.project.trim().to_string();
                        row.repo = open.to_string();
                        row.escalate_hours = h;
                        let rows = rows_with(&m.rows, open, Some(row));
                        let r = open.to_string();
                        model.act(move |mm| async move {
                            mm.core()
                                .save_issue_mirrors(&rows, kls_core::board::OWNER)
                                .await?;
                            Ok(Some(Notice::Info(format!("Mirror saved for {r}"))))
                        });
                    }
                    _ => model.notify(Notice::Error("Escalate takes whole hours".into())),
                }
            }
            if stored.is_some()
                && kit::ghost_button(ui, &icons::label(icons::STOP, "Stop mirroring")).clicked()
            {
                let rows = rows_with(&m.rows, open, None);
                let r = open.to_string();
                model.act(move |mm| async move {
                    mm.core()
                        .save_issue_mirrors(&rows, kls_core::board::OWNER)
                        .await?;
                    Ok(Some(Notice::Info(format!("Stopped mirroring {r}"))))
                });
                reset = true;
            }
            if dirty
                && stored.is_some()
                && kit::ghost_button(ui, &icons::label(icons::ARROW_COUNTER_CLOCKWISE, "Discard"))
                    .clicked()
            {
                reset = true;
            }
        });
        if reset {
            self.mirror_draft = None;
        }
    }
}

/// `rows` with `repo`'s row replaced by `row` (or dropped for `None`).
fn rows_with(rows: &[MirrorRow], repo: &str, row: Option<MirrorRow>) -> Vec<MirrorRow> {
    let mut out: Vec<MirrorRow> = rows.iter().filter(|r| r.repo != repo).cloned().collect();
    out.extend(row);
    out
}

fn inbound_word(k: &str) -> String {
    match k {
        "review" => icons::label(icons::EYE, "Review"),
        "off" => icons::label(icons::PROHIBIT, "Off"),
        _ => icons::label(icons::TRAY, "Triage"),
    }
}

fn rule_word(k: &str) -> &'static str {
    match k {
        "board" => "Board wins",
        "github" => "GitHub wins",
        _ => "Ask",
    }
}

/// The existing-cards choice with its count (web `backfillTitles`).
fn backfill_word(b: &str, counts: Option<BackfillCounts>) -> String {
    match (b, counts) {
        ("recent", Some(c)) => format!("Recent 30 days ({})", c.recent),
        ("recent", None) => "Recent 30 days".into(),
        ("all", Some(c)) => format!("All ({})", c.all),
        ("all", None) => "All".into(),
        _ => "None".into(),
    }
}

/// Live (published or draft) workflows with their scope.
async fn attached_workflows(core: &kls_core::Core) -> anyhow::Result<Vec<Attached>> {
    let s = core.store();
    let mut out = Vec::new();
    for w in s.workflows(false).await? {
        let Some(v) = kls_engine::ops::live_version(&w) else {
            continue;
        };
        let Some(ver) = s.workflow_version(&w.id, v).await? else {
            continue;
        };
        let g = kls_engine::graph::Graph::parse(&ver.graph_json).unwrap_or_default();
        out.push(Attached {
            id: w.id,
            name: w.name,
            enabled: w.enabled,
            system: w.origin == "system",
            scope: g.scope.repos,
        });
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    Ok(out)
}

/// Security alerts and risk signals per repository (lower case), last 90 days.
async fn findings_by_repo(core: &kls_core::Core) -> anyhow::Result<BTreeMap<String, i64>> {
    use kls_core::github::kinds;
    let evs = core
        .store()
        .events_query(&kls_store::audit::EventQuery {
            prefixes: vec![kinds::ALERT.into(), kinds::RISK.into()],
            from_at: Some(kls_store::now_ms() - 90 * 86_400_000),
            limit: 5000,
            ..Default::default()
        })
        .await?;
    let mut out = BTreeMap::new();
    for e in evs {
        let d: serde_json::Value = serde_json::from_str(&e.data_json).unwrap_or_default();
        if let Some(r) = d.get("repo").and_then(|v| v.as_str())
            && !r.is_empty()
        {
            *out.entry(r.to_lowercase()).or_insert(0) += 1;
        }
    }
    Ok(out)
}

fn sort_rows(rows: &mut [&GhRepo], by: SortBy, desc: bool, data: &Data) {
    let findings = |r: &GhRepo| {
        data.findings
            .get(&r.full_name.to_lowercase())
            .copied()
            .unwrap_or(-1)
    };
    rows.sort_by(|a, b| {
        let o = match by {
            SortBy::Name => a.full_name.to_lowercase().cmp(&b.full_name.to_lowercase()),
            SortBy::Prs => data
                .open
                .get(&a.full_name)
                .unwrap_or(&0)
                .cmp(data.open.get(&b.full_name).unwrap_or(&0)),
            SortBy::Findings => findings(a).cmp(&findings(b)),
            SortBy::Synced => a.synced_at.cmp(&b.synced_at),
        };
        let o = if desc { o.reverse() } else { o };
        o.then(a.full_name.cmp(&b.full_name))
    });
}

/// Opt-in signals: watched, board bound, TEA, profile signals.
fn signal_chips(ui: &mut egui::Ui, r: &GhRepo, data: &Data, p: &theme::Palette) {
    let s = &r.settings;
    let mut any = false;
    if s.watched {
        ui.label(egui::RichText::new(icons::EYE).color(p.cyan))
            .on_hover_text("Watched");
        any = true;
    }
    if s.kanban_mode != "off" {
        ui.label(egui::RichText::new(icons::KANBAN).color(p.cyan))
            .on_hover_text(format!("Board: {}", s.kanban_mode));
        any = true;
    }
    if s.tea_publish {
        ui.label(egui::RichText::new(icons::UPLOAD_SIMPLE).color(p.cyan))
            .on_hover_text("TEA releases");
        any = true;
    }
    if let Some(prof) = data.profiles.get(&r.full_name.to_lowercase())
        && !prof.signals.is_empty()
    {
        ui.label(
            egui::RichText::new(icons::label(
                icons::SPARKLE,
                &prof.signals.len().to_string(),
            ))
            .color(p.muted),
        )
        .on_hover_text(prof.signals.join(", "));
        any = true;
    }
    if !any {
        ui.label(egui::RichText::new("-").color(p.muted));
    }
}

/// The SLSA column from the scout's profile: signed releases (sigstore) or
/// release artifacts without signing; "-" when the repo was never scouted.
fn slsa_word(p: Option<&RepoProfile>) -> (&'static str, &'static str) {
    match p {
        None => ("-", "Not scouted"),
        Some(p) if p.signals.iter().any(|s| s == "sigstore") => ("signed", "Sigstore provenance"),
        Some(p) if p.signals.iter().any(|s| s == "release-artifacts") => {
            ("unsigned", "Release artifacts, no provenance")
        }
        Some(_) => ("none", "No release provenance"),
    }
}

/// Branches the factory never pushes to: the default branch and every open
/// pull request's base.
fn protected_list(repo: &GhRepo, pulls: &[GhPull]) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    if !repo.default_branch.is_empty() {
        out.insert(repo.default_branch.clone());
    }
    for p in pulls {
        if !p.base_ref.is_empty() {
            out.insert(p.base_ref.clone());
        }
    }
    out.into_iter().collect()
}

/// Adds `repos` to a workflow's scope: saves the draft, and publishes it when
/// the workflow was live with no draft of its own. Audited with the owner as
/// the actor. Returns how many were added (a workflow with no scope already
/// runs on every repository).
async fn attach_repos(core: &kls_core::Core, id: &str, repos: &[String]) -> anyhow::Result<usize> {
    use anyhow::Context;
    let s = core.store();
    let w = s.workflow(id).await?.context("no such workflow")?;
    let had_draft = w.draft_version > 0;
    let v = if had_draft {
        w.draft_version
    } else {
        w.published_version
    };
    let ver = s
        .workflow_version(&w.id, v)
        .await?
        .context("the workflow has no version")?;
    let mut g = kls_engine::graph::Graph::parse(&ver.graph_json)?;
    if g.scope.repos.is_empty() {
        return Ok(0);
    }
    let mut added = Vec::new();
    for r in repos {
        if !kls_engine::ops::repo_in_scope(&g.scope.repos, r) {
            g.scope.repos.push(r.clone());
            added.push(r.clone());
        }
    }
    if added.is_empty() {
        return Ok(0);
    }
    let saved =
        kls_engine::ops::save_draft(core, &w.id, if had_draft { ver.revision } else { 0 }, &g)
            .await?;
    let published = !had_draft && w.published_version > 0;
    if published {
        kls_engine::ops::publish(core, &w.id, saved.version, "attached repositories").await?;
    }
    core.audit_record(kls_store::audit::LedgerDraft {
        kind: "workflow".into(),
        actor: "owner".into(),
        actor_kind: "user".into(),
        msg: format!("workflow attach {} {}", w.name, added.join(", ")),
        subject: serde_json::json!({ "action": "workflow.attach", "workflowId": w.id }),
        payload: serde_json::json!({ "repos": added, "version": saved.version,
                                     "published": published, "origin": "repos screen" }),
        origin: "factory".into(),
        ..Default::default()
    })
    .await?;
    Ok(added.len())
}

fn refresh_orgs(model: &Model) {
    model.act(|m| async move {
        let r = kls_github::service(m.core()).refresh_orgs().await?;
        Ok(Some(Notice::Info(format!(
            "{} orgs, {} repositories of managed orgs ({} new)",
            r.orgs,
            r.repos,
            r.added.len()
        ))))
    });
}

/// The connection's word for a pull request (before the detail loads: "pull request").
fn noun_or_default(noun: &'static str) -> &'static str {
    if noun.is_empty() {
        "pull request"
    } else {
        noun
    }
}

/// "Pull requests" / "Merge requests".
fn plural_title(noun: &'static str) -> String {
    let n = noun_or_default(noun);
    let mut c = n.chars();
    match c.next() {
        Some(f) => format!("{}{}s", f.to_uppercase(), c.as_str()),
        None => String::new(),
    }
}

/// One open pull request in the detail panel: state chips and actions.
fn pull_card(
    ui: &mut egui::Ui,
    model: &Model,
    pr: &GhPull,
    repo: &GhRepo,
    reviewers: &[String],
    noun: &str,
) {
    let p = theme::palette(ui.ctx());
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(format!("#{}", pr.number))
                .monospace()
                .color(p.muted),
        );
        ui.label(egui::RichText::new(&pr.title).strong());
    });
    ui.horizontal_wrapped(|ui| {
        if pr.draft {
            kit::chip(ui, &icons::label(icons::PENCIL_SIMPLE, "draft"), p.muted);
        }
        let (c, g, t) = match pr.mergeable.as_str() {
            "clean" => (p.green, icons::CHECK, "mergeable"),
            "conflict" => (p.red, icons::X, "conflicts"),
            _ => (p.muted, icons::QUESTION, "unknown"),
        };
        kit::chip(ui, &icons::label(g, t), c).on_hover_text("Mergeability");
        let (c, g, t) = match pr.checks_state.as_str() {
            "success" => (
                p.green,
                icons::CHECK,
                format!("{}/{}", pr.checks.success, pr.checks.total),
            ),
            "failure" => (p.red, icons::X, pr.checks.failing.join(", ")),
            "pending" => (
                p.amber,
                icons::CLOCK,
                format!("{} pending", pr.checks.pending),
            ),
            _ => (p.muted, icons::MINUS, "none".into()),
        };
        kit::chip(ui, &format!("{g} {} {t}", icons::LIST_CHECKS), c).on_hover_text("Checks");
        let (c, g, t) = match pr.review_decision.as_str() {
            "APPROVED" => (p.green, icons::CHECK, "approved"),
            "CHANGES_REQUESTED" => (p.red, icons::WARNING, "changes"),
            _ => (p.muted, icons::CLOCK, "review"),
        };
        kit::chip(ui, &icons::label(g, t), c).on_hover_text("Review");
    });
    ui.label(
        egui::RichText::new(format!(
            "{} → {} · @{}",
            pr.head_ref, pr.base_ref, pr.author
        ))
        .monospace()
        .size(12.0)
        .color(p.muted),
    );
    ui.horizontal_wrapped(|ui| {
        let act = |a: PullAction| {
            let r = pr.repo.clone();
            let n = pr.number;
            let name = a.name();
            model.act(move |m| async move {
                kls_github::service(m.core()).pull_action(&r, n, a).await?;
                Ok(Some(Notice::Info(format!("{r}#{n}: {name} done"))))
            });
        };
        if pr.draft {
            if kit::ghost_button(ui, &icons::label(icons::CHECK, "Ready"))
                .on_hover_text("Ready for review")
                .clicked()
            {
                act(PullAction::Ready);
            }
        } else if kit::ghost_button(ui, &icons::label(icons::PENCIL_SIMPLE, "Draft"))
            .on_hover_text("Convert to draft")
            .clicked()
        {
            act(PullAction::Draft);
        }
        if kit::ghost_button(ui, &icons::label(icons::ARROWS_CLOCKWISE, "Update"))
            .on_hover_text("Update branch; GitHub makes and signs the merge")
            .clicked()
        {
            act(PullAction::UpdateBranch);
        }
        if ui
            .add_enabled_ui(!reviewers.is_empty(), |ui| {
                kit::ghost_button(ui, &icons::label(icons::USER_PLUS, "Request"))
            })
            .inner
            .on_hover_text("Request reviewers")
            .clicked()
        {
            act(PullAction::RequestReviewers(reviewers.to_vec()));
        }
        if ui
            .add_enabled_ui(pr.mergeable != "conflict" && !pr.draft, |ui| {
                kit::ghost_button(ui, &icons::label(icons::GIT_MERGE, "Merge"))
            })
            .inner
            .on_hover_text(format!("Merge ({})", repo.settings.merge_method))
            .clicked()
        {
            act(PullAction::Merge(repo.settings.merge_method.clone()));
        }
        if pr.mergeable == "conflict"
            && theme::primary_button(ui, &icons::label(icons::GIT_MERGE, "Resolve")).clicked()
        {
            model.go(
                crate::Screen::Conflicts,
                Some(format!("{}#{}", pr.repo, pr.number)),
            );
        }
        if kit::ghost_button(ui, icons::ARROWS_CLOCKWISE)
            .on_hover_text(format!("Refresh {noun}"))
            .clicked()
        {
            let r = pr.repo.clone();
            let n = pr.number;
            model.act(move |m| async move {
                kls_github::service(m.core()).refresh_pull(&r, n).await?;
                Ok(None)
            });
        }
        if !pr.html_url.is_empty() {
            ui.hyperlink_to(icons::ARROW_SQUARE_OUT, &pr.html_url)
                .on_hover_text("Open on GitHub");
        }
    });
}

/// The three status tiles: route to GitHub, rate budget, polling.
/// `Some(refresh)` when "Sign in" (false) or "Grant scopes" (true) was clicked.
fn status_tiles(ui: &mut egui::Ui, data: &Data) -> Option<bool> {
    let p = theme::palette(ui.ctx());
    let now = kls_store::now_ms();
    let sign_in = std::cell::Cell::new(None);
    kit::Grid::new(260.0).tall().show(ui, 3, |ui, i, _| match i {
        0 => {
            let ok = data.route.route.available();
            let edge = if ok { p.gold_edge } else { p.red };
            kit::status_tile(ui, edge, |ui| {
                ui.label(kit::caps(
                    &icons::label(icons::GITHUB_LOGO, "GitHub route"),
                    p.muted,
                ));
                ui.horizontal(|ui| {
                    kit::dot(ui, if ok { p.green } else { p.red });
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(data.route.label()).size(18.0).strong(),
                        )
                        .truncate(),
                    )
                    .on_hover_text(data.route.label());
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "checked {}",
                            fmt::ago(now, data.route.checked_at)
                        ))
                        .size(12.0)
                        .color(p.muted),
                    );
                    match &data.local {
                        kls_github::LocalAuth::SignedIn { login, missing } => {
                            if !login.is_empty() {
                                kit::chip(ui, &icons::label(icons::USER, login), p.muted);
                            }
                            if !missing.is_empty()
                                && kit::ghost_button(
                                    ui,
                                    &icons::label(
                                        icons::KEY,
                                        &format!("Grant scopes · {}", missing.len()),
                                    ),
                                )
                                .on_hover_text(missing.join("\n"))
                                .clicked()
                            {
                                sign_in.set(Some(true));
                            }
                        }
                        kls_github::LocalAuth::SignedOut(why) => {
                            if theme::primary_button(
                                ui,
                                &icons::label(icons::KEY, "Sign in"),
                            )
                            .on_hover_text(why)
                            .clicked()
                            {
                                sign_in.set(Some(false));
                            }
                        }
                        kls_github::LocalAuth::Missing(why) => {
                            ui.hyperlink_to(
                                icons::label(icons::DOWNLOAD_SIMPLE, "Install gh"),
                                "https://cli.github.com",
                            )
                            .on_hover_text(why);
                        }
                    }
                });
            });
        }
        1 => kit::status_tile(ui, p.line, |ui| {
            ui.label(kit::caps(
                &icons::label(icons::GAUGE, "Rate budget"),
                p.muted,
            ));
            match &data.quota.core {
                Some(q) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new(q.remaining.to_string())
                                .size(18.0)
                                .strong(),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "of {} left · resets {}",
                                q.limit,
                                clock(q.reset_at)
                            ))
                            .size(13.0)
                            .color(p.muted),
                        );
                    })
                    .response
                    .on_hover_text(format!("Resets {}", fmt::when(q.reset_at)));
                    let frac = if q.limit > 0 {
                        q.remaining as f32 / q.limit as f32
                    } else {
                        0.0
                    };
                    kit::meter(ui, frac, p.cyan);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new(icons::label(
                                icons::ROBOT,
                                &format!(
                                    "{}/{} /h",
                                    data.quota.factory_used, data.quota.factory_limit
                                ),
                            ))
                            .size(12.0)
                            .color(p.muted),
                        )
                        .on_hover_text(format!(
                            "Poller calls this hour. Factory share {}%, reserve {}% ({} calls kept for people). 304 answers are free.",
                            data.quota.budget.map(|b| b.0).unwrap_or(0),
                            data.quota.budget.map(|b| b.1).unwrap_or(0),
                            data.quota.reserve
                        ));
                        if q.secondary_until > now {
                            ui.label(
                                egui::RichText::new(icons::label(
                                    icons::PAUSE,
                                    &format!("Backoff until {}", clock(q.secondary_until)),
                                ))
                                .size(12.0)
                                .color(p.amber),
                            );
                        }
                    });
                }
                None => {
                    ui.label(egui::RichText::new("—").size(18.0).strong())
                        .on_hover_text("Rate limit not read yet");
                    kit::meter(ui, 0.0, p.cyan);
                }
            }
        }),
        _ => kit::status_tile(ui, p.line, |ui| {
            ui.label(kit::caps(&icons::label(icons::CLOCK, "Polling"), p.muted));
            match &data.last_poll {
                Some(r) => {
                    ui.label(egui::RichText::new(fmt::ago(now, r.at)).size(18.0).strong())
                        .on_hover_text(format!("Last poll {}", fmt::when(r.at)));
                    match &r.refused {
                        Some(why) => {
                            ui.label(
                                egui::RichText::new(icons::label(icons::WARNING, why))
                                    .size(12.0)
                                    .color(p.amber),
                            );
                        }
                        None => {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} repos · {} pulls · {} issues changed",
                                    r.repos, r.pulls_changed, r.issues_changed
                                ))
                                .size(12.0)
                                .color(p.muted),
                            );
                        }
                    }
                }
                None => {
                    ui.label(egui::RichText::new("—").size(18.0).strong())
                        .on_hover_text("No poll yet");
                }
            }
            let calls: i64 = data.quota.calls.iter().map(|c| c.calls).sum();
            let not_modified: i64 = data.quota.calls.iter().map(|c| c.not_modified).sum();
            if calls > 0 {
                ui.label(
                    egui::RichText::new(format!(
                        "304 rate {:.0}%",
                        not_modified as f64 * 100.0 / calls as f64
                    ))
                    .size(12.0)
                    .color(p.muted),
                )
                .on_hover_text("Answers served from the ETag cache");
            }
        }),
    });
    sign_in.get()
}

fn autonomy_word(a: &str) -> &'static str {
    match a {
        "observe" => "Observe only",
        "suggest" => "Suggest",
        "draft_pr" => "Draft PR",
        "pr" => "Pull request",
        "merge" => "Merge",
        _ => "Unknown",
    }
}

fn autonomy_color(p: &theme::Palette, a: &str) -> egui::Color32 {
    match a {
        "draft_pr" => p.cyan,
        "pr" => p.violet,
        "merge" => p.gold_text,
        _ => p.muted,
    }
}

fn mirroring_word(r: &GhRepo) -> String {
    let on: Vec<&str> = [
        (r.mirror_pulls, "pulls"),
        (r.mirror_issues, "issues"),
        (r.sync_labels, "labels"),
    ]
    .into_iter()
    .filter_map(|(on, w)| on.then_some(w))
    .collect();
    if on.is_empty() {
        "off".into()
    } else {
        on.join(" · ")
    }
}

/// "HH:MM" local time.
fn clock(ms: i64) -> String {
    match jiff::Timestamp::from_millisecond(ms) {
        Ok(ts) => ts
            .to_zoned(jiff::tz::TimeZone::system())
            .strftime("%H:%M")
            .to_string(),
        Err(_) => "?".into(),
    }
}

/// A legend-titled bordered group (the design's fieldset).
fn fieldset(ui: &mut egui::Ui, legend: &str, body: impl FnOnce(&mut egui::Ui)) {
    let p = theme::palette(ui.ctx());
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, p.line))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(legend).size(12.0).color(p.muted));
            body(ui);
        });
}

/// `codeberg.org/ana/app` -> (`codeberg.org`, `ana/app`); github.com keys have no host.
fn split_host(key: &str) -> (Option<&str>, &str) {
    match key.split_once('/') {
        Some((h, rest)) if h.contains('.') && rest.contains('/') => (Some(h), rest),
        _ => (None, key),
    }
}

/// The icon of a forge kind (Codeberg has its own).
pub(crate) fn kind_icon(kind: kls_github::ForgeKind, host: &str) -> &'static str {
    use kls_github::ForgeKind as K;
    match kind {
        K::Github => icons::GITHUB_LOGO,
        K::Ghes => icons::BUILDINGS,
        K::Gitlab => icons::GITLAB_LOGO,
        K::Gitea => icons::COFFEE,
        K::Forgejo if host == "codeberg.org" => icons::MOUNTAINS,
        K::Forgejo => icons::GIT_FORK,
    }
}

fn host_icon(conns: &[ConnView], host: &str) -> &'static str {
    conns
        .iter()
        .find(|c| c.conn.host == host)
        .map(|c| kind_icon(c.conn.kind, host))
        .unwrap_or(icons::GIT_BRANCH)
}

enum StripAction {
    SignIn(Box<Connection>, bool),
    Connect,
}

/// One row per connection off github.com (github.com has its route tile),
/// and the Connect button.
fn connections_strip(ui: &mut egui::Ui, model: &Model, data: &Data) -> Option<StripAction> {
    let p = theme::palette(ui.ctx());
    let mut out = None;
    kit::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(kit::caps(
                        &icons::label(icons::PLUGS_CONNECTED, "Forges"),
                        p.muted,
                    ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if kit::ghost_button(ui, &icons::label(icons::PLUS, "Connect forge"))
                            .clicked()
                        {
                            out = Some(StripAction::Connect);
                        }
                    });
                });
                for v in data.conns.iter().filter(|v| !v.conn.is_github_com()) {
                    ui.add_space(4.0);
                    kit::rule(ui);
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let ok = v.route.route.available() && v.conn.enabled;
                        kit::dot(
                            ui,
                            if !v.conn.enabled {
                                p.muted
                            } else if ok {
                                p.green
                            } else {
                                p.red
                            },
                        );
                        ui.label(
                            egui::RichText::new(icons::label(
                                kind_icon(v.conn.kind, &v.conn.host),
                                v.conn.name(),
                            ))
                            .strong(),
                        )
                        .on_hover_text(format!(
                            "{} · {}",
                            v.conn.kind.label(),
                            v.conn.host
                        ));
                        match &v.auth {
                            kls_github::forge::auth::AuthState::SignedIn {
                                login,
                                missing,
                                expires_at,
                            } => {
                                if !login.is_empty() {
                                    kit::chip(ui, &icons::label(icons::USER, login), p.muted);
                                }
                                if !missing.is_empty()
                                    && kit::ghost_button(
                                        ui,
                                        &icons::label(
                                            icons::KEY,
                                            &format!("Scopes · {}", missing.len()),
                                        ),
                                    )
                                    .on_hover_text(missing.join("\n"))
                                    .clicked()
                                {
                                    out = Some(StripAction::SignIn(Box::new(v.conn.clone()), true));
                                }
                                if let Some(exp) = expires_at {
                                    let soon = *exp - kls_store::now_ms() < 14 * 86_400_000;
                                    kit::chip(
                                        ui,
                                        &icons::label(icons::TIMER, &fmt::when(*exp)),
                                        if soon { p.amber } else { p.muted },
                                    )
                                    .on_hover_text("Token expires");
                                }
                            }
                            kls_github::forge::auth::AuthState::Missing(why) => {
                                ui.hyperlink_to(
                                    icons::label(
                                        icons::DOWNLOAD_SIMPLE,
                                        &format!("Install {}", v.conn.cli().name()),
                                    ),
                                    v.conn.cli().docs_url(),
                                )
                                .on_hover_text(why);
                            }
                            other => {
                                ui.label(
                                    egui::RichText::new(icons::label(
                                        icons::X_CIRCLE,
                                        "Signed out",
                                    ))
                                    .color(p.red),
                                )
                                .on_hover_text(other.why());
                            }
                        }
                        ui.label(
                            egui::RichText::new(v.route.label())
                                .size(12.0)
                                .color(p.muted),
                        )
                        .on_hover_text(v.route.label());
                        if let Some(q) = &v.quota.core {
                            let txt = if v.quota.budget_known {
                                format!("{}/{}", q.remaining, q.limit)
                            } else {
                                format!("{} calls/h", q.used)
                            };
                            ui.label(
                                egui::RichText::new(icons::label(icons::GAUGE, &txt))
                                    .size(12.0)
                                    .color(p.muted),
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let id = v.conn.id.clone();
                            ui.menu_button(icons::DOTS_THREE, |ui| {
                                if ui
                                    .button(icons::label(icons::PLUGS_CONNECTED, "Recheck"))
                                    .clicked()
                                {
                                    let c = v.conn.clone();
                                    model.act(move |m| async move {
                                        let s =
                                            kls_github::service(m.core()).recheck_conn(&c).await;
                                        Ok(Some(Notice::Info(format!("{} {}", c.host, s.label()))))
                                    });
                                    ui.close();
                                }
                                if ui
                                    .button(icons::label(icons::KEY, "Sign in again"))
                                    .clicked()
                                {
                                    out =
                                        Some(StripAction::SignIn(Box::new(v.conn.clone()), false));
                                    ui.close();
                                }
                                let toggle = if v.conn.enabled { "Disable" } else { "Enable" };
                                if ui.button(icons::label(icons::POWER, toggle)).clicked() {
                                    let on = !v.conn.enabled;
                                    let id = id.clone();
                                    model.act(move |m| async move {
                                        kls_github::service(m.core())
                                            .set_connection_enabled(&id, on, "user")
                                            .await?;
                                        Ok(None)
                                    });
                                    ui.close();
                                }
                                if ui.button(icons::label(icons::TRASH, "Remove")).clicked() {
                                    let id = id.clone();
                                    model.act(move |m| async move {
                                        kls_github::service(m.core())
                                            .remove_connection(&id, "user")
                                            .await?;
                                        Ok(Some(Notice::Info("Connection removed".into())))
                                    });
                                    ui.close();
                                }
                            })
                            .response
                            .on_hover_text("Options");
                            if !v.auth.signed_in()
                                && !matches!(v.auth, kls_github::forge::auth::AuthState::Missing(_))
                                && theme::primary_button(ui, &icons::label(icons::KEY, "Sign in"))
                                    .clicked()
                            {
                                out = Some(StripAction::SignIn(Box::new(v.conn.clone()), false));
                            }
                        });
                    });
                }
            });
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_rows_replace_one_repository() {
        let row = |p: &str, r: &str| MirrorRow {
            project: p.into(),
            repo: r.into(),
            ..Default::default()
        };
        let rows = vec![row("web", "acme/api"), row("ops", "acme/infra")];
        let out = rows_with(&rows, "acme/api", Some(row("app", "acme/api")));
        assert_eq!(out, vec![row("ops", "acme/infra"), row("app", "acme/api")]);
        assert_eq!(
            rows_with(&rows, "acme/api", None),
            vec![row("ops", "acme/infra")]
        );
        let c = BackfillCounts { recent: 2, all: 7 };
        assert_eq!(backfill_word("recent", Some(c)), "Recent 30 days (2)");
        assert_eq!(backfill_word("all", Some(c)), "All (7)");
        assert_eq!(backfill_word("none", Some(c)), "None");
        // A new draft is dirty (nothing stored); a stored one is not until edited.
        let mut d = MirrorDraft::of("acme/api", None);
        assert!(d.dirty() && d.row.repo == "acme/api" && d.row.backfill == "none");
        let stored = row("web", "acme/api");
        d = MirrorDraft::of("acme/api", Some(&stored));
        assert!(!d.dirty());
        d.hours = "4".into();
        assert!(d.dirty());
    }

    #[test]
    fn slsa_and_protected_branches() {
        assert_eq!(slsa_word(None).0, "-");
        let p = RepoProfile {
            signals: vec!["sigstore".into()],
            ..Default::default()
        };
        assert_eq!(slsa_word(Some(&p)).0, "signed");
        let repo = GhRepo {
            default_branch: "main".into(),
            ..Default::default()
        };
        let pulls = [GhPull {
            base_ref: "release/1".into(),
            ..Default::default()
        }];
        assert_eq!(protected_list(&repo, &pulls), vec!["main", "release/1"]);
    }

    #[tokio::test]
    async fn attach_adds_repos_to_scope_publishes_and_audits() {
        let dir = std::env::temp_dir().join(format!(
            "kls-ui-attach-{}-{}",
            std::process::id(),
            kls_store::now_ms()
        ));
        let core = kls_core::Core::open(&dir, None).await.unwrap();
        let g: kls_engine::graph::Graph = serde_json::from_value(serde_json::json!({
            "nodes": [{"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
                      {"id":"b","type":"notify","title":"Say","config":{"message":"hi"}}],
            "edges": [{"from":"t","to":"b"}],
            "scope": {"repos": ["acme/api"]}
        }))
        .unwrap();
        let w = kls_engine::ops::create_workflow(
            &core,
            kls_engine::ops::NewWorkflow {
                name: "Scan".into(),
                graph: Some(g),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        kls_engine::ops::publish(&core, &w.id, 1, "first")
            .await
            .unwrap();
        let n = attach_repos(&core, &w.id, &["acme/web".into(), "acme/api".into()])
            .await
            .unwrap();
        assert_eq!(n, 1);
        let all = attached_workflows(&core).await.unwrap();
        let a = all.iter().find(|x| x.id == w.id).unwrap();
        assert!(kls_engine::ops::repo_in_scope(&a.scope, "acme/web"));
        let led = core
            .audit_entries(&kls_store::audit::LedgerQuery {
                kinds: vec!["workflow".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            led.iter()
                .any(|e| e.subject["action"] == "workflow.attach" && e.actor == "owner")
        );
        // Already attached: nothing to do.
        assert_eq!(
            attach_repos(&core, &w.id, &["acme/web".into()])
                .await
                .unwrap(),
            0
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Fleet: connected belai nodes, joining (the Add node panel), renaming
//! and revoking.

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::model::{Model, Notice, Snapshot};
use crate::theme::{palette, primary_button};
use egui::RichText;
use egui_extras::{Column, TableBuilder};
use kls_node::token::DEFAULT_TTL_MS;

#[derive(Default)]
pub struct Fleet {
    add_open: bool,
    label: String,
    ttl_minutes: u32,
    pub minted: Option<(String, i64)>,
    confirm_revoke: Option<(String, String)>,
    rename: Option<(String, String)>,
    /// Node policy and requests waiting for approval (WP-I); built on first draw.
    cross: Option<crate::loader::Loader<CrossHost>>,
    /// The schedule being added: (node, profile, cron, directory).
    new_schedule: (String, String, String, String),
    /// The library install being prepared: (node, item id), and a pending
    /// confirm for a hook or MCP server.
    lib_install: (String, String),
    lib_confirm: bool,
    /// A hook install waiting on its files' confirmation (P5 item 1).
    lib_files: Option<crate::screens::library::FilesConfirm>,
}

#[derive(Clone, Default)]
struct CrossHost {
    policy: kls_core::belai::fleet::Policy,
    waiting: Vec<kls_store::belai::NodeRequest>,
    recent: Vec<kls_store::belai::NodeRequest>,
    /// Each node's latest live status (WP-B).
    status: Vec<(String, i64, kls_core::belai::status::NodeStatus)>,
    /// Open host requests (WP-J).
    attention: Vec<kls_store::belai::Attention>,
    /// Host schedules (WP-G).
    schedules: Vec<kls_store::belai::HostSchedule>,
    /// Library items that can go to a node (WP-E).
    library: Vec<kls_store::vault::LibraryItem>,
    /// The automatic library sync queues backups and installs (#20).
    auto_sync: bool,
    /// Each node's latest library scan (WP-K).
    scans: Vec<(String, Vec<kls_store::belai::ScanItem>)>,
    /// Each node's provider plan windows (row 48).
    plan: Vec<(String, Vec<kls_core::belai::plan::PlanLimit>)>,
}

/// The capability tags a node shows.
fn capability_tags(c: &kls_store::Capabilities) -> Vec<String> {
    let mut out = Vec::new();
    if c.remote_control {
        out.push("rc".to_string());
    }
    if c.gh_authenticated {
        out.push("gh".into());
    }
    if c.terminal {
        out.push("terminal".into());
    }
    out.extend(c.providers.iter().cloned());
    out
}

impl Fleet {
    pub fn new() -> Self {
        Self {
            ttl_minutes: (DEFAULT_TTL_MS / 60_000) as u32,
            ..Default::default()
        }
    }

    fn open_add(&mut self) {
        self.add_open = true;
        self.minted = None;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model, snap: &Snapshot) {
        // An open host view replaces the list (screens/host.rs).
        if crate::screens::host::show(ui, model, snap) {
            return;
        }
        let p = palette(ui.ctx());
        kit::page_header(ui, icons::HARD_DRIVES, "Fleet", |ui| {
            if primary_button(ui, &icons::label(icons::PLUS, "Add node")).clicked() {
                self.open_add();
            }
        });

        if self.add_open || self.minted.is_some() {
            self.add_panel(ui, model, snap);
            ui.add_space(18.0);
        }

        self.cross_ui(ui, model, snap);

        if snap.nodes.is_empty() {
            kit::card(ui)
                .inner_margin(egui::Margin::same(20))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(icons::label(icons::HARD_DRIVES, "No nodes"))
                                .color(p.muted),
                        );
                        if !self.add_open
                            && primary_button(ui, &icons::label(icons::PLUS, "Add node")).clicked()
                        {
                            self.open_add();
                        }
                    });
                });
        } else {
            self.table(ui, model, snap);
        }

        // "Last seen" labels age without any event.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(5));

        self.revoke_dialog(ui.ctx(), model);
        self.rename_dialog(ui.ctx(), model);
    }

    fn table(&mut self, ui: &mut egui::Ui, _model: &Model, snap: &Snapshot) {
        let ctx = ui.ctx().clone();
        let now = kls_store::now_ms();
        let p = palette(ui.ctx());
        kit::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 4))
                .show(ui, |ui| {
                    TableBuilder::new(ui)
                        .striped(false)
                        .sense(egui::Sense::click())
                        .resizable(true)
                        .column(Column::auto().at_least(90.0))
                        .column(Column::auto().at_least(120.0))
                        .column(Column::auto().at_least(160.0))
                        .column(Column::auto().at_least(70.0))
                        .column(Column::auto().at_least(80.0))
                        .column(Column::remainder().at_least(140.0).clip(true))
                        .column(Column::auto().at_least(80.0))
                        .header(30.0, |mut h| {
                            for t in [
                                "Status",
                                "Name",
                                "Host",
                                "belai",
                                "Last seen",
                                "Capabilities",
                                "",
                            ] {
                                h.col(|ui| {
                                    kit::rule(ui);
                                    kit::th(ui, t);
                                });
                            }
                        })
                        .body(|mut body| {
                            for n in &snap.nodes {
                                let is_online = snap.online.contains(&n.id);
                                body.row(38.0, |mut row| {
                                    let node_id = n.id.clone();
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        let (txt, state) = if n.revoked_at.is_some() {
                                            ("Revoked", "revoked")
                                        } else if is_online {
                                            ("Online", "online")
                                        } else {
                                            ("Offline", "offline")
                                        };
                                        let c = kit::status_color(&p, state);
                                        ui.horizontal(|ui| {
                                            kit::dot(ui, c);
                                            let r = ui.label(RichText::new(txt).color(c));
                                            if let Some(why) = &n.revoked_reason {
                                                r.on_hover_text(why);
                                            }
                                        });
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        ui.label(RichText::new(&n.name).strong())
                                            .on_hover_text(&n.id);
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        ui.label(kit::mono(
                                            &format!(
                                                "{} {}/{}",
                                                n.host.hostname, n.host.os, n.host.arch
                                            ),
                                            12.0,
                                            p.muted,
                                        ));
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        ui.label(kit::mono(&n.host.belai_version, 12.0, p.text));
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        ui.label(
                                            RichText::new(
                                                n.last_seen_at
                                                    .map(|t| fmt::ago(now, t))
                                                    .unwrap_or("never".into()),
                                            )
                                            .color(p.muted),
                                        )
                                        .on_hover_text(
                                            n.last_seen_at.map(fmt::when).unwrap_or_default(),
                                        );
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        for t in capability_tags(&n.host.capabilities) {
                                            kit::tag(ui, &t);
                                        }
                                        let launch: Vec<_> = n
                                            .host
                                            .harnesses
                                            .iter()
                                            .filter(|h| h.launchable())
                                            .collect();
                                        if !launch.is_empty() {
                                            kit::chip(
                                                ui,
                                                &icons::label(
                                                    icons::ROBOT,
                                                    &launch.len().to_string(),
                                                ),
                                                p.muted,
                                            )
                                            .on_hover_text({
                                                // C8 rule 1: path, version hash, probe time.
                                                let now = kls_store::now_ms();
                                                launch
                                                    .iter()
                                                    .map(|h| {
                                                        kls_core::belai::fleet::probe_provenance(
                                                            h, now,
                                                        )
                                                    })
                                                    .collect::<Vec<_>>()
                                                    .join("\n\n")
                                            });
                                        }
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        if n.revoked_at.is_none() {
                                            ui.horizontal(|ui| {
                                                if kit::ghost_button(ui, icons::CARET_RIGHT)
                                                    .on_hover_text("Open")
                                                    .clicked()
                                                {
                                                    crate::screens::host::open(
                                                        ui.ctx(),
                                                        &n.id,
                                                        None,
                                                    );
                                                }
                                                if kit::ghost_button(ui, icons::PENCIL_SIMPLE)
                                                    .on_hover_text("Rename")
                                                    .clicked()
                                                {
                                                    self.rename =
                                                        Some((n.id.clone(), n.name.clone()));
                                                }
                                                if kit::danger_button(ui, icons::PROHIBIT)
                                                    .on_hover_text("Revoke")
                                                    .clicked()
                                                {
                                                    self.confirm_revoke =
                                                        Some((n.id.clone(), n.name.clone()));
                                                }
                                            });
                                        }
                                    });
                                    if row.response().clicked() {
                                        crate::screens::host::open(&ctx, &node_id, None);
                                    }
                                });
                            }
                        });
                });
        });
    }

    /// The highlighted Add node panel: name and expiry, then the single-use
    /// `/connect` command once a token is minted.
    fn add_panel(&mut self, ui: &mut egui::Ui, model: &Model, snap: &Snapshot) {
        let p = palette(ui.ctx());
        let mut close = false;
        kit::section(
            ui,
            icons::PLUS,
            "Add node",
            true,
            |ui| {
                if kit::ghost_button(ui, &icons::label(icons::CHECK, "Done")).clicked() {
                    close = true;
                }
            },
            |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                if let Some((token, exp)) = self.minted.clone() {
                    let cmd = format!("/connect {token}");
                    ui.horizontal_wrapped(|ui| {
                        let w = (ui.available_width() - 300.0).max(260.0);
                        egui::Frame::new()
                            .fill(p.bg)
                            .stroke(egui::Stroke::new(1.0, p.line))
                            .inner_margin(egui::Margin::symmetric(14, 12))
                            .show(ui, |ui| {
                                ui.set_width(w);
                                ui.add(
                                    egui::Label::new(kit::mono(&cmd, 13.0, p.text))
                                        .wrap_mode(egui::TextWrapMode::Wrap)
                                        .selectable(true),
                                );
                            });
                        if primary_button(ui, &icons::label(icons::COPY, "Copy")).clicked() {
                            ui.ctx().copy_text(cmd.clone());
                        }
                        if kit::ghost_button(ui, &icons::label(icons::TERMINAL, "Copy headless"))
                            .on_hover_text("belai connect <token>")
                            .clicked()
                        {
                            ui.ctx().copy_text(format!("belai connect {token}"));
                        }
                    });
                    ui.label(
                        RichText::new(format!(
                            "single use · expires {} · {}",
                            fmt::when(exp),
                            snap.listen_addr
                        ))
                        .size(12.0)
                        .color(p.muted),
                    )
                    .on_hover_text("Shown once");
                } else {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        kit::field(ui, "Name", |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.label)
                                    .hint_text("optional")
                                    .desired_width(220.0),
                            );
                        });
                        kit::field(ui, "Expires", |ui| {
                            ui.add(
                                egui::Slider::new(&mut self.ttl_minutes, 5..=1440).suffix(" min"),
                            );
                        });
                        kit::field(ui, " ", |ui| {
                            if primary_button(ui, &icons::label(icons::KEY, "Create token"))
                                .clicked()
                            {
                                let label = self.label.trim().to_string();
                                let ttl = self.ttl_minutes as i64 * 60_000;
                                model.act(move |m| async move {
                                    let t = m.hub.mint_token(&label, ttl).await?;
                                    Ok(Some(Notice::Token {
                                        token: t.token,
                                        expires_at: t.expires_at,
                                    }))
                                });
                                self.label.clear();
                            }
                        });
                    });
                    ui.label(
                        RichText::new(icons::label(icons::BROADCAST, &snap.listen_addr))
                            .size(12.0)
                            .color(p.muted),
                    )
                    .on_hover_text("Listening on");
                }
            },
        );
        if close {
            self.add_open = false;
            self.minted = None;
        }
    }

    fn revoke_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some((id, name)) = self.confirm_revoke.clone() else {
            return;
        };
        egui::Window::new(icons::label(icons::PROHIBIT, "Revoke node"))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let p = palette(ui.ctx());
                ui.colored_label(
                    p.amber,
                    icons::label(icons::WARNING, &format!("Revoke {name}?")),
                )
                .on_hover_text("Certificate stops at once; rejoin needs a new token");
                ui.horizontal(|ui| {
                    if kit::danger_button(ui, &icons::label(icons::PROHIBIT, "Revoke")).clicked() {
                        let id = id.clone();
                        model.act(move |m| async move {
                            m.hub.revoke(&id, "revoked from Fleet").await?;
                            Ok(Some(Notice::Info("Node revoked".into())))
                        });
                        self.confirm_revoke = None;
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        self.confirm_revoke = None;
                    }
                });
            });
    }

    fn rename_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some((id, mut name)) = self.rename.clone() else {
            return;
        };
        let mut done = false;
        egui::Window::new(icons::label(icons::PENCIL_SIMPLE, "Rename node"))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.text_edit_singleline(&mut name);
                ui.horizontal(|ui| {
                    if primary_button(ui, &icons::label(icons::FLOPPY_DISK, "Save")).clicked()
                        && !name.trim().is_empty()
                    {
                        let (id, n) = (id.clone(), name.trim().to_string());
                        model.act(move |m| async move {
                            m.core().store().rename_node(&id, &n).await?;
                            m.core()
                                .emit(
                                    kls_core::kinds::NODE_UPDATED,
                                    &id,
                                    &serde_json::json!({ "name": n }),
                                )
                                .await?;
                            Ok(None)
                        });
                        done = true;
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        done = true;
                    }
                });
            });
        self.rename = if done { None } else { Some((id, name)) };
    }
}

impl Fleet {
    /// Cross-host dispatch: the node policy switch and the requests one
    /// node made for another that wait for the owner. Every field shown is
    /// a node's (untrusted) text.
    fn cross_ui(&mut self, ui: &mut egui::Ui, model: &Model, snap: &Snapshot) {
        use kls_core::belai::fleet::{Action, Rule};
        let loader = self.cross.get_or_insert_with(|| {
            crate::loader::Loader::new(
                model,
                &[
                    "fleet.",
                    "node.status",
                    "attention.",
                    "schedule.",
                    "library.",
                    "belai.plan_limit",
                ],
                |core| async move {
                    let recent = core.store().node_requests("", 20).await?;
                    Ok(CrossHost {
                        status: core.node_statuses().await?,
                        plan: core.plan_limits_all().await?,
                        attention: core.store().attention("open", 50).await?,
                        schedules: core.store().host_schedules(None).await?,
                        library: core.store().library_items("").await?,
                        auto_sync: core.library_auto_sync().await?,
                        scans: {
                            let mut v = Vec::new();
                            for n in core.store().nodes().await? {
                                let s = core.store().latest_scan(&n.id).await?;
                                if !s.is_empty() {
                                    v.push((n.id, s));
                                }
                            }
                            v
                        },
                        policy: core.fleet_policy().await?,
                        waiting: recent
                            .iter()
                            .filter(|r| r.state == "awaiting_approval")
                            .cloned()
                            .collect(),
                        recent,
                    })
                },
            )
        });
        let Some(Ok(c)) = loader.get() else { return };
        let p = palette(ui.ctx());
        let name = |id: &str| {
            snap.nodes
                .iter()
                .find(|n| n.id == id)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| id.chars().take(8).collect())
        };
        kit::card(ui)
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(kit::caps(
                        &icons::label(icons::TREE_STRUCTURE, "Cross-host"),
                        p.muted,
                    ));
                    let mut pol = c.policy.clone();
                    let mut changed = false;
                    changed |= ui.checkbox(&mut pol.enabled, "Requests").changed();
                    changed |= ui
                        .checkbox(&mut pol.share_dirs, "Share directories")
                        .changed();
                    changed |= ui.checkbox(&mut pol.prompts, "Prompts").changed();
                    let auto = pol.rules.iter().any(|r| r.action == Action::Auto);
                    let mut want_auto = auto;
                    changed |= ui
                        .checkbox(&mut want_auto, "Auto-approve people")
                        .on_hover_text("Agent requests always wait")
                        .changed();
                    if want_auto != auto {
                        pol.rules = if want_auto {
                            vec![Rule {
                                action: Action::Auto,
                                ..Default::default()
                            }]
                        } else {
                            Vec::new()
                        };
                    }
                    if changed {
                        model.act(move |m| async move {
                            m.core().set_fleet_policy(&pol).await?;
                            Ok(None)
                        });
                    }
                });
                for a in &c.attention {
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        kit::chip(
                            ui,
                            &icons::label(icons::BELL, &a.reason.replace('_', " ")),
                            p.gold_text,
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} · {} {}",
                                name(&a.node_id),
                                a.kind.replace('_', " "),
                                a.subject
                            ))
                            .strong(),
                        );
                        let (node, id) = (a.node_id.clone(), a.id.clone());
                        if primary_button(ui, &icons::label(icons::CHECK, "Approve")).clicked() {
                            let (node, id) = (node.clone(), id.clone());
                            model.act(move |m| async move {
                                m.core().attention_decide(&node, &id, true).await?;
                                Ok(None)
                            });
                        }
                        if kit::ghost_button(ui, &icons::label(icons::X, "Decline")).clicked() {
                            model.act(move |m| async move {
                                m.core().attention_decide(&node, &id, false).await?;
                                Ok(None)
                            });
                        }
                    });
                }
                for r in &c.waiting {
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        kit::chip(
                            ui,
                            &icons::label(icons::CLOCK, &r.initiated_by),
                            if r.initiated_by == "agent" {
                                p.gold_text
                            } else {
                                p.muted
                            },
                        );
                        let what = format!(
                            "{} → {} · {} {}",
                            name(&r.origin_node),
                            name(&r.target_node),
                            r.kind,
                            r.cwd
                        );
                        ui.label(RichText::new(what).strong());
                        let id = r.id.clone();
                        if primary_button(ui, &icons::label(icons::CHECK, "Approve")).clicked() {
                            let id = id.clone();
                            model.act(move |m| async move {
                                let t = m.core().fleet_decide(&id, true).await?;
                                Ok(Some(if t.state == "refused" {
                                    Notice::Error(t.reason)
                                } else {
                                    Notice::Info("Queued".into())
                                }))
                            });
                        }
                        if kit::ghost_button(ui, &icons::label(icons::X, "Decline")).clicked() {
                            model.act(move |m| async move {
                                m.core().fleet_decide(&id, false).await?;
                                Ok(None)
                            });
                        }
                    });
                }
                self.plan_ui(ui, snap, &c);
                self.workers_ui(ui, model, snap, &c);
                self.schedules_ui(ui, model, snap, &c);
                self.library_ui(ui, model, snap, &c);
                let refused = c.recent.iter().filter(|r| r.state == "refused").count();
                if refused > 0 {
                    ui.label(
                        RichText::new(icons::label(icons::PROHIBIT, &format!("{refused} refused")))
                            .color(p.muted)
                            .size(12.0),
                    )
                    .on_hover_text(
                        c.recent
                            .iter()
                            .filter(|r| r.state == "refused")
                            .map(|r| format!("{}: {}", name(&r.origin_node), r.reason))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                }
            });
        ui.add_space(12.0);
    }
}
/// One line per plan window: provider and window, a used meter, the reset
/// time and where the number came from (row 48). Shared by the Fleet view
/// and the Host screen.
pub(crate) fn plan_limits_ui(ui: &mut egui::Ui, limits: &[kls_core::belai::plan::PlanLimit]) {
    let p = palette(ui.ctx());
    let now = kls_store::now_ms();
    for l in limits {
        let (label, tone) = plan_row(l, kls_core::belai::plan::DEFAULT_SKIP_AT);
        let color = match tone {
            2 => p.red,
            1 => p.amber,
            _ => p.green,
        };
        ui.horizontal(|ui| {
            ui.label(kit::mono(&label, 12.0, p.text));
            if l.reset_at > 0 {
                ui.label(
                    RichText::new(icons::label(icons::TIMER, &fmt::when(l.reset_at)))
                        .color(p.muted)
                        .size(12.0),
                );
            }
            kit::tag(ui, &l.source);
        })
        .response
        .on_hover_text(format!(
            "{}seen {}",
            if l.session_id.is_empty() {
                "host status · ".to_string()
            } else {
                format!("session {} · ", l.session_id)
            },
            fmt::ago(now, l.at)
        ));
        kit::meter(ui, l.used_fraction as f32, color);
    }
}

/// The text of a plan window and its tone: 0 room, 1 past 70%, 2 at the
/// routing threshold (routing skips the node).
pub(crate) fn plan_row(l: &kls_core::belai::plan::PlanLimit, skip_at: f64) -> (String, u8) {
    let pct = (l.used_fraction * 100.0).round();
    let tone = if l.used_fraction >= skip_at {
        2
    } else if l.used_fraction >= 0.7 {
        1
    } else {
        0
    };
    (format!("{} {} {pct:.0}%", l.provider, l.window), tone)
}

impl Fleet {
    /// Provider plan windows per node (row 48).
    fn plan_ui(&mut self, ui: &mut egui::Ui, snap: &Snapshot, c: &CrossHost) {
        if c.plan.is_empty() {
            return;
        }
        let p = palette(ui.ctx());
        ui.add_space(8.0);
        ui.label(kit::caps(
            &icons::label(icons::GAUGE, "Plan limits"),
            p.muted,
        ));
        for (node_id, limits) in &c.plan {
            let name = snap
                .nodes
                .iter()
                .find(|n| &n.id == node_id)
                .map(|n| n.name.as_str())
                .unwrap_or(node_id.as_str());
            ui.label(RichText::new(icons::label(icons::HARD_DRIVES, name)).strong());
            plan_limits_ui(ui, limits);
        }
    }
}

impl Fleet {
    /// Live workers and crew runs per node, with the lifecycle controls the
    /// node takes (Capabilities.dispatch_kinds).
    fn workers_ui(&mut self, ui: &mut egui::Ui, model: &Model, snap: &Snapshot, c: &CrossHost) {
        let p = palette(ui.ctx());
        for (node_id, _at, st) in &c.status {
            if st.workers.is_empty() && st.crew_runs.is_empty() {
                continue;
            }
            let Some(n) = snap.nodes.iter().find(|n| &n.id == node_id) else {
                continue;
            };
            let caps = &n.host.capabilities;
            ui.add_space(8.0);
            ui.label(kit::caps(
                &icons::label(
                    icons::ROBOT,
                    &format!("{} · {} sessions", n.name, st.running_sessions),
                ),
                p.muted,
            ));
            // Through core: kill switch, A2A blackout for crews, audit with
            // the `fleet/workers` origin.
            let send = |c: kls_core::vault::workforce::Control| {
                let node = node_id.clone();
                model.act(move |m| async move {
                    kls_core::vault::workforce::worker_control(
                        m.core(),
                        &node,
                        c,
                        kls_core::hosts::Origin::fleet("workers"),
                    )
                    .await?;
                    Ok(Some(Notice::Info("Sent".into())))
                });
            };
            use kls_core::vault::workforce::Control;
            for w in &st.workers {
                ui.horizontal(|ui| {
                    kit::chip(ui, &w.state, kit::status_color(&p, &w.state));
                    ui.label(RichText::new(&w.profile).strong())
                        .on_hover_text(&w.id);
                    ui.label(kit::mono(&w.cwd, 12.0, p.muted));
                    if w.cost_micro_usd > 0 {
                        ui.label(kit::mono(
                            &format!("${:.2}", w.cost_micro_usd as f64 / 1e6),
                            12.0,
                            p.muted,
                        ));
                    }
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
                        send(if paused {
                            Control::Resume { worker }
                        } else {
                            Control::Pause { worker }
                        });
                    }
                    if !w.session_id.is_empty()
                        && caps.takes_dispatch("stop")
                        && kit::danger_button(ui, &icons::label(icons::STOP, "Stop")).clicked()
                    {
                        send(Control::Stop {
                            session: w.session_id.clone(),
                        });
                    }
                });
            }
            for r in &st.crew_runs {
                ui.horizontal(|ui| {
                    kit::chip(ui, &r.state, kit::status_color(&p, &r.state));
                    ui.label(RichText::new(icons::label(icons::USERS_THREE, &r.crew)).strong())
                        .on_hover_text(&r.id);
                    ui.label(kit::mono(&r.scope_dirs.join(", "), 12.0, p.muted));
                    if caps.takes_dispatch("crew_control")
                        && r.state != "stopped"
                        && kit::danger_button(ui, &icons::label(icons::STOP, "Stop run")).clicked()
                    {
                        send(Control::CrewStop {
                            run: r.id.clone(),
                            crew: r.crew.clone(),
                        });
                    }
                });
            }
        }
    }
}

impl Fleet {
    /// Host schedules (WP-G): each node's scheduled workers, editable here;
    /// the host checks and fires them.
    fn schedules_ui(&mut self, ui: &mut egui::Ui, model: &Model, snap: &Snapshot, c: &CrossHost) {
        let p = palette(ui.ctx());
        ui.add_space(8.0);
        ui.label(kit::caps(
            &icons::label(icons::CALENDAR, "Schedules"),
            p.muted,
        ));
        let name = |id: &str| {
            snap.nodes
                .iter()
                .find(|n| n.id == id)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| id.chars().take(8).collect())
        };
        for s in &c.schedules {
            ui.horizontal(|ui| {
                let mut on = s.enabled;
                if ui.checkbox(&mut on, "").on_hover_text("Enabled").changed() {
                    let mut e = s.clone();
                    e.enabled = on;
                    model.act(move |m| async move {
                        kls_core::hosts::save_schedule(
                            m.core(),
                            e,
                            kls_core::hosts::Origin::fleet("schedules"),
                        )
                        .await?;
                        Ok(None)
                    });
                }
                ui.label(RichText::new(&s.profile).strong());
                ui.label(kit::mono(
                    &format!("{} · {} · {}", s.cron, name(&s.node_id), s.cwd),
                    12.0,
                    p.muted,
                ));
                if !s.last_status.is_empty() {
                    kit::chip(ui, &s.last_status, kit::status_color(&p, &s.last_status));
                }
                if kit::ghost_button(ui, icons::TRASH)
                    .on_hover_text("Delete")
                    .clicked()
                {
                    let mut e = s.clone();
                    e.deleted = true;
                    model.act(move |m| async move {
                        kls_core::hosts::save_schedule(
                            m.core(),
                            e,
                            kls_core::hosts::Origin::fleet("schedules"),
                        )
                        .await?;
                        Ok(None)
                    });
                }
            });
        }
        let rc: Vec<&kls_store::Node> = snap
            .nodes
            .iter()
            .filter(|n| n.revoked_at.is_none() && n.host.capabilities.remote_control)
            .collect();
        if rc.is_empty() {
            return;
        }
        let (node, profile, cron, dir) = &mut self.new_schedule;
        ui.horizontal(|ui| {
            let chosen = rc.iter().find(|n| &n.id == node).copied();
            egui::ComboBox::from_id_salt("schedule_node")
                .selected_text(chosen.map(|n| n.name.as_str()).unwrap_or("Node"))
                .show_ui(ui, |ui| {
                    for n in &rc {
                        ui.selectable_value(node, n.id.clone(), &n.name);
                    }
                });
            egui::ComboBox::from_id_salt("schedule_profile")
                .selected_text(if profile.is_empty() {
                    "Profile"
                } else {
                    profile.as_str()
                })
                .show_ui(ui, |ui| {
                    for a in chosen.map(|n| n.host.agents.clone()).unwrap_or_default() {
                        ui.selectable_value(profile, a.clone(), a);
                    }
                });
            egui::ComboBox::from_id_salt("schedule_dir")
                .selected_text(if dir.is_empty() {
                    "Directory"
                } else {
                    dir.as_str()
                })
                .show_ui(ui, |ui| {
                    for d in chosen
                        .map(|n| n.host.trusted_dirs.clone())
                        .unwrap_or_default()
                    {
                        ui.selectable_value(dir, d.clone(), d);
                    }
                });
            ui.add(
                egui::TextEdit::singleline(cron)
                    .hint_text("@daily")
                    .desired_width(110.0),
            );
            let ready = chosen.is_some() && !profile.is_empty() && !dir.is_empty();
            if ui
                .add_enabled_ui(ready, |ui| {
                    primary_button(ui, &icons::label(icons::PLUS, "Add"))
                })
                .inner
                .clicked()
            {
                let s = kls_store::belai::HostSchedule {
                    node_id: node.clone(),
                    profile: profile.clone(),
                    cron: if cron.trim().is_empty() {
                        "@daily".into()
                    } else {
                        cron.trim().into()
                    },
                    cwd: dir.clone(),
                    enabled: true,
                    ..Default::default()
                };
                model.act(move |m| async move {
                    kls_core::hosts::save_schedule(
                        m.core(),
                        s,
                        kls_core::hosts::Origin::fleet("schedules"),
                    )
                    .await?;
                    Ok(Some(Notice::Info("Schedule saved".into())))
                });
                cron.clear();
            }
        });
    }
}

impl Fleet {
    /// Library distribution (WP-E): put a library item on a node. A hook or
    /// an MCP server runs code on the host, so it needs a second click.
    fn library_ui(&mut self, ui: &mut egui::Ui, model: &Model, snap: &Snapshot, c: &CrossHost) {
        let p = palette(ui.ctx());
        let rc: Vec<&kls_store::Node> = snap
            .nodes
            .iter()
            .filter(|n| {
                n.revoked_at.is_none() && n.host.capabilities.takes_dispatch("library_install")
            })
            .collect();
        if rc.is_empty() || c.library.is_empty() {
            return;
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(kit::caps(&icons::label(icons::BOOKS, "Library"), p.muted));
            let mut on = c.auto_sync;
            if ui
                .checkbox(&mut on, icons::label(icons::ARROWS_CLOCKWISE, "Auto sync"))
                .on_hover_text("Never hooks or MCP")
                .changed()
            {
                model.act(move |m| async move {
                    m.core()
                        .set_library_auto_sync(on, "owner", "fleet/library")
                        .await?;
                    Ok(None)
                });
            }
        });
        let (node, item) = &mut self.lib_install;
        let chosen_id = node.clone();
        ui.horizontal(|ui| {
            let chosen = rc.iter().find(|n| &n.id == node).copied();
            egui::ComboBox::from_id_salt("lib_node")
                .selected_text(chosen.map(|n| n.name.as_str()).unwrap_or("Node"))
                .show_ui(ui, |ui| {
                    for n in &rc {
                        ui.selectable_value(node, n.id.clone(), &n.name);
                    }
                });
            let it = c.library.iter().find(|i| &i.id == item);
            egui::ComboBox::from_id_salt("lib_item")
                .selected_text(
                    it.map(|i| format!("{} {}", i.kind, i.name))
                        .unwrap_or("Item".into()),
                )
                .show_ui(ui, |ui| {
                    for i in &c.library {
                        ui.selectable_value(item, i.id.clone(), format!("{} {}", i.kind, i.name));
                    }
                });
            if let Some(n) = chosen {
                let held = n.host.capabilities.inventory.len();
                kit::chip(ui, &icons::label(icons::BOOKS, &held.to_string()), p.muted)
                    .on_hover_text("Items on the node");
            }
            if let Some(n) = chosen
                && n.host.capabilities.takes_dispatch("library_scan")
                && kit::ghost_button(ui, &icons::label(icons::MAGNIFYING_GLASS, "Scan"))
                    .on_hover_text("List what this host's harnesses hold")
                    .clicked()
            {
                let node = n.id.clone();
                model.act(move |m| async move {
                    kls_core::hosts::library_dispatch(
                        m.core(),
                        &node,
                        "library_scan",
                        Vec::new(),
                        false,
                        kls_core::hosts::Origin::fleet("library"),
                    )
                    .await?;
                    Ok(Some(Notice::Info("Scan sent".into())))
                });
            }
            let risky = it.is_some_and(|i| matches!(i.kind.as_str(), "hook" | "mcp"));
            let ready = chosen.is_some() && it.is_some();
            let label = if risky && !self.lib_confirm {
                icons::label(icons::WARNING, "Install (runs code)")
            } else {
                icons::label(icons::DOWNLOAD_SIMPLE, "Install")
            };
            if ui
                .add_enabled_ui(ready, |ui| primary_button(ui, &label))
                .inner
                .clicked()
            {
                if risky && !self.lib_confirm {
                    self.lib_confirm = true;
                    return;
                }
                self.lib_confirm = false;
                let it = it.cloned().unwrap();
                if it.kind == "hook" {
                    // A hook's bundle files get their own window.
                    self.lib_files =
                        Some(crate::screens::library::FilesConfirm::open(model, node, it));
                } else {
                    // The second click above is the MCP confirmation.
                    crate::screens::library::send_install(
                        model,
                        node,
                        &it,
                        false,
                        kls_core::hosts::Origin::fleet("library"),
                    );
                }
            }
        });
        let step = self.lib_files.as_mut().map(|f| f.show(ui.ctx()));
        match step {
            Some(crate::screens::library::FilesStep::Cancel) => self.lib_files = None,
            Some(crate::screens::library::FilesStep::Send { confirm }) => {
                if let Some(fc) = self.lib_files.take() {
                    fc.send(model, confirm, kls_core::hosts::Origin::fleet("library"));
                }
            }
            _ => {}
        }
        // The chosen node's latest scan: import an item into the library.
        if let Some((_, items)) = c.scans.iter().find(|(n, _)| *n == chosen_id) {
            for it in items.iter().filter(|i| i.verdict != "invalid") {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} {}", it.kind, it.name)).strong());
                    ui.label(kit::mono(
                        &format!("{} · {}", it.harness, it.path),
                        12.0,
                        p.muted,
                    ));
                    if kit::ghost_button(ui, &icons::label(icons::DOWNLOAD_SIMPLE, "Import"))
                        .clicked()
                    {
                        let (scan, idx, node) =
                            (it.dispatch_id.clone(), it.idx, it.node_id.clone());
                        model.act(move |m| async move {
                            kls_core::hosts::library_import(
                                m.core(),
                                &node,
                                &scan,
                                idx,
                                kls_core::hosts::Origin::fleet("library"),
                            )
                            .await?;
                            Ok(Some(Notice::Info("Import sent".into())))
                        });
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_rows_show_provider_window_and_tone() {
        let l = |f: f64| kls_core::belai::plan::PlanLimit {
            provider: "anthropic".into(),
            window: "5h".into(),
            used_fraction: f,
            ..Default::default()
        };
        assert_eq!(plan_row(&l(0.42), 0.9), ("anthropic 5h 42%".to_string(), 0));
        assert_eq!(plan_row(&l(0.75), 0.9).1, 1);
        assert_eq!(plan_row(&l(0.95), 0.9).1, 2, "routing skips the node");
    }

    #[test]
    fn capability_tags_list_flags_then_providers() {
        let c = kls_store::Capabilities {
            gh_authenticated: true,
            terminal: false,
            remote_control: true,
            providers: vec!["anthropic".into()],
            ..Default::default()
        };
        assert_eq!(capability_tags(&c), ["rc", "gh", "anthropic"]);
    }
}

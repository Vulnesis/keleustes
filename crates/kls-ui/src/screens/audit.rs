//! Audit screen: the hash-chained ledger (filters, verify, JSONL export),
//! deleting sessions (a tombstone stays in the chain), teleports, the signed
//! decision ledger and the weekly report.
//! Owner: M6 finish workstream.

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme::{self, Palette};
use egui::{Color32, Margin, RichText, Stroke};
use egui_extras::{Column, TableBuilder};
use kls_core::audit::{LedgerEntry, LedgerQuery, Teleport, VerifyReport, kinds};
use kls_core::sessions::{Session, SessionQuery};
use kls_store::{Node, now_ms};
use std::sync::{Arc, Mutex};

pub const READY: bool = true;

/// Rows the ledger table holds at once (it renders only what is visible).
const LEDGER_WINDOW: i64 = 5000;
/// The delete dialog's id for "the ticked sessions".
const MULTI: &str = "\u{0}picked";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Ledger,
    Sessions,
    Teleports,
    Decisions,
    Weekly,
}

#[derive(Clone, Default)]
struct LedgerData {
    entries: Arc<Vec<LedgerEntry>>,
    total: i64,
    kinds: Arc<Vec<(String, i64)>>,
    head_seq: i64,
    head_hash: String,
    last_verify: Option<serde_json::Value>,
}

/// Sessions matching the fact filters (`None`: no filter) and the most
/// common repositories across sessions.
type FactHits = (Option<Vec<String>>, Vec<(String, i64)>);

#[derive(Clone, Default)]
struct SideData {
    sessions: Arc<Vec<Session>>,
    nodes: Arc<Vec<Node>>,
    teleports: Arc<Vec<Teleport>>,
}

/// The decision ledger and one week's report.
#[derive(Clone, Default)]
struct GovData {
    decisions: Arc<Vec<LedgerEntry>>,
    week: Arc<kls_core::audit::report::WeeklyReport>,
}

#[derive(Default)]
struct NewTeleport {
    session_id: String,
    from: String,
    to: String,
}

pub struct Screen {
    ledger: Loader<LedgerData>,
    side: Loader<SideData>,
    query: Arc<Mutex<LedgerQuery>>,
    tab: Tab,
    text: String,
    kind: String,
    origin: String,
    session: String,
    applied: (String, String, String, String, String),
    /// Card, agent, repo, workflow, run or vulnerability ids; all must match.
    facts: String,
    /// Export window in days (0: everything filtered).
    export_days: i64,
    /// Where the signed export goes, while its dialog is open.
    export_path: Option<String>,
    selected: Option<i64>,
    verify: Arc<Mutex<Option<Result<VerifyReport, String>>>>,
    verifying: bool,
    session_filter: String,
    /// Fact filters of `session_filter`, and the sessions holding them.
    fact_query: Arc<Mutex<Vec<(String, String)>>>,
    fact_hits: Loader<FactHits>,
    confirm_delete: Option<(String, String)>,
    /// Sessions ticked for a bulk delete.
    picked: std::collections::BTreeSet<String>,
    delete_reason: String,
    new_teleport: Option<NewTeleport>,
    /// Decisions and one week's report; `week_back` weeks before this one.
    gov: Loader<GovData>,
    week_back: Arc<std::sync::atomic::AtomicI64>,
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        let fact_query: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let fq = fact_query.clone();
        let fact_hits = Loader::new(model, &["session."], move |core| {
            let q = fq.lock().unwrap().clone();
            async move {
                let hits = if q.is_empty() {
                    None
                } else {
                    Some(kls_core::audit::facts::sessions_with(&core, &q, 1000).await?)
                };
                Ok((
                    hits,
                    kls_core::audit::facts::fact_values(&core, "repo", 12).await?,
                ))
            }
        });
        let query = Arc::new(Mutex::new(LedgerQuery {
            limit: LEDGER_WINDOW,
            ..Default::default()
        }));
        let q = query.clone();
        let ledger = Loader::new(model, &kinds::PREFIXES, move |core| {
            let q = q.lock().unwrap().clone();
            async move {
                let store = core.store();
                let mut count_q = q.clone();
                count_q.limit = 0;
                let head = store.ledger_head().await?;
                Ok(LedgerData {
                    entries: Arc::new(store.ledger(&q).await?),
                    total: store.ledger_count(&count_q).await?,
                    kinds: Arc::new(store.ledger_kinds().await?),
                    head_seq: head.seq,
                    head_hash: head.hash,
                    last_verify: core.audit_last_verify().await?,
                })
            }
        });
        let side = Loader::new(
            model,
            &["session.", "node.", "teleport."],
            |core| async move {
                Ok(SideData {
                    sessions: Arc::new(
                        core.store()
                            .sessions(&SessionQuery {
                                limit: 500,
                                ..Default::default()
                            })
                            .await?,
                    ),
                    nodes: Arc::new(core.store().nodes().await?),
                    teleports: Arc::new(core.teleports(200).await?),
                })
            },
        );
        let week_back = Arc::new(std::sync::atomic::AtomicI64::new(0));
        let wb = week_back.clone();
        let gov = Loader::new(model, &kinds::PREFIXES, move |core| {
            let back = wb.load(std::sync::atomic::Ordering::Relaxed);
            async move {
                use kls_core::audit::report;
                let start = report::week_start(now_ms()) - back * 7 * 86_400_000;
                Ok(GovData {
                    decisions: Arc::new(report::decisions(&core, None, 500).await?),
                    week: Arc::new(report::weekly_report(&core, start).await?),
                })
            }
        });
        // The ledger folds host lines in the background; catch up on open.
        model.act(|m| async move {
            m.core().audit_sync().await?;
            Ok(None)
        });
        Self {
            ledger,
            side,
            query,
            tab: Tab::Ledger,
            text: String::new(),
            kind: String::new(),
            origin: String::new(),
            session: String::new(),
            applied: Default::default(),
            facts: String::new(),
            export_days: 90,
            export_path: None,
            selected: None,
            verify: Arc::default(),
            verifying: false,
            session_filter: String::new(),
            fact_query,
            fact_hits,
            confirm_delete: None,
            picked: Default::default(),
            delete_reason: String::new(),
            new_teleport: None,
            gov,
            week_back,
        }
    }

    /// Focuses a session's ledger entries (`session id`) or an entry (`seq`).
    pub fn focus(&mut self, id: &str) {
        self.tab = Tab::Ledger;
        if let Some(n) = id.strip_prefix("node:") {
            self.query.lock().unwrap().node_id = Some(n.to_string()).filter(|n| !n.is_empty());
            self.ledger.reload();
        } else if let Some(k) = id.strip_prefix("kind:") {
            self.kind = k.to_string();
        } else if let Some(v) = id.strip_prefix("fact:") {
            self.facts = v.to_string();
        } else if let Ok(seq) = id.parse::<i64>() {
            self.selected = Some(seq);
        } else {
            self.session = id.to_string();
        }
    }

    fn apply_filters(&mut self) {
        let key = (
            self.text.trim().to_string(),
            self.kind.clone(),
            self.origin.clone(),
            self.session.trim().to_string(),
            self.facts.trim().to_string(),
        );
        if key == self.applied {
            return;
        }
        {
            let mut q = self.query.lock().unwrap();
            q.text = Some(key.0.clone()).filter(|s| !s.is_empty());
            q.kinds = if key.1.is_empty() {
                Vec::new()
            } else {
                vec![key.1.clone()]
            };
            q.origin = Some(key.2.clone()).filter(|s| !s.is_empty());
            q.session_id = Some(key.3.clone()).filter(|s| !s.is_empty());
            q.facts = key.4.split_whitespace().map(str::to_string).collect();
        }
        self.applied = key;
        self.ledger.reload();
    }

    /// Where to write the signed export, and its range.
    fn export_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(path) = &mut self.export_path else {
            return;
        };
        let mut open = true;
        let mut go = false;
        egui::Window::new(icons::label(icons::EXPORT, "Export signed"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.add(egui::TextEdit::singleline(path).desired_width(420.0))
                    .on_hover_text("File to write");
                ui.horizontal(|ui| {
                    for (d, l) in [(30, "30 d"), (90, "90 d"), (400, "400 d"), (0, "All")] {
                        if kit::pill(ui, l, self.export_days == d).clicked() {
                            self.export_days = d;
                        }
                    }
                });
                ui.weak(icons::label(icons::FUNNEL, "Current filters apply"));
                if theme::primary_button(ui, &icons::label(icons::SEAL_CHECK, "Sign and save"))
                    .clicked()
                {
                    go = true;
                }
            });
        if go {
            let mut q = self.query.lock().unwrap().clone();
            q.limit = 0;
            if self.export_days > 0 {
                q.from_at = Some(kls_store::now_ms() - self.export_days * 86_400_000);
            }
            let p = std::path::PathBuf::from(path.trim());
            model.act(move |m| async move {
                let n = m.core().audit_export_to(&q, &p).await?;
                let key = m.core().audit_public_key().await?;
                Ok(Some(Notice::Info(format!(
                    "Signed {n} entries to {} · key {}…",
                    p.display(),
                    &key[..16]
                ))))
            });
            open = false;
        }
        if !open {
            self.export_path = None;
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        self.export_dialog(&ui.ctx().clone(), model);
        kit::page_header(ui, icons::SCROLL, "Audit", |ui| {
            egui::ComboBox::from_id_salt("audit_export_days")
                .selected_text(match self.export_days {
                    0 => "All".to_string(),
                    d => format!("{d} d"),
                })
                .show_ui(ui, |ui| {
                    for (d, l) in [(30, "30 d"), (90, "90 d"), (400, "400 d"), (0, "All")] {
                        ui.selectable_value(&mut self.export_days, d, l);
                    }
                })
                .response
                .on_hover_text("Export range");
            if kit::ghost_button(ui, &icons::label(icons::EXPORT, "Export signed"))
                .on_hover_text(
                    "JSONL of the filtered entries, oldest first, the chain head, then an Ed25519 signature",
                )
                .clicked()
            {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                let day = crate::fmt::when(kls_store::now_ms())
                    .split(' ')
                    .next()
                    .unwrap_or("")
                    .to_string();
                self.export_path = Some(format!("{home}/Downloads/keleustes-audit-{day}.jsonl"));
            }
            if theme::primary_button(ui, &icons::label(icons::SEAL_CHECK, "Verify chain"))
                .on_hover_text("Recompute every chain")
                .clicked()
                && !self.verifying
            {
                self.verifying = true;
                *self.verify.lock().unwrap() = None;
                let slot = self.verify.clone();
                model.act(move |m| async move {
                    let r = m.core().audit_verify().await.map_err(|e| format!("{e:#}"));
                    *slot.lock().unwrap() = Some(r);
                    Ok(None)
                });
            }
        });
        self.verify_strip(ui);
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            for (tab, icon, label, hover) in [
                (Tab::Ledger, icons::LIST_CHECKS, "Ledger", ""),
                (
                    Tab::Sessions,
                    icons::TRASH,
                    "Sessions",
                    "Delete sessions (a tombstone stays in the chain)",
                ),
                (Tab::Teleports, icons::BROADCAST, "Teleports", ""),
                (
                    Tab::Decisions,
                    icons::SEAL_CHECK,
                    "Decisions",
                    "Approvals, System One, overrides, kill switch",
                ),
                (Tab::Weekly, icons::CALENDAR, "Weekly", ""),
            ] {
                let r = kit::pill(ui, &icons::label(icon, label), self.tab == tab);
                let r = if hover.is_empty() {
                    r
                } else {
                    r.on_hover_text(hover)
                };
                if r.clicked() {
                    self.tab = tab;
                }
            }
        });
        ui.add_space(12.0);
        match self.tab {
            Tab::Ledger => self.ledger_ui(ui, model),
            Tab::Sessions => self.sessions_ui(ui, model),
            Tab::Teleports => self.teleports_ui(ui, model),
            Tab::Decisions => self.decisions_ui(ui, model),
            Tab::Weekly => self.weekly_ui(ui, model),
        }
        self.delete_dialog(ui.ctx(), model);
        self.teleport_dialog(ui.ctx(), model);
    }

    /// The decision ledger: newest first, exported signed.
    fn decisions_ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let p = theme::palette(ui.ctx());
        let data = match self.gov.get() {
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
        ui.horizontal(|ui| {
            ui.label(kit::caps(
                &icons::label(
                    icons::SEAL_CHECK,
                    &format!("{} decisions", data.decisions.len()),
                ),
                p.muted,
            ));
            if kit::ghost_button(ui, &icons::label(icons::EXPORT, "Export signed"))
                .on_hover_text("JSONL of every decision entry with an Ed25519 signature")
                .clicked()
            {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                let path = std::path::PathBuf::from(format!(
                    "{home}/Downloads/keleustes-decisions-{}.jsonl",
                    now_ms()
                ));
                model.act(move |m| async move {
                    let n =
                        kls_core::audit::report::export_decisions(m.core(), None, &path).await?;
                    Ok(Some(Notice::Info(format!(
                        "{n} decisions signed to {}",
                        path.display()
                    ))))
                });
            }
        });
        ui.add_space(8.0);
        if data.decisions.is_empty() {
            ui.weak("No decisions yet");
        }
        egui::ScrollArea::vertical()
            .id_salt("audit_decisions")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for e in data.decisions.iter() {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(fmt::when(e.at))
                                .monospace()
                                .size(12.0)
                                .color(p.muted),
                        );
                        kit::chip(ui, &e.kind, kind_color(&p, &e.kind));
                        ui.label(RichText::new(icons::label(icons::USER, &e.actor)).size(12.0));
                        ui.add(egui::Label::new(&e.msg).truncate());
                        ui.label(kit::mono(&short_hash(&e.hash), 11.0, p.muted))
                            .on_hover_text(format!("#{} {}", e.seq, e.hash));
                    });
                }
            });
    }

    /// One ISO week of the ledger, summed; save writes the Markdown report.
    fn weekly_ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        use std::sync::atomic::Ordering;
        let p = theme::palette(ui.ctx());
        let back = self.week_back.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            if kit::ghost_button(ui, "\u{E138}") // caret-left
                .on_hover_text("Week before")
                .clicked()
            {
                self.week_back.store(back + 1, Ordering::Relaxed);
                self.gov.reload();
            }
            ui.add_enabled_ui(back > 0, |ui| {
                if kit::ghost_button(ui, icons::CARET_RIGHT)
                    .on_hover_text("Week after")
                    .clicked()
                {
                    self.week_back.store(back - 1, Ordering::Relaxed);
                    self.gov.reload();
                }
            });
        });
        let data = match self.gov.get() {
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
        let w = data.week.clone();
        ui.horizontal(|ui| {
            ui.label(kit::caps(
                &icons::label(
                    icons::CALENDAR,
                    &format!(
                        "{} – {}",
                        fmt::when(w.week_start).split(' ').next().unwrap_or(""),
                        fmt::when(w.week_end - 1).split(' ').next().unwrap_or("")
                    ),
                ),
                p.muted,
            ));
            if w.chain_ok {
                kit::chip(
                    ui,
                    &icons::label(icons::SEAL_CHECK, "Chain verified"),
                    p.green,
                );
            } else {
                kit::chip(
                    ui,
                    &icons::label(icons::SEAL_WARNING, "Chain not verified"),
                    p.red,
                );
            }
            if kit::ghost_button(ui, &icons::label(icons::FILE_TEXT, "Save report")).clicked() {
                let start = w.week_start;
                model.act(move |m| async move {
                    let (path, _) =
                        kls_core::audit::report::save_weekly_report(m.core(), start).await?;
                    Ok(Some(Notice::Info(format!("Report in {}", path.display()))))
                });
            }
        });
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
            for (icon, label, n, warn) in [
                (icons::LIST_CHECKS, "Entries", w.entries, false),
                (icons::SEAL_CHECK, "Decisions", w.decisions, false),
                (icons::PROHIBIT, "Refusals", w.refusals, w.refusals > 0),
                (icons::FLOW_ARROW, "Runs", w.runs_finished, false),
                (
                    icons::X_CIRCLE,
                    "Runs failed",
                    w.runs_failed,
                    w.runs_failed > 0,
                ),
                (
                    icons::WALLET,
                    "Budget",
                    w.budget_alerts,
                    w.budget_alerts > 0,
                ),
                (icons::LOCK_SIMPLE, "Vault", w.vault_changes, false),
                (icons::GEAR_SIX, "Settings", w.settings_changed, false),
            ] {
                kit::kpi(
                    ui,
                    icon,
                    label,
                    &n.to_string(),
                    None,
                    warn.then_some(p.amber),
                );
            }
        });
        ui.add_space(12.0);
        kit::Split::new(320.0, 320.0, kit::Widths::Halves).show(ui, |ui, side| {
            let (icon, title, rows) = if side == 0 {
                (icons::LIST_CHECKS, "By kind", &w.by_kind)
            } else {
                (icons::USER, "Top actors", &w.top_actors)
            };
            kit::section(
                ui,
                icon,
                title,
                false,
                |_| {},
                |ui| {
                    if rows.is_empty() {
                        ui.weak("—");
                    }
                    let max = rows.iter().map(|r| r.1).max().unwrap_or(1).max(1);
                    for (k, n) in rows.iter().take(15) {
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(k.as_str()).truncate());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(n.to_string())
                                            .monospace()
                                            .size(12.0)
                                            .color(p.muted),
                                    );
                                },
                            );
                        });
                        kit::meter(ui, *n as f32 / max as f32, p.violet);
                    }
                },
            );
        });
    }

    /// The chain status strip: verdict, then entries, head and host streams.
    fn verify_strip(&mut self, ui: &mut egui::Ui) {
        let pal = theme::palette(ui.ctx());
        let fresh = self.verify.lock().unwrap().clone();
        let data = match self.ledger.get() {
            Some(Ok(d)) => Some(d),
            _ => None,
        };
        // (icon, words, colour, hover, host streams)
        let mut streams: Option<usize> = None;
        let (icon, words, color, hover) = if let Some(r) = &fresh {
            self.verifying = false;
            match r {
                Ok(v) => {
                    let f = v.factory().cloned().unwrap_or_default();
                    streams = Some(v.chains.len().saturating_sub(1));
                    if v.ok {
                        (
                            icons::SEAL_CHECK,
                            format!("Verified {}", fmt::when(v.verified_at)),
                            pal.green,
                            format!(
                                "{} entries · {} host streams · head {} · {} ms",
                                f.entries,
                                v.chains.len().saturating_sub(1),
                                short_hash(&f.head),
                                v.ms
                            ),
                        )
                    } else {
                        let detail: Vec<String> = v
                            .chains
                            .iter()
                            .filter(|c| !c.ok)
                            .filter_map(|c| {
                                c.broken_at.as_ref().map(|b| {
                                    format!(
                                        "{} broken at {} (index {}, {}): expected {}, found {}",
                                        c.chain,
                                        b.seq,
                                        b.index,
                                        b.reason,
                                        short_hash(&b.expected),
                                        short_hash(&b.found)
                                    )
                                })
                            })
                            .collect();
                        let first = v
                            .chains
                            .iter()
                            .find_map(|c| c.broken_at.as_ref().map(|b| b.seq))
                            .unwrap_or(0);
                        (
                            icons::WARNING,
                            format!("Broken at {first}"),
                            pal.red,
                            detail.join("\n"),
                        )
                    }
                }
                Err(e) => (icons::WARNING, "Verify failed".into(), pal.red, e.clone()),
            }
        } else if self.verifying {
            (
                icons::CIRCLE_NOTCH,
                "Verifying…".into(),
                pal.gold_text,
                String::new(),
            )
        } else {
            match data.as_ref().and_then(|d| d.last_verify.clone()) {
                Some(v) => {
                    let ok = v["ok"].as_bool().unwrap_or(false);
                    let at = v["at"].as_i64().unwrap_or(0);
                    if ok {
                        (
                            icons::SEAL_CHECK,
                            format!("Intact · {}", fmt::ago(now_ms(), at)),
                            pal.green,
                            format!("Last verified {}", fmt::when(at)),
                        )
                    } else {
                        (
                            icons::WARNING,
                            format!("Broken · {}", fmt::ago(now_ms(), at)),
                            pal.red,
                            format!("Last verified {}", fmt::when(at)),
                        )
                    }
                }
                None => (
                    icons::QUESTION,
                    "Not verified yet".into(),
                    pal.muted,
                    String::new(),
                ),
            }
        };
        let edge = if color == pal.muted || color == pal.gold_text {
            pal.line
        } else {
            color
        };
        kit::card(ui)
            .stroke(Stroke::new(1.0, edge))
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    if self.verifying {
                        ui.spinner();
                    }
                    let r = ui.label(
                        RichText::new(icons::label(icon, &words))
                            .strong()
                            .color(color),
                    );
                    if !hover.is_empty() {
                        r.on_hover_text(hover);
                    }
                    ui.add_space(12.0);
                    if let Some(d) = &data {
                        let mut parts = vec![
                            format!("{} entries", d.head_seq),
                            format!("head {}", chain_hash(&d.head_hash)),
                        ];
                        if let Some(n) = streams {
                            parts.push(format!("{n} host streams"));
                        }
                        ui.label(
                            RichText::new(parts.join(" · "))
                                .monospace()
                                .size(12.0)
                                .color(pal.muted),
                        );
                    }
                });
            });
    }

    fn ledger_ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let pal = theme::palette(ui.ctx());
        let data = match self.ledger.get() {
            Some(Ok(d)) => d,
            Some(Err(e)) => {
                ui.colored_label(pal.red, e);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        let nodes = match self.side.get() {
            Some(Ok(d)) => d.nodes.clone(),
            _ => Arc::default(),
        };
        kit::card(ui).inner_margin(Margin::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                kit::field(ui, "Host", |ui| {
                    egui::ComboBox::from_id_salt("audit_origin")
                        .selected_text(match self.origin.as_str() {
                            "" => "Factory and hosts",
                            "factory" => "Factory only",
                            _ => "Hosts only",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.origin,
                                String::new(),
                                "Factory and hosts",
                            );
                            ui.selectable_value(&mut self.origin, "factory".into(), "Factory only");
                            ui.selectable_value(&mut self.origin, "host".into(), "Hosts only");
                        });
                });
                kit::field(ui, "Kind", |ui| {
                    let label = if self.kind.is_empty() {
                        "All kinds".to_string()
                    } else {
                        self.kind.clone()
                    };
                    egui::ComboBox::from_id_salt("audit_kind")
                        .selected_text(label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.kind, String::new(), "All kinds");
                            for (k, n) in data.kinds.iter() {
                                ui.selectable_value(
                                    &mut self.kind,
                                    k.clone(),
                                    format!("{k} ({n})"),
                                );
                            }
                        });
                });
                kit::field(ui, "Search", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.text)
                            .hint_text(icons::label(icons::MAGNIFYING_GLASS, "Search"))
                            .desired_width(260.0),
                    );
                });
                let node = self.query.lock().unwrap().node_id.clone();
                if let Some(n) = node {
                    kit::field(ui, "Node", |ui| {
                        if kit::ghost_button(ui, &icons::label(icons::X, &n))
                            .on_hover_text("Clear the node filter")
                            .clicked()
                        {
                            self.query.lock().unwrap().node_id = None;
                            self.ledger.reload();
                        }
                    });
                }
                kit::field(ui, "Session", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.session)
                            .hint_text("Session id")
                            .desired_width(150.0),
                    );
                });
                kit::field(ui, "Card, agent, repo, run, vuln", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.facts)
                            .hint_text(icons::label(icons::FUNNEL, "K-1 acme/api CVE-…"))
                            .desired_width(200.0),
                    )
                    .on_hover_text("Space separated; every value must appear in the entry");
                });
                kit::field(ui, "", |ui| {
                    if kit::ghost_button(ui, icons::X)
                        .on_hover_text("Clear filters")
                        .clicked()
                    {
                        self.text.clear();
                        self.kind.clear();
                        self.origin.clear();
                        self.session.clear();
                        self.facts.clear();
                    }
                });
                kit::field(ui, "Shown", |ui| {
                    let resp = ui.label(
                        RichText::new(format!("{} / {}", data.total, data.head_seq))
                            .monospace()
                            .color(pal.muted),
                    );
                    if data.total > data.entries.len() as i64 {
                        resp.on_hover_text(format!(
                            "Newest {} shown; export for all",
                            data.entries.len()
                        ));
                    } else {
                        resp.on_hover_text("Matching / total entries");
                    }
                });
            });
        });
        self.apply_filters();
        ui.add_space(12.0);

        let detail_h = if self.selected.is_some() { 220.0 } else { 0.0 };
        let entries = data.entries.clone();
        let mut clicked = None;
        let avail = (ui.available_height() - detail_h - 40.0).max(120.0);
        kit::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if entries.is_empty() {
                egui::Frame::new()
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.weak("No entries");
                    });
                return;
            }
            egui::ScrollArea::horizontal()
                .id_salt("audit_ledger_h")
                .show(ui, |ui| {
                    TableBuilder::new(ui)
                        .id_salt("audit_ledger")
                        .striped(false)
                        .resizable(true)
                        .max_scroll_height(avail)
                        .sense(egui::Sense::click())
                        .column(Column::exact(70.0))
                        .column(Column::initial(140.0))
                        .column(Column::initial(100.0).clip(true))
                        .column(Column::initial(130.0).clip(true))
                        .column(Column::initial(120.0).clip(true))
                        .column(Column::remainder().at_least(220.0).clip(true))
                        .column(Column::initial(190.0).at_least(120.0).clip(true))
                        .header(30.0, |mut h| {
                            for t in [
                                "#",
                                "When",
                                "Host",
                                "Kind",
                                "Actor",
                                "Detail",
                                "Hash ← prev",
                            ] {
                                h.col(|ui| {
                                    kit::rule(ui);
                                    kit::th(ui, t);
                                });
                            }
                        })
                        .body(|body| {
                            body.rows(30.0, entries.len(), |mut row| {
                                let e = &entries[row.index()];
                                let i = row.index();
                                row.set_selected(self.selected == Some(e.seq));
                                row.col(|ui| {
                                    kit::rule(ui);
                                    ui.label(
                                        RichText::new(e.seq.to_string())
                                            .monospace()
                                            .color(pal.muted),
                                    );
                                });
                                row.col(|ui| {
                                    kit::rule(ui);
                                    let when = fmt::when(e.at);
                                    let (day, time) = when.split_once(' ').unwrap_or(("", ""));

                                    let first = i == 0
                                        || fmt::when(entries[i - 1].at).split(' ').next()
                                            != Some(day);
                                    if first {
                                        ui.label(RichText::new(day).monospace().strong())
                                            .on_hover_text(&when);
                                    } else {
                                        ui.label(RichText::new(time).monospace().color(pal.muted))
                                            .on_hover_text(&when);
                                    }
                                });
                                row.col(|ui| {
                                    kit::rule(ui);
                                    let host = if e.origin == "factory" {
                                        "factory".to_string()
                                    } else {
                                        node_name(&nodes, &e.node_id)
                                    };
                                    ui.add(egui::Label::new(host).truncate())
                                        .on_hover_text(&e.origin);
                                });
                                row.col(|ui| {
                                    kit::rule(ui);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&e.kind)
                                                .monospace()
                                                .color(kind_color(&pal, &e.kind)),
                                        )
                                        .truncate(),
                                    );
                                });
                                row.col(|ui| {
                                    kit::rule(ui);
                                    ui.add(
                                        egui::Label::new(format!("{} ({})", e.actor, e.actor_kind))
                                            .truncate(),
                                    );
                                });
                                row.col(|ui| {
                                    kit::rule(ui);
                                    ui.add(egui::Label::new(&e.msg).truncate());
                                });
                                row.col(|ui| {
                                    kit::rule(ui);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(format!(
                                                "{} ← {}",
                                                chain_hash(&e.hash),
                                                chain_hash(&e.prev_hash)
                                            ))
                                            .monospace()
                                            .size(12.0)
                                            .color(pal.muted),
                                        )
                                        .truncate(),
                                    );
                                });
                                if row.response().clicked() {
                                    clicked = Some(e.seq);
                                }
                            });
                        });
                });
        });
        if let Some(s) = clicked {
            self.selected = if self.selected == Some(s) {
                None
            } else {
                Some(s)
            };
        }
        // Paging past the window: older pages by sequence.
        let paged = self.query.lock().unwrap().before_seq.is_some();
        if entries.len() as i64 >= LEDGER_WINDOW || paged {
            ui.horizontal(|ui| {
                if entries.len() as i64 >= LEDGER_WINDOW
                    && let Some(last) = entries.last()
                    && kit::ghost_button(ui, &icons::label(icons::ARROW_DOWN, "Older")).clicked()
                {
                    self.query.lock().unwrap().before_seq = Some(last.seq);
                    self.ledger.reload();
                }
                if paged
                    && kit::ghost_button(ui, &icons::label(icons::ARROW_UP, "Newest")).clicked()
                {
                    self.query.lock().unwrap().before_seq = None;
                    self.ledger.reload();
                }
            });
        }
        if let Some(seq) = self.selected {
            ui.add_space(12.0);
            kit::glow(ui)
                .inner_margin(Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    match entries.iter().find(|e| e.seq == seq) {
                        Some(e) => {
                            if let Some((s, focus)) = entry_detail(ui, e, &mut self.selected) {
                                model.go(s, focus);
                            }
                        }
                        None => {
                            ui.horizontal(|ui| {
                                ui.weak(format!("Entry {seq} filtered out"));
                                if kit::ghost_button(ui, "Close").clicked() {
                                    self.selected = None;
                                }
                            });
                        }
                    }
                });
        }
    }

    fn sessions_ui(&mut self, ui: &mut egui::Ui, _model: &Model) {
        let pal = theme::palette(ui.ctx());
        let data = match self.side.get() {
            Some(Ok(d)) => d,
            Some(Err(e)) => {
                ui.colored_label(pal.red, e);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        ui.add(
            egui::TextEdit::singleline(&mut self.session_filter)
                .hint_text(icons::label(
                    icons::FUNNEL,
                    "Filter, or repo: pr: agent: model: node: vuln:",
                ))
                .desired_width(260.0),
        );
        ui.add_space(12.0);
        // `kind:value` words filter by the session fact index; the rest match text.
        let facts = kls_core::audit::facts::parse_filters(&self.session_filter);
        {
            let mut q = self.fact_query.lock().unwrap();
            if *q != facts {
                *q = facts.clone();
                drop(q);
                self.fact_hits.reload();
            }
        }
        let (hits, top) = self
            .fact_hits
            .get()
            .and_then(Result::ok)
            .unwrap_or_default();
        if !top.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(icons::label(icons::GIT_BRANCH, "Repos")).color(pal.muted));
                for (repo, n) in top.iter().take(8) {
                    if kit::pill(
                        ui,
                        &format!("{repo} {n}"),
                        facts.iter().any(|f| f.1 == *repo),
                    )
                    .clicked()
                    {
                        self.session_filter = format!("repo:{repo}");
                    }
                }
            });
        }
        let words: Vec<String> = self
            .session_filter
            .split_whitespace()
            .filter(|w| kls_core::audit::facts::parse_filters(w).is_empty())
            .map(str::to_lowercase)
            .collect();
        let rows: Vec<&Session> = data
            .sessions
            .iter()
            .filter(|s| facts.is_empty() || hits.as_ref().is_some_and(|h| h.contains(&s.id)))
            .filter(|s| {
                words.iter().all(|w| {
                    [&s.title, &s.cwd, &s.id, &s.repo]
                        .iter()
                        .any(|f| f.to_lowercase().contains(w.as_str()))
                })
            })
            .collect();
        if rows.is_empty() {
            ui.weak("No sessions");
            return;
        }
        let mut delete = None;
        ui.horizontal(|ui| {
            if kit::ghost_button(
                ui,
                &icons::label(icons::CHECKS, &format!("Select {}", rows.len())),
            )
            .clicked()
            {
                self.picked = rows.iter().map(|s| s.id.clone()).collect();
            }
            if !self.picked.is_empty() {
                if kit::danger_button(
                    ui,
                    &icons::label(icons::TRASH, &format!("Delete {}", self.picked.len())),
                )
                .clicked()
                {
                    delete = Some((MULTI.to_string(), format!("{} sessions", self.picked.len())));
                }
                if kit::ghost_button(ui, icons::X)
                    .on_hover_text("Clear selection")
                    .clicked()
                {
                    self.picked.clear();
                }
            }
        });
        kit::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            TableBuilder::new(ui)
                .id_salt("audit_sessions")
                .striped(false)
                .resizable(true)
                .sense(egui::Sense::click())
                .column(Column::initial(90.0))
                .column(Column::initial(220.0).clip(true))
                .column(Column::initial(110.0).clip(true))
                .column(Column::initial(160.0).clip(true))
                .column(Column::initial(90.0))
                .column(Column::remainder().at_least(100.0))
                .header(30.0, |mut h| {
                    for t in ["State", "Title", "Node", "Directory", "Entries", ""] {
                        h.col(|ui| {
                            kit::rule(ui);
                            kit::th(ui, t);
                        });
                    }
                })
                .body(|body| {
                    body.rows(32.0, rows.len(), |mut row| {
                        let s = rows[row.index()];
                        row.col(|ui| {
                            kit::rule(ui);
                            let state = if s.is_open() { "live" } else { "ended" };
                            ui.horizontal(|ui| {
                                let mut on = self.picked.contains(&s.id);
                                if ui.checkbox(&mut on, "").changed() {
                                    if on {
                                        self.picked.insert(s.id.clone());
                                    } else {
                                        self.picked.remove(&s.id);
                                    }
                                }
                                kit::dot(ui, kit::status_color(&pal, state));
                                ui.label(
                                    RichText::new(if s.is_open() { "open" } else { "ended" })
                                        .size(12.0)
                                        .color(pal.muted),
                                );
                            });
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            let t = if s.title.is_empty() { &s.id } else { &s.title };
                            ui.add(egui::Label::new(t).truncate()).on_hover_text(&s.id);
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.add(egui::Label::new(node_name(&data.nodes, &s.node_id)).truncate());
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.add(
                                egui::Label::new(RichText::new(&s.cwd).monospace().size(12.0))
                                    .truncate(),
                            );
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.label(RichText::new(s.entry_count.to_string()).monospace());
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            if kit::danger_button(ui, icons::TRASH)
                                .on_hover_text("Delete…")
                                .clicked()
                            {
                                let t = if s.title.is_empty() {
                                    s.id.clone()
                                } else {
                                    s.title.clone()
                                };
                                delete = Some((s.id.clone(), t));
                            }
                        });
                    });
                });
        });
        if let Some(d) = delete {
            self.delete_reason.clear();
            self.confirm_delete = Some(d);
        }
    }

    fn delete_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some((id, title)) = self.confirm_delete.clone() else {
            return;
        };
        let mut open = true;
        let mut done = false;
        egui::Window::new(icons::label(icons::TRASH, "Delete session"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!("Delete \u{201c}{title}\u{201d}?"));
                ui.weak("Ledger keeps a tombstone")
                    .on_hover_text("Transcript, inbox and raw host lines are removed; audit entries stay. A host still running it may report it again.");
                ui.add(
                    egui::TextEdit::singleline(&mut self.delete_reason)
                        .hint_text("Reason (optional)")
                        .desired_width(320.0),
                );
                ui.horizontal(|ui| {
                    if kit::danger_button(ui, &icons::label(icons::TRASH, "Delete")).clicked() {
                        let reason = self.delete_reason.clone();
                        let ids: Vec<String> = if id == MULTI {
                            self.picked.iter().cloned().collect()
                        } else {
                            vec![id.clone()]
                        };
                        self.picked.clear();
                        model.act(move |m| async move {
                            let mut last = 0;
                            for id in &ids {
                                last = m.core().delete_session(id, &reason).await?.seq;
                            }
                            Ok(Some(Notice::Info(format!(
                                "{} deleted; tombstone at seq {last}",
                                ids.len()
                            ))))
                        });
                        done = true;
                    }
                    if kit::ghost_button(ui, "Cancel").clicked() {
                        done = true;
                    }
                });
            });
        if done || !open {
            self.confirm_delete = None;
        }
    }

    fn teleports_ui(&mut self, ui: &mut egui::Ui, _model: &Model) {
        let pal = theme::palette(ui.ctx());
        let data = match self.side.get() {
            Some(Ok(d)) => d,
            Some(Err(e)) => {
                ui.colored_label(pal.red, e);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::primary_button(ui, &icons::label(icons::PLUS, "Teleport")).clicked() {
                    self.new_teleport = Some(NewTeleport::default());
                }
            });
        });
        ui.add_space(8.0);
        if data.teleports.is_empty() {
            ui.weak("No teleports");
            return;
        }
        let now = now_ms();
        let rows = data.teleports.clone();
        kit::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            TableBuilder::new(ui)
                .id_salt("audit_teleports")
                .striped(false)
                .resizable(true)
                .sense(egui::Sense::click())
                .column(Column::initial(100.0))
                .column(Column::initial(200.0).clip(true))
                .column(Column::initial(220.0).clip(true))
                .column(Column::initial(80.0))
                .column(Column::initial(90.0))
                .column(Column::remainder().at_least(120.0).clip(true))
                .header(30.0, |mut h| {
                    for t in [
                        "Status",
                        "Session",
                        "From → to",
                        "Size",
                        "Created",
                        "Expires / error",
                    ] {
                        h.col(|ui| {
                            kit::rule(ui);
                            kit::th(ui, t);
                        });
                    }
                })
                .body(|body| {
                    body.rows(30.0, rows.len(), |mut row| {
                        let t = &rows[row.index()];
                        row.col(|ui| {
                            kit::rule(ui);
                            let c = match t.status.as_str() {
                                "ready" => pal.cyan,
                                "delivered" => pal.green,
                                "failed" => pal.red,
                                "expired" => pal.muted,
                                _ => pal.amber,
                            };
                            kit::chip(ui, &t.status, c);
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            let label = if t.title.is_empty() {
                                t.session_id.clone()
                            } else {
                                t.title.clone()
                            };
                            ui.add(egui::Label::new(label).truncate())
                                .on_hover_text(format!(
                                    "teleport {}\nsession {}",
                                    t.id, t.session_id
                                ));
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.add(
                                egui::Label::new(format!(
                                    "{} → {}",
                                    node_name(&data.nodes, &t.from_node),
                                    node_name(&data.nodes, &t.to_node)
                                ))
                                .truncate(),
                            );
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.label(RichText::new(size(t.size)).monospace());
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.label(RichText::new(fmt::ago(now, t.created_at)).color(pal.muted));
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            if !t.error.is_empty() {
                                ui.colored_label(pal.red, &t.error);
                            } else if t.expires_at > 0 {
                                ui.label(
                                    RichText::new(fmt::when(t.expires_at))
                                        .monospace()
                                        .color(pal.muted),
                                );
                            }
                        });
                    });
                });
        });
    }

    fn teleport_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(n) = self.new_teleport.as_mut() else {
            return;
        };
        let (nodes, sessions) = match self.side.get() {
            Some(Ok(d)) => (d.nodes.clone(), d.sessions.clone()),
            _ => (Arc::default(), Arc::default()),
        };
        let live: Vec<&Node> = nodes.iter().filter(|x| x.revoked_at.is_none()).collect();
        let mut open = true;
        let mut done = false;
        egui::Window::new("Request teleport")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("teleport_form")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Session");
                        let label = sessions
                            .iter()
                            .find(|s| s.id == n.session_id)
                            .map(|s| {
                                if s.title.is_empty() {
                                    s.id.clone()
                                } else {
                                    s.title.clone()
                                }
                            })
                            .unwrap_or_else(|| "Pick a session".into());
                        egui::ComboBox::from_id_salt("tp_session")
                            .selected_text(label)
                            .width(280.0)
                            .show_ui(ui, |ui| {
                                for s in sessions.iter().take(200) {
                                    let t = if s.title.is_empty() { &s.id } else { &s.title };
                                    if ui.selectable_label(n.session_id == s.id, t).clicked() {
                                        n.session_id = s.id.clone();
                                        n.from = s.node_id.clone();
                                    }
                                }
                            });
                        ui.end_row();
                        for (label, slot, salt) in
                            [("From", &mut n.from, "tp_from"), ("To", &mut n.to, "tp_to")]
                        {
                            ui.label(label);
                            let shown = live
                                .iter()
                                .find(|x| x.id == *slot)
                                .map(|x| x.name.clone())
                                .unwrap_or_else(|| "Pick a node".into());
                            egui::ComboBox::from_id_salt(salt)
                                .selected_text(shown)
                                .width(280.0)
                                .show_ui(ui, |ui| {
                                    for x in &live {
                                        ui.selectable_value(slot, x.id.clone(), &x.name);
                                    }
                                });
                            ui.end_row();
                        }
                    });
                let ok = !n.session_id.is_empty()
                    && !n.from.is_empty()
                    && !n.to.is_empty()
                    && n.from != n.to;
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled_ui(ok, |ui| {
                            theme::primary_button(ui, &icons::label(icons::BROADCAST, "Request"))
                        })
                        .inner
                        .clicked()
                    {
                        let (s, f, t) = (n.session_id.clone(), n.from.clone(), n.to.clone());
                        model.act(move |m| async move {
                            m.core().teleport_request(&s, &f, &t).await?;
                            Ok(Some(Notice::Info("Teleport requested".into())))
                        });
                        done = true;
                    }
                    if kit::ghost_button(ui, "Cancel").clicked() {
                        done = true;
                    }
                });
            });
        if done || !open {
            self.new_teleport = None;
        }
    }
}

fn entry_detail(
    ui: &mut egui::Ui,
    e: &LedgerEntry,
    selected: &mut Option<i64>,
) -> Option<(crate::Screen, Option<String>)> {
    let mut nav = None;
    let pal = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(RichText::new(icons::HASH).color(pal.gold_text));
        ui.label(RichText::new(format!("{} · {}", e.seq, e.kind)).strong());
        kit::chip(ui, &e.origin, pal.muted);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if kit::ghost_button(ui, icons::X)
                .on_hover_text("Close")
                .clicked()
            {
                *selected = None;
            }
            if kit::ghost_button(ui, icons::COPY)
                .on_hover_text("Copy JSON")
                .clicked()
            {
                ui.ctx()
                    .copy_text(serde_json::to_string_pretty(e).unwrap_or_default());
            }
        });
    });
    ui.add_space(6.0);
    let links = entry_links(e);
    if !links.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for (screen, focus, label) in links {
                if kit::ghost_button(ui, &icons::label(screen.icon(), &label)).clicked() {
                    nav = Some((screen, Some(focus)));
                }
            }
        });
        ui.add_space(4.0);
    }
    egui::ScrollArea::vertical()
        .id_salt("audit_detail")
        .max_height(170.0)
        .show(ui, |ui| {
            ui.label(&e.msg);
            ui.add_space(4.0);
            for (k, v) in [("hash", &e.hash), ("prev", &e.prev_hash)] {
                ui.horizontal(|ui| {
                    ui.label(kit::caps(k, pal.muted));
                    ui.label(RichText::new(v).monospace().size(11.0));
                });
            }
            ui.label(
                RichText::new(format!(
                    "subject {}\npayload {}",
                    e.subject,
                    serde_json::to_string_pretty(&e.payload).unwrap_or_default()
                ))
                .monospace()
                .size(11.0)
                .color(pal.muted),
            );
        });
    nav
}

/// Where an entry's subject and payload point: card, run, session,
/// workflow, repo, node, budget, key, and the events around it.
fn entry_links(e: &LedgerEntry) -> Vec<(crate::Screen, String, String)> {
    use crate::Screen as S;
    let pick = |keys: &[&str]| -> Option<String> {
        let from = |v: &serde_json::Value| {
            let v = v.get("data").unwrap_or(v);
            keys.iter()
                .find_map(|k| v.get(*k).and_then(|x| x.as_str()))
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        from(&e.subject).or_else(|| from(&e.payload))
    };
    let mut out = Vec::new();
    let mut add = |s: S, focus: Option<String>, label: &str, filter: &str| {
        if let Some(f) = focus {
            out.push((
                s,
                f.clone(),
                format!("{label} {}", f.chars().take(14).collect::<String>()),
            ));
            if !filter.is_empty() {
                out.push((S::Events, format!("{filter}:{f}"), "Events".into()));
            }
        }
    };
    add(
        S::Board,
        pick(&["card", "cardId", "card_id", "key"]),
        "Card",
        "card",
    );
    add(S::Runs, pick(&["run", "runId", "run_id"]), "Run", "run");
    let session = Some(e.session_id.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| pick(&["session", "sessionId"]));
    add(S::Sessions, session, "Session", "session");
    add(
        S::Workflows,
        pick(&["workflow", "workflowId"]),
        "Workflow",
        "wf",
    );
    add(S::Repos, pick(&["repo"]), "Repo", "repo");
    if e.kind == "budget" {
        add(S::Budgets, pick(&["budgetId", "id"]), "Budget", "");
    }
    if e.kind == "vault" {
        add(S::Vault, pick(&["id", "secretId"]), "Key", "");
    }
    // Events at most once.
    let mut seen_events = false;
    out.retain(|(s, _, _)| {
        if *s == S::Events {
            if seen_events {
                return false;
            }
            seen_events = true;
        }
        true
    });
    out
}

/// Kind colour by family: refusals red, permissions gold, vault violet,
/// git green, everything else cyan.
fn kind_color(p: &Palette, kind: &str) -> Color32 {
    let k = kind.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| k.contains(w));
    if has(&["refus", "denied", "blocked", "fail", "revoke", "deleted"]) {
        p.red
    } else if has(&["permission", "hazard", "approv", "checkpoint"]) {
        p.gold_text
    } else if has(&["vault", "secret", "grant"]) {
        p.violet
    } else if has(&["commit", "merge", "git", "pr."]) {
        p.green
    } else {
        p.cyan
    }
}

/// A hash shortened to its first and last four characters.
fn chain_hash(h: &str) -> String {
    let h = h.trim_start_matches("sha256:");
    let n = h.chars().count();
    if h.is_empty() {
        "—".into()
    } else if n <= 10 {
        h.to_string()
    } else {
        let head: String = h.chars().take(4).collect();
        let tail: String = h.chars().skip(n - 4).collect();
        format!("{head}…{tail}")
    }
}

fn short_hash(h: &str) -> String {
    let h = h.trim_start_matches("sha256:");
    if h.is_empty() {
        "—".into()
    } else {
        h.chars().take(10).collect()
    }
}

fn node_name(nodes: &[Node], id: &str) -> String {
    nodes
        .iter()
        .find(|n| n.id == id)
        .map(|n| n.name.clone())
        .unwrap_or_else(|| id.chars().take(8).collect())
}

fn size(n: i64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MiB", n as f64 / (1 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KiB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_links_point_at_what_the_entry_names() {
        let e = LedgerEntry {
            kind: "card".into(),
            subject: serde_json::json!({"data": {"cardId": "K-1", "runId": "r9"}}),
            session_id: "s1".into(),
            ..Default::default()
        };
        let l = entry_links(&e);
        let screens: Vec<crate::Screen> = l.iter().map(|x| x.0).collect();
        assert!(screens.contains(&crate::Screen::Board));
        assert!(screens.contains(&crate::Screen::Runs));
        assert!(screens.contains(&crate::Screen::Sessions));
        assert_eq!(
            screens
                .iter()
                .filter(|s| **s == crate::Screen::Events)
                .count(),
            1
        );
        assert!(l.iter().any(|x| x.1 == "card:K-1"));
    }

    #[test]
    fn chain_hash_keeps_both_ends() {
        assert_eq!(chain_hash("sha256:9c41aaaaaaaae07a"), "9c41…e07a");
        assert_eq!(chain_hash("abc"), "abc");
        assert_eq!(chain_hash(""), "—");
    }
}

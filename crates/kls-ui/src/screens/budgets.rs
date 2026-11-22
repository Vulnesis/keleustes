//! Budgets screen. Owner: governance workstream.
//!
//! Budgets with their spend (the gate in `kls_core::vault::budget_gate`
//! enforces them), routing rules and default models, usage by
//! provider/model/agent/repo, host gauges, and Value: the human-equivalent
//! estimate of agent work and performance per agent. Provider keys live on
//! the Vault screen.

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme;
use egui_extras::{Column, TableBuilder};
use kls_core::estimates::{Estimate, PerfRow};
use kls_core::vault::{
    self as v, BUDGET_ACTIONS, BUDGET_PERIODS, BUDGET_SCOPES, Budget, BudgetStatus, BudgetsView,
    RoutingRule, UsageTotal,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

pub const READY: bool = true;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Budgets,
    Value,
    Routing,
    Usage,
}

struct BudgetForm {
    b: Budget,
    tokens: String,
    usd: String,
    alerts: String,
}

impl BudgetForm {
    fn new(b: Budget) -> Self {
        Self {
            tokens: b.limit_tokens.map(|t| t.to_string()).unwrap_or_default(),
            usd: b.limit_usd.map(|u| u.to_string()).unwrap_or_default(),
            alerts: b
                .alert_at
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            b,
        }
    }
}

enum Confirm {
    Budget(String, String),
    Rule(String),
}

pub struct Screen {
    data: Loader<BudgetsView>,
    /// GitHub calls this hour per caller (people, the poller, each workflow).
    forge: Loader<Vec<kls_github::budget::CallerCalls>>,
    range_days: Arc<AtomicI64>,
    tab: Tab,
    budget_form: Option<BudgetForm>,
    rule_form: Option<RoutingRule>,
    confirm: Option<Confirm>,
    default_model: Option<(String, String)>,
    /// A focus that belongs on the Vault screen (old `key:` links).
    to_vault: Option<String>,
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        let range_days = Arc::new(AtomicI64::new(7));
        let r = range_days.clone();
        let data = Loader::new(
            model,
            &[
                "budget.",
                "vault.",
                "session.",
                "node.",
                "settings.",
                "run.finished",
            ],
            move |core| {
                let days = r.load(Ordering::Relaxed);
                async move { v::budgets_view(&core, days).await }
            },
        );
        Self {
            data,
            forge: Loader::new(
                model,
                &["github.quota", "forge.", "run.finished"],
                |core| async move {
                    kls_github::budget::calls_this_hour(core.store(), kls_store::now_ms()).await
                },
            ),
            range_days,
            tab: Tab::default(),
            budget_form: None,
            rule_form: None,
            confirm: None,
            default_model: None,
            to_vault: None,
        }
    }

    /// `value`, `routing`, `usage`, a budget id; `providers` and
    /// `key:<provider>` open the Vault.
    pub fn focus(&mut self, id: &str) {
        match id {
            "value" => self.tab = Tab::Value,
            "routing" => self.tab = Tab::Routing,
            "usage" => self.tab = Tab::Usage,
            "providers" => self.to_vault = Some(String::new()),
            id if id.starts_with("key:") => self.to_vault = Some(id.replacen("key:", "add:", 1)),
            _ => self.tab = Tab::Budgets,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        if let Some(f) = self.to_vault.take() {
            model.go(crate::Screen::Vault, (!f.is_empty()).then_some(f));
        }
        let p = theme::palette(ui.ctx());
        kit::page_header(ui, icons::WALLET, "Budgets", |ui| {
            if kit::ghost_button(ui, icons::ARROWS_CLOCKWISE)
                .on_hover_text("Reload")
                .clicked()
            {
                self.data.reload();
            }
            if kit::ghost_button(ui, &icons::label(icons::LOCK_SIMPLE, "Vault")).clicked() {
                model.go(crate::Screen::Vault, None);
            }
            ui.add_space(8.0);
            for (t, icon, label) in [
                (Tab::Usage, icons::CHART_BAR, "Usage"),
                (Tab::Routing, icons::FLOW_ARROW, "Routing"),
                (Tab::Value, icons::TREND_UP, "Value"),
                (Tab::Budgets, icons::WALLET, "Budgets"),
            ] {
                if kit::tab(ui, &icons::label(icon, label), self.tab == t).clicked() {
                    self.tab = t;
                }
            }
        });
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
        if let Some(e) = &view.vault_error {
            kit::chip(ui, &format!("{}  Vault: {e}", icons::X_CIRCLE), p.red);
            ui.add_space(6.0);
        }
        egui::ScrollArea::vertical()
            .id_salt("budgets_page")
            .auto_shrink([false, false])
            .show(ui, |ui| match self.tab {
                Tab::Budgets => self.overview(ui, &view),
                Tab::Value => self.value(ui, model, &view),
                Tab::Routing => self.routing(ui, model, &view),
                Tab::Usage => self.usage(ui, &view),
            });
        let ctx = ui.ctx().clone();
        self.budget_dialog(&ctx, model, &view);
        self.rule_dialog(&ctx, model, &view);
        self.confirm_dialog(&ctx, model);
    }

    // ---- overview: budgets, routing ----------------------------------------

    fn overview(&mut self, ui: &mut egui::Ui, view: &BudgetsView) {
        let p = theme::palette(ui.ctx());
        ui.horizontal(|ui| {
            ui.label(kit::caps(&icons::label(icons::WALLET, "Budgets"), p.muted))
                .on_hover_text("Tightest budget wins. A stopping action holds new model work.");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if kit::ghost_button(ui, &icons::label(icons::PLUS, "New budget")).clicked() {
                    self.budget_form = Some(BudgetForm::new(Budget {
                        applies_to: "all".into(),
                        provider: "all".into(),
                        period: "month".into(),
                        alert_at: vec![80, 100],
                        // Empty takes budget.on_exceed when saved.
                        action_at_limit: String::new(),
                        ..Default::default()
                    }));
                }
            });
        });
        ui.add_space(6.0);
        if view.budgets.is_empty() {
            kit::card(ui)
                .inner_margin(egui::Margin::same(16))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        egui::RichText::new(icons::label(icons::WALLET, "No budgets"))
                            .color(p.muted),
                    )
                    .on_hover_text("Without one, model work is never limited");
                });
        } else {
            let mut act: Option<(bool, Budget)> = None;
            kit::Grid::new(260.0)
                .tall()
                .show(ui, view.budgets.len(), |ui, i, _| {
                    if let Some(a) = budget_tile(ui, &view.budgets[i]) {
                        act = Some(a);
                    }
                });
            match act {
                Some((true, b)) => self.confirm = Some(Confirm::Budget(b.id.clone(), b.name)),
                Some((false, b)) => self.budget_form = Some(BudgetForm::new(b)),
                None => {}
            }
        }
        if view.budgets.len() > 1 {
            ui.add_space(16.0);
            nesting(ui, &view.budgets);
        }
        ui.add_space(16.0);
        kit::Split::new(460.0, 380.0, kit::Widths::Second(0.45)).show(ui, |ui, side| {
            if side == 0 {
                estimate_panel(ui, &view.estimate, self.range_days.load(Ordering::Relaxed));
            } else {
                self.rules_panel(ui, view);
            }
        });
    }

    // ---- value: estimates and performance ------------------------------------

    fn value(&mut self, ui: &mut egui::Ui, model: &Model, view: &BudgetsView) {
        let p = theme::palette(ui.ctx());
        ui.horizontal(|ui| {
            for d in [7, 30, 90] {
                if kit::pill(
                    ui,
                    &format!("{d} d"),
                    self.range_days.load(Ordering::Relaxed) == d,
                )
                .clicked()
                {
                    self.range_days.store(d, Ordering::Relaxed);
                    self.data.reload();
                }
            }
            if kit::ghost_button(ui, &icons::label(icons::SLIDERS, "Rates")).clicked() {
                model.go(crate::Screen::Settings, Some("estimates".into()));
            }
        });
        ui.add_space(8.0);
        estimate_panel(ui, &view.estimate, self.range_days.load(Ordering::Relaxed));
        ui.add_space(16.0);
        ui.label(kit::caps(
            &icons::label(icons::USERS_THREE, "Performance by agent"),
            p.muted,
        ));
        ui.add_space(4.0);
        perf_table(ui, &view.performance);
    }

    /// Routing rules in order, then the default model.
    fn rules_panel(&mut self, ui: &mut egui::Ui, view: &BudgetsView) {
        let p = theme::palette(ui.ctx());
        let mut edit: Option<RoutingRule> = None;
        let mut new_rule = false;
        kit::section(
            ui,
            icons::FLOW_ARROW,
            "Routing rules",
            false,
            |ui| {
                new_rule = kit::ghost_button(ui, &icons::label(icons::PLUS, "New rule"))
                    .on_hover_text(
                        "First matching enabled rule wins, then the agent's model, then the default.",
                    )
                    .clicked();
            },
            |ui| {
                for (i, r) in view.rules.iter().enumerate() {
                    if i > 0 {
                        kit::hline_full(ui, p.line);
                    }
                    let resp = rule_row(
                        ui,
                        &(i + 1).to_string(),
                        &rule_when(r),
                        &route_to(r),
                        r.enabled,
                    )
                    .on_hover_text(format!(
                        "Priority {}{} · click to edit",
                        r.priority,
                        if r.enabled { "" } else { " · disabled" }
                    ));
                    if resp.clicked() {
                        edit = Some(r.clone());
                    }
                }
                if !view.rules.is_empty() {
                    kit::hline_full(ui, p.line);
                }
                let n = (view.rules.len() + 1).to_string();
                rule_row(
                    ui,
                    &n,
                    "Everything else",
                    or_dash(&view.default_model),
                    true,
                )
                .on_hover_text("Default model");
            },
        );
        if new_rule {
            self.rule_form = Some(RoutingRule {
                priority: (view.rules.iter().map(|r| r.priority).max().unwrap_or(0) + 10),
                enabled: true,
                ..Default::default()
            });
        }
        if let Some(r) = edit {
            self.rule_form = Some(r);
        }
    }

    // ---- routing -----------------------------------------------------------------

    fn routing(&mut self, ui: &mut egui::Ui, model: &Model, view: &BudgetsView) {
        let p = theme::palette(ui.ctx());
        let (dm, fm) = self
            .default_model
            .get_or_insert_with(|| (view.default_model.clone(), view.fast_model.clone()));
        kit::section(
            ui,
            icons::CPU,
            "Default models",
            false,
            |_| {},
            |ui| {
                egui::Grid::new("defaults")
                    .num_columns(3)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label(icons::label(icons::CPU, "Default model"));
                        ui.add(egui::TextEdit::singleline(dm).hint_text("provider/model"));
                        if kit::ghost_button(ui, &icons::label(icons::FLOPPY_DISK, "Save"))
                            .clicked()
                        {
                            let val = dm.clone();
                            model.act(move |m| async move {
                                v::set_default_model(m.core(), v::settings::MODELS_DEFAULT, &val)
                                    .await?;
                                Ok(Some(Notice::Info("Default model saved".into())))
                            });
                        }
                        ui.end_row();
                        ui.label(icons::label(icons::LIGHTNING, "Fast model"));
                        ui.add(egui::TextEdit::singleline(fm).hint_text("provider/model"));
                        if kit::ghost_button(ui, &icons::label(icons::FLOPPY_DISK, "Save"))
                            .clicked()
                        {
                            let val = fm.clone();
                            model.act(move |m| async move {
                                v::set_default_model(m.core(), v::settings::MODELS_FAST, &val)
                                    .await?;
                                Ok(Some(Notice::Info("Fast model saved".into())))
                            });
                        }
                        ui.end_row();
                    });
            },
        );
        ui.add_space(14.0);
        let mut new_rule = false;
        let mut act: Option<(bool, RoutingRule)> = None;
        kit::section(
            ui,
            icons::FLOW_ARROW,
            "Rules",
            false,
            |ui| {
                new_rule = theme::primary_button(ui, &icons::label(icons::PLUS, "New rule"))
                    .on_hover_text(
                        "First matching enabled rule wins, then the agent's model, then the default.",
                    )
                    .clicked();
            },
            |ui| {
                if view.rules.is_empty() {
                    ui.label(
                        egui::RichText::new(icons::label(icons::FLOW_ARROW, "No rules"))
                            .color(p.muted),
                    );
                    return;
                }
                let mut lines = Vec::new();
                TableBuilder::new(ui)
                    .id_salt("rules")
                    .striped(false)
                    .vscroll(false)
                    .sense(egui::Sense::click())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::auto().at_least(60.0))
                    .column(Column::remainder().at_least(220.0))
                    .column(Column::auto().at_least(180.0))
                    .column(Column::auto().at_least(50.0))
                    .column(Column::auto())
                    .header(30.0, |mut h| {
                        for t in ["Priority", "When", "Use", "On", ""] {
                            lines.push(h.col(|ui| kit::th(ui, t)).0);
                        }
                    })
                    .body(|mut body| {
                        for r in &view.rules {
                            body.row(30.0, |mut row| {
                                row.col(|ui| {
                                    ui.label(
                                        egui::RichText::new(r.priority.to_string())
                                            .monospace()
                                            .color(p.muted),
                                    );
                                });
                                row.col(|ui| {
                                    ui.label(rule_when(r));
                                });
                                row.col(|ui| {
                                    ui.label(
                                        egui::RichText::new(route_to(r)).monospace().color(p.cyan),
                                    );
                                });
                                row.col(|ui| {
                                    if r.enabled {
                                        ui.colored_label(p.green, icons::CHECK)
                                            .on_hover_text("Enabled");
                                    } else {
                                        ui.colored_label(p.muted, icons::PAUSE)
                                            .on_hover_text("Disabled");
                                    }
                                });
                                row.col(|ui| {
                                    ui.menu_button(icons::DOTS_THREE, |ui| {
                                        if ui
                                            .button(icons::label(icons::PENCIL_SIMPLE, "Edit"))
                                            .clicked()
                                        {
                                            act = Some((false, r.clone()));
                                            ui.close();
                                        }
                                        if ui.button(icons::label(icons::TRASH, "Delete")).clicked()
                                        {
                                            act = Some((true, r.clone()));
                                            ui.close();
                                        }
                                    })
                                    .response
                                    .on_hover_text("More");
                                });
                                let resp = row.response();
                                lines.push(resp.rect);
                                if resp.double_clicked() {
                                    act = Some((false, r.clone()));
                                }
                            });
                        }
                    });
                kit::row_lines(ui, &lines, p.line);
            },
        );
        if new_rule {
            self.rule_form = Some(RoutingRule {
                priority: (view.rules.iter().map(|r| r.priority).max().unwrap_or(0) + 10),
                enabled: true,
                ..Default::default()
            });
        }
        match act {
            Some((true, r)) => self.confirm = Some(Confirm::Rule(r.id)),
            Some((false, r)) => self.rule_form = Some(r),
            None => {}
        }
    }

    // ---- usage -------------------------------------------------------------------

    fn usage(&mut self, ui: &mut egui::Ui, view: &BudgetsView) {
        let p = theme::palette(ui.ctx());
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(icons::CALENDAR).color(p.muted))
                .on_hover_text("Range");
            let cur = self.range_days.load(Ordering::Relaxed);
            for d in [7, 30, 90] {
                if kit::pick_chip(ui, &format!("{d}d"), cur == d, p.muted).clicked() && cur != d {
                    self.range_days.store(d, Ordering::Relaxed);
                    self.data.reload();
                }
            }
        });
        ui.add_space(10.0);
        let total: i64 = view.by_provider.iter().map(|u| u.tokens).sum();
        let cost: f64 = view.by_provider.iter().map(|u| u.cost_usd).sum();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
            kit::kpi(ui, icons::COINS, "Tokens", &tokens(total), None, None);
            kit::kpi(
                ui,
                icons::CURRENCY_DOLLAR,
                "Cost",
                &format!("${cost:.2}"),
                Some("estimated"),
                None,
            );
            kit::kpi(
                ui,
                icons::HARD_DRIVES,
                "Hosts",
                &view.hosts.len().to_string(),
                Some("7 days"),
                None,
            );
        });
        ui.add_space(14.0);
        let groups: [(&str, &str, &[UsageTotal]); 4] = [
            (icons::PLUGS_CONNECTED, "Provider", &view.by_provider),
            (icons::CPU, "Model", &view.by_model),
            (icons::ROBOT, "Agent", &view.by_agent),
            (icons::GIT_BRANCH, "Repository", &view.by_repo),
        ];
        kit::Grid::new(240.0).tall().show(ui, 4, |ui, i, _| {
            let (icon, title, rows) = groups[i];
            kit::section(ui, icon, title, false, |_| {}, |ui| totals(ui, rows));
        });
        ui.add_space(14.0);
        let forge = self.forge.get().and_then(Result::ok).unwrap_or_default();
        kit::Split::new(360.0, 360.0, kit::Widths::Halves).show(ui, |ui, side| {
            if side == 0 {
                kit::section(
                    ui,
                    icons::WRENCH,
                    "Tool calls",
                    false,
                    |_| {},
                    |ui| {
                        bars(
                            ui,
                            &view
                                .tool_calls
                                .iter()
                                .map(|t| {
                                    (
                                        t.key.clone(),
                                        t.calls,
                                        format!("{} · {} sessions", t.calls, t.sessions),
                                    )
                                })
                                .collect::<Vec<_>>(),
                            p.cyan,
                        )
                    },
                );
            } else {
                kit::section(
                    ui,
                    icons::GITHUB_LOGO,
                    "GitHub calls · this hour",
                    false,
                    |_| {},
                    |ui| {
                        bars(
                            ui,
                            &forge
                                .iter()
                                .map(|c| {
                                    let who = if !c.workflow.is_empty() {
                                        icons::label(icons::TREE_STRUCTURE, &c.workflow)
                                    } else if c.caller == "user" {
                                        icons::label(icons::USER, "You")
                                    } else {
                                        icons::label(icons::ARROWS_CLOCKWISE, &c.caller)
                                    };
                                    (
                                        who,
                                        c.calls,
                                        format!("{} · {} cached", c.calls, c.not_modified),
                                    )
                                })
                                .collect::<Vec<_>>(),
                            p.violet,
                        )
                    },
                );
            }
        });
        ui.add_space(14.0);
        kit::section(
            ui,
            icons::HARD_DRIVES,
            "Hosts · 7d",
            false,
            |_| {},
            |ui| {
                if view.hosts.is_empty() {
                    ui.label(
                        egui::RichText::new(icons::label(icons::HARD_DRIVES, "No nodes"))
                            .color(p.muted),
                    );
                }
                for h in &view.hosts {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [140.0, 18.0],
                            egui::Label::new(egui::RichText::new(&h.name).strong()).truncate(),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "{:.0}% · {} tok · ${:.2}",
                                h.pct,
                                tokens(h.tokens_7d),
                                h.cost_7d
                            ))
                            .monospace()
                            .size(12.0),
                        );
                        ui.label(
                            egui::RichText::new(
                                h.by_provider
                                    .iter()
                                    .map(|u| format!("{} {}", or_dash(&u.key), tokens(u.tokens)))
                                    .collect::<Vec<_>>()
                                    .join(" · "),
                            )
                            .size(12.0)
                            .color(p.muted),
                        );
                    });
                    kit::meter(ui, (h.pct / 100.0) as f32, p.cyan);
                    ui.add_space(6.0);
                }
            },
        );
        ui.add_space(14.0);
        kit::section(
            ui,
            icons::CLOCK,
            "Recent",
            false,
            |_| {},
            |ui| {
                if view.usage.is_empty() {
                    ui.label(
                        egui::RichText::new(icons::label(icons::CHART_BAR, "No usage"))
                            .color(p.muted),
                    );
                    return;
                }
                let mut lines = Vec::new();
                egui::ScrollArea::horizontal()
                    .id_salt("usage_rows_h")
                    .show(ui, |ui| {
                        TableBuilder::new(ui)
                            .id_salt("usage_rows")
                            .striped(false)
                            .vscroll(false)
                            .sense(egui::Sense::click())
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(Column::auto().at_least(140.0))
                            .column(Column::auto().at_least(60.0))
                            .column(Column::auto().at_least(160.0))
                            .column(Column::auto().at_least(90.0))
                            .column(Column::auto().at_least(120.0))
                            .column(Column::auto().at_least(80.0))
                            .column(Column::remainder().at_least(60.0))
                            .header(30.0, |mut h| {
                                for t in [
                                    "When",
                                    "Source",
                                    "Model",
                                    "Agent",
                                    "Repository",
                                    "Tokens",
                                    "Cost",
                                ] {
                                    lines.push(h.col(|ui| kit::th(ui, t)).0);
                                }
                            })
                            .body(|mut body| {
                                for u in &view.usage {
                                    body.row(28.0, |mut row| {
                                        row.col(|ui| {
                                            ui.label(
                                                egui::RichText::new(fmt::when(u.at)).color(p.muted),
                                            );
                                        });
                                        row.col(|ui| {
                                            ui.label(&u.source);
                                        });
                                        row.col(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "{}/{}",
                                                    or_dash(&u.provider),
                                                    or_dash(&u.model)
                                                ))
                                                .monospace()
                                                .color(p.cyan),
                                            );
                                        });
                                        row.col(|ui| {
                                            ui.label(or_dash(&u.agent));
                                        });
                                        row.col(|ui| {
                                            ui.label(or_dash(&u.repo));
                                        });
                                        row.col(|ui| {
                                            ui.label(
                                                egui::RichText::new(tokens(
                                                    u.input_tokens + u.output_tokens,
                                                ))
                                                .monospace(),
                                            );
                                        });
                                        row.col(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!("${:.3}", u.cost_usd))
                                                    .monospace(),
                                            );
                                        });
                                        lines.push(row.response().rect);
                                    });
                                }
                            });
                    });
                kit::row_lines(ui, &lines, p.line);
            },
        );
    }

    // ---- confirm -------------------------------------------------------------------

    fn confirm_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(c) = &self.confirm else {
            return;
        };
        let text = match c {
            Confirm::Budget(_, name) => format!("Delete budget {name}?"),
            Confirm::Rule(_) => "Delete rule?".to_string(),
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
                            let core = m.core();
                            match c {
                                Some(Confirm::Budget(id, _)) => {
                                    v::delete_budget(core, &id).await?;
                                }
                                Some(Confirm::Rule(id)) => {
                                    v::delete_rule(core, &id).await?;
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

    fn budget_dialog(&mut self, ctx: &egui::Context, model: &Model, view: &BudgetsView) {
        let Some(form) = &mut self.budget_form else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let title = if form.b.id.is_empty() {
            icons::label(icons::PLUS, "New budget")
        } else {
            icons::label(icons::PENCIL_SIMPLE, "Edit budget")
        };
        egui::Window::new(title)
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("budget_form")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Name");
                        ui.text_edit_singleline(&mut form.b.name);
                        ui.end_row();
                        ui.label("Scope");
                        combo(ui, "scope", &mut form.b.applies_to, &BUDGET_SCOPES);
                        ui.end_row();
                        if form.b.applies_to == "org" {
                            ui.label("");
                            ui.label(
                                egui::RichText::new(icons::label(
                                    icons::BUILDINGS,
                                    "The whole factory, every provider",
                                ))
                                .size(12.0)
                                .color(theme::palette(ui.ctx()).muted),
                            );
                            ui.end_row();
                        }
                        if form.b.applies_to != "all" && form.b.applies_to != "org" {
                            let (label, hint) = match form.b.applies_to.as_str() {
                                "provider" => ("Provider", ""),
                                "model" => ("Model", "provider/model"),
                                "agent" => ("Agent", ""),
                                "workflow" => ("Workflow", "workflow id"),
                                "node" => ("Node", "node id"),
                                _ => ("Repository", "owner/name or owner/*"),
                            };
                            ui.label(label);
                            ui.add(egui::TextEdit::singleline(&mut form.b.subject).hint_text(hint));
                            ui.end_row();
                        }
                        ui.label("Providers");
                        ui.add(
                            egui::TextEdit::singleline(&mut form.b.provider)
                                .hint_text("all, or a, b"),
                        );
                        ui.end_row();
                        ui.label("Period");
                        combo(ui, "period", &mut form.b.period, &BUDGET_PERIODS);
                        ui.end_row();
                        ui.label("Token limit");
                        ui.add(egui::TextEdit::singleline(&mut form.tokens).hint_text("none"));
                        ui.end_row();
                        ui.label("USD limit");
                        ui.add(egui::TextEdit::singleline(&mut form.usd).hint_text("none"));
                        ui.end_row();
                        ui.label("Alert at %");
                        ui.text_edit_singleline(&mut form.alerts);
                        ui.end_row();
                        ui.label("At limit");
                        combo(ui, "action", &mut form.b.action_at_limit, &BUDGET_ACTIONS);
                        ui.end_row();
                    });
                if !view.providers.is_empty() {
                    ui.weak(format!(
                        "{}  {}",
                        icons::PLUGS_CONNECTED,
                        view.providers
                            .iter()
                            .map(|p| p.id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if crate::theme::primary_button(ui, &icons::label(icons::FLOPPY_DISK, "Save"))
                    .clicked()
                {
                    save = true;
                }
            });
        if save {
            let mut b = form.b.clone();
            b.limit_tokens = form.tokens.trim().replace([',', '_'], "").parse().ok();
            b.limit_usd = form.usd.trim().trim_start_matches('$').parse().ok();
            b.alert_at = form
                .alerts
                .split([',', ' '])
                .filter_map(|s| s.trim().trim_end_matches('%').parse().ok())
                .collect();
            model.act(move |m| async move {
                v::save_budget(m.core(), &b).await?;
                Ok(Some(Notice::Info("Budget saved".into())))
            });
            open = false;
        }
        if !open {
            self.budget_form = None;
        }
    }

    fn rule_dialog(&mut self, ctx: &egui::Context, model: &Model, view: &BudgetsView) {
        let Some(r) = &mut self.rule_form else {
            return;
        };
        let mut open = true;
        let mut save = false;
        egui::Window::new(icons::label(icons::FLOW_ARROW, "Routing rule"))
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("rule_form").num_columns(2).show(ui, |ui| {
                    ui.label("Priority");
                    ui.add(egui::DragValue::new(&mut r.priority));
                    ui.end_row();
                    ui.label("Repository");
                    ui.add(
                        egui::TextEdit::singleline(&mut r.match_repo)
                            .hint_text("owner/name or owner/*"),
                    );
                    ui.end_row();
                    ui.label("Agent");
                    ui.text_edit_singleline(&mut r.match_agent);
                    ui.end_row();
                    ui.label("Card label");
                    ui.text_edit_singleline(&mut r.match_label);
                    ui.end_row();
                    ui.label("Kind");
                    ui.add(egui::TextEdit::singleline(&mut r.match_kind).hint_text("code_smart"))
                        .on_hover_text("code_smart, code_fast, planner_reviewer, images");
                    ui.end_row();
                    ui.label("Provider");
                    egui::ComboBox::from_id_salt("rule_provider")
                        .selected_text(or_dash(&r.provider))
                        .show_ui(ui, |ui| {
                            for p in &view.providers {
                                ui.selectable_value(&mut r.provider, p.id.clone(), &p.name);
                            }
                        });
                    ui.end_row();
                    ui.label("Model");
                    ui.add(egui::TextEdit::singleline(&mut r.model).hint_text("provider default"));
                    ui.end_row();
                    ui.label("Enabled");
                    ui.checkbox(&mut r.enabled, "");
                    ui.end_row();
                });
                if crate::theme::primary_button(ui, &icons::label(icons::FLOPPY_DISK, "Save"))
                    .clicked()
                {
                    save = true;
                }
            });
        if save {
            let r = r.clone();
            model.act(move |m| async move {
                v::save_rule(m.core(), &r).await?;
                Ok(None)
            });
            open = false;
        }
        if !open {
            self.rule_form = None;
        }
    }
}

/// One budget: name and period, spend against the limit, a meter and the
/// alert/limit note. Returns `(delete?, budget)` when its menu was used.
fn budget_tile(ui: &mut egui::Ui, st: &BudgetStatus) -> Option<(bool, Budget)> {
    let p = theme::palette(ui.ctx());
    let b = &st.budget;
    let first_alert = b
        .alert_at
        .iter()
        .copied()
        .filter(|a| *a > 0)
        .min()
        .unwrap_or(80) as f64;
    let (edge, warn) = if st.pct >= 100.0 {
        (p.red, true)
    } else if st.pct >= first_alert {
        (p.gold_edge, true)
    } else {
        (p.line, false)
    };
    let mut act = None;
    kit::card(ui)
        .stroke(egui::Stroke::new(1.0, edge))
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(&b.name).size(13.0).strong()).truncate(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button(icons::DOTS_THREE, |ui| {
                        if ui
                            .button(icons::label(icons::PENCIL_SIMPLE, "Edit"))
                            .clicked()
                        {
                            act = Some((false, b.clone()));
                            ui.close();
                        }
                        if ui.button(icons::label(icons::TRASH, "Delete")).clicked() {
                            act = Some((true, b.clone()));
                            ui.close();
                        }
                    })
                    .response
                    .on_hover_text("More");
                    ui.label(egui::RichText::new(&b.period).size(13.0).color(p.muted));
                });
            });
            let (spent, limit) = match (b.limit_usd, b.limit_tokens) {
                (Some(u), _) => (format!("${:.2}", st.spent_usd), format!("of ${u:.2}")),
                (None, Some(t)) => (
                    format!("{} tok", tokens(st.spent_tokens)),
                    format!("of {}", tokens(t)),
                ),
                (None, None) => (
                    format!("{} tok", tokens(st.spent_tokens)),
                    "no limit".to_string(),
                ),
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(spent).size(24.0).strong());
                ui.label(egui::RichText::new(limit).size(13.0).color(p.muted));
            })
            .response
            .on_hover_text(format!("{:.0}% used", st.pct));
            kit::meter(
                ui,
                (st.pct / 100.0) as f32,
                gauge_color(ui, st.pct, &b.alert_at),
            );
            let scope = if b.applies_to == "org" {
                "the whole factory".to_string()
            } else if b.applies_to == "all" {
                "everything".to_string()
            } else {
                format!("{} {}", b.applies_to, b.subject)
            };
            let prov = if b.provider == "all" {
                String::new()
            } else {
                format!(" · {}", b.provider)
            };
            let alerts = if b.alert_at.is_empty() {
                String::new()
            } else {
                format!(
                    " · alert {}%",
                    b.alert_at
                        .iter()
                        .map(|a| a.to_string())
                        .collect::<Vec<_>>()
                        .join("/")
                )
            };
            let mut note = format!(
                "{scope}{prov}{alerts} · {}",
                b.action_at_limit.replace('_', " ")
            );
            if b.limit_usd.is_some()
                && let Some(t) = b.limit_tokens
            {
                note.push_str(&format!(
                    " · {} / {} tok",
                    tokens(st.spent_tokens),
                    tokens(t)
                ));
            }
            let icon = if b.action_at_limit == "alert_only" {
                icons::BELL
            } else {
                icons::HAND_PALM
            };
            ui.label(
                egui::RichText::new(icons::label(icon, &note))
                    .size(12.0)
                    .color(if warn { p.amber } else { p.muted }),
            );
        });
    act
}

/// A routing rule row: number, condition, model. Clickable.
fn rule_row(ui: &mut egui::Ui, n: &str, when: &str, to: &str, enabled: bool) -> egui::Response {
    let p = theme::palette(ui.ctx());
    let bg = ui.painter().add(egui::Shape::Noop);
    let inner = ui.horizontal(|ui| {
        ui.set_min_height(36.0);
        ui.label(egui::RichText::new(n).monospace().color(p.muted));
        let when = egui::RichText::new(when).size(13.0);
        ui.add(egui::Label::new(if enabled { when } else { when.color(p.muted) }).truncate());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !enabled {
                ui.label(egui::RichText::new(icons::PAUSE).color(p.muted))
                    .on_hover_text("Disabled");
            }
            ui.label(
                egui::RichText::new(to)
                    .monospace()
                    .size(13.0)
                    .color(if enabled { p.cyan } else { p.muted }),
            );
        });
    });
    let rect = inner.response.rect;
    let resp = ui.interact(
        rect,
        ui.make_persistent_id(("rule_row", n)),
        egui::Sense::click(),
    );
    if resp.hovered() {
        ui.painter().set(
            bg,
            egui::Shape::rect_filled(rect.expand2(egui::vec2(6.0, 0.0)), 0.0, p.panel2),
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn rule_when(r: &RoutingRule) -> String {
    let mut when = Vec::new();
    for (k, val) in [
        ("Repo", &r.match_repo),
        ("Agent", &r.match_agent),
        ("Card labelled", &r.match_label),
        ("Kind", &r.match_kind),
    ] {
        if !val.is_empty() {
            when.push(format!("{k} {val}"));
        }
    }
    if when.is_empty() {
        "Always".into()
    } else {
        when.join(" and ")
    }
}

fn route_to(r: &RoutingRule) -> String {
    format!("{} · {}", or_dash(&r.provider), or_dash(&r.model))
}

fn combo(ui: &mut egui::Ui, id: &str, value: &mut String, options: &[&str]) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(value.replace('_', " "))
        .show_ui(ui, |ui| {
            for o in options {
                ui.selectable_value(value, o.to_string(), o.replace('_', " "));
            }
        });
}

/// Labelled bars, longest first: (label, value, hover).
fn bars(ui: &mut egui::Ui, rows: &[(String, i64, String)], color: egui::Color32) {
    let p = theme::palette(ui.ctx());
    if rows.is_empty() {
        ui.label(egui::RichText::new("—").color(p.muted));
    }
    let max = rows.iter().map(|r| r.1).max().unwrap_or(1).max(1);
    for (label, n, hover) in rows.iter().take(12) {
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(or_dash(label)).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(n.to_string())
                        .monospace()
                        .size(12.0)
                        .color(p.muted),
                )
                .on_hover_text(hover);
            });
        });
        kit::meter(ui, *n as f32 / max as f32, color);
        ui.add_space(4.0);
    }
}

fn totals(ui: &mut egui::Ui, rows: &[UsageTotal]) {
    let p = theme::palette(ui.ctx());
    if rows.is_empty() {
        ui.label(egui::RichText::new("—").color(p.muted));
    }
    let max = rows.iter().map(|r| r.tokens).max().unwrap_or(1).max(1);
    for r in rows.iter().take(10) {
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(or_dash(&r.key)).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(tokens(r.tokens))
                        .monospace()
                        .size(12.0)
                        .color(p.muted),
                );
            });
        });
        kit::meter(ui, r.tokens as f32 / max as f32, p.violet);
        ui.add_space(4.0);
    }
}

fn gauge_color(ui: &egui::Ui, pct: f64, alerts: &[i64]) -> egui::Color32 {
    let first = alerts
        .iter()
        .copied()
        .filter(|a| *a > 0)
        .min()
        .unwrap_or(80) as f64;
    if pct >= 100.0 {
        crate::theme::palette(ui.ctx()).red
    } else if pct >= first {
        crate::theme::palette(ui.ctx()).amber
    } else {
        crate::theme::palette(ui.ctx()).green
    }
}

fn or_dash(s: &str) -> &str {
    if s.is_empty() { "—" } else { s }
}

/// 1234567 -> "1.2M".
pub(crate) fn tokens(n: i64) -> String {
    let f = n as f64;
    if n.abs() >= 1_000_000 {
        format!("{:.1}M", f / 1e6)
    } else if n.abs() >= 10_000 {
        format!("{:.0}k", f / 1e3)
    } else {
        n.to_string()
    }
}

/// How budgets nest: factory-wide, then provider and model, workflow, agent
/// and repository, node. Work is held by the tightest one it falls under.
fn nesting(ui: &mut egui::Ui, budgets: &[BudgetStatus]) {
    let p = theme::palette(ui.ctx());
    const TIERS: [(&str, &str, &[&str]); 5] = [
        (icons::GLOBE, "Factory", &["org", "all"]),
        (
            icons::PLUGS_CONNECTED,
            "Provider and model",
            &["provider", "model"],
        ),
        (icons::TREE_STRUCTURE, "Workflow", &["workflow"]),
        (icons::ROBOT, "Agent and repository", &["agent", "repo"]),
        (icons::HARD_DRIVES, "Node", &["node"]),
    ];
    kit::section(
        ui,
        icons::STACK,
        "Nesting",
        false,
        |_| {},
        |ui| {
            for (depth, (icon, title, scopes)) in TIERS.iter().enumerate() {
                let mine: Vec<&BudgetStatus> = budgets
                    .iter()
                    .filter(|b| scopes.contains(&b.budget.applies_to.as_str()))
                    .collect();
                if mine.is_empty() {
                    continue;
                }
                ui.horizontal(|ui| {
                    ui.add_space(depth as f32 * 18.0);
                    ui.label(kit::caps(&icons::label(icon, title), p.muted));
                });
                for b in mine {
                    ui.horizontal(|ui| {
                        ui.add_space(depth as f32 * 18.0 + 12.0);
                        ui.label(&b.budget.name);
                        if !b.budget.subject.is_empty() {
                            ui.label(kit::mono(&b.budget.subject, 12.0, p.muted));
                        }
                        let c = gauge_color(ui, b.pct, &b.budget.alert_at);
                        ui.label(egui::RichText::new(format!("{:.0}%", b.pct)).color(c));
                        ui.allocate_ui(egui::vec2(120.0, 8.0), |ui| {
                            kit::meter(ui, (b.pct / 100.0).clamp(0.0, 1.0) as f32, c)
                        });
                    });
                }
            }
        },
    );
}

/// The human-equivalent estimate: what the range's agent work would have
/// cost a team, against what it cost. Hover shows how it was priced.
fn estimate_panel(ui: &mut egui::Ui, e: &Estimate, days: i64) {
    let p = theme::palette(ui.ctx());
    let usd = |x: f64| format!("${}", tokens(x.round() as i64));
    kit::section(
        ui,
        icons::TREND_UP,
        &format!("Value · {days} d"),
        true,
        |_| {},
        |ui| {
            if e.agent_hours <= 0.0 {
                ui.label(
                    egui::RichText::new(icons::label(icons::TREND_UP, "No agent work yet"))
                        .color(p.muted),
                );
                return;
            }
            let tiles: [(&str, &str, String, String); 4] = [
                (
                    icons::USERS,
                    "By hand",
                    usd(e.by_hand_value),
                    format!("{} h · {} people/month", e.by_hand_hours, e.baseline_people),
                ),
                (
                    icons::COINS,
                    "Factory cost",
                    e.factory_cost
                        .or(e.model_cost)
                        .map(usd)
                        .unwrap_or_else(|| "—".into()),
                    "model use + people".into(),
                ),
                (
                    icons::TREND_UP,
                    "Return",
                    e.return_on_total
                        .or(e.return_on_model)
                        .map(|r| format!("{r:.1}×"))
                        .unwrap_or_else(|| "—".into()),
                    format!("conservative {}", usd(e.conservative.value)),
                ),
                (
                    icons::TIMER,
                    "Agent hours",
                    format!("{:.1}", e.agent_hours),
                    format!("{:.1} engineer h", e.engineer_hours),
                ),
            ];
            kit::Grid::new(150.0).show(ui, tiles.len(), |ui, i, w| {
                let (icon, label, value, note) = &tiles[i];
                kit::kpi_tile(ui, w, icon, label, value, Some(note), None);
            });
            ui.add_space(6.0);
            for r in e.roles.iter().filter(|r| r.agent_hours > 0.0) {
                let name = kls_core::estimates::ROLES
                    .iter()
                    .find(|x| x.0 == r.role)
                    .map(|x| x.1)
                    .unwrap_or(&r.role);
                ui.horizontal(|ui| {
                    ui.label(name);
                    ui.label(kit::mono(
                        &format!(
                            "{:.1} h × {:.1} × ${:.0} = {}",
                            r.agent_hours,
                            r.mult,
                            r.rate,
                            usd(r.value)
                        ),
                        12.0,
                        p.muted,
                    ));
                    if !r.counted {
                        kit::chip(ui, "default rate", p.muted);
                    }
                });
            }
            ui.label(
                egui::RichText::new(icons::label(
                    icons::INFO,
                    &format!(
                        "{} sources · settings rev {}",
                        e.inputs.sources.len().max(e.inputs.agent_hours.len()),
                        e.settings_revision
                    ),
                ))
                .size(12.0)
                .color(p.muted),
            )
            .on_hover_text(format!(
                "Agent work {} + decisions {} ({} h) + overhead {} ({} h)",
                usd(e.agent_value),
                usd(e.decision_value),
                e.decision_hours,
                usd(e.overhead_value),
                e.overhead_hours
            ));
        },
    );
}

/// Throughput, success, cost per item and cycle time per agent.
fn perf_table(ui: &mut egui::Ui, rows: &[PerfRow]) {
    let p = theme::palette(ui.ctx());
    if rows.is_empty() {
        kit::empty(ui, icons::USERS_THREE, "No sessions in range");
        return;
    }
    let mut lines = Vec::new();
    egui::ScrollArea::horizontal()
        .id_salt("perf_h")
        .show(ui, |ui| {
            TableBuilder::new(ui)
                .id_salt("perf")
                .striped(false)
                .vscroll(false)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::remainder().at_least(160.0))
                .columns(Column::auto().at_least(70.0), 7)
                .header(28.0, |mut h| {
                    for t in [
                        "Agent",
                        "Done",
                        "Per day",
                        "Success",
                        "Cost/item",
                        "Cycle",
                        "Tokens",
                        "Value",
                    ] {
                        lines.push(h.col(|ui| kit::th(ui, t)).0);
                    }
                })
                .body(|mut body| {
                    for r in rows {
                        body.row(28.0, |mut row| {
                            row.col(|ui| {
                                ui.label(&r.key);
                            });
                            row.col(|ui| {
                                ui.label(format!("{}", r.items))
                                    .on_hover_text(format!("{} open", r.open));
                            });
                            row.col(|ui| {
                                ui.label(format!("{:.1}", r.per_day));
                            });
                            row.col(|ui| match r.success_rate {
                                Some(s) => {
                                    let c = if s >= 0.8 {
                                        p.green
                                    } else if s >= 0.5 {
                                        p.amber
                                    } else {
                                        p.red
                                    };
                                    ui.label(
                                        egui::RichText::new(format!("{:.0}%", s * 100.0)).color(c),
                                    )
                                    .on_hover_text(format!(
                                        "{} ok · {} failed",
                                        r.succeeded, r.failed
                                    ));
                                }
                                None => {
                                    ui.label(egui::RichText::new("—").color(p.muted));
                                }
                            });
                            row.col(|ui| {
                                ui.label(
                                    r.cost_per_item
                                        .map(|c| format!("${c:.2}"))
                                        .unwrap_or_else(|| "—".into()),
                                );
                            });
                            row.col(|ui| {
                                ui.label(
                                    r.median_cycle_ms
                                        .map(|ms| format!("{} min", ms / 60_000))
                                        .unwrap_or_else(|| "—".into()),
                                );
                            });
                            row.col(|ui| {
                                ui.label(tokens(r.tokens));
                            });
                            row.col(|ui| {
                                ui.label(format!("${}", tokens(r.value_usd as i64)));
                            });
                            lines.push(row.response().rect);
                        });
                    }
                });
        });
    kit::row_lines(ui, &lines, p.line);
}

#[cfg(test)]
mod tests {
    #[test]
    fn token_format() {
        assert_eq!(super::tokens(950), "950");
        assert_eq!(super::tokens(42_000), "42k");
        assert_eq!(super::tokens(1_250_000), "1.2M");
    }
}

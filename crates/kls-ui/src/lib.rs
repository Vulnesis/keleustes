//! Keleustes desktop UI (egui). One module per Software Factory screen.

pub mod brand;
mod events;
mod fleet;
pub mod fmt;
pub mod icons;
pub mod kit;
pub mod loader;
mod model;
pub mod screens;
pub mod theme;

pub use model::{Model, Notice};

/// Every Software Factory screen, in navigation order. Screens marked with a
/// milestone are not built yet and say so instead of pretending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Cockpit,
    Events,
    Board,
    Checkpoints,
    Workforce,
    Sessions,
    Workflows,
    Templates,
    Runs,
    Repos,
    Budgets,
    Vault,
    Trust,
    Audit,
    Memory,
    Conflicts,
    Delivery,
    Library,
    Fleet,
    Settings,
}

impl Screen {
    pub const ALL: [Screen; 20] = [
        Screen::Cockpit,
        Screen::Events,
        Screen::Board,
        Screen::Checkpoints,
        Screen::Workforce,
        Screen::Sessions,
        Screen::Workflows,
        Screen::Templates,
        Screen::Runs,
        Screen::Repos,
        Screen::Budgets,
        Screen::Vault,
        Screen::Trust,
        Screen::Audit,
        Screen::Memory,
        Screen::Conflicts,
        Screen::Delivery,
        Screen::Library,
        Screen::Fleet,
        Screen::Settings,
    ];

    /// Navigation groups, in order.
    pub const GROUPS: &'static [(&'static str, &'static [Screen])] = &[
        (
            "Factory",
            &[
                Screen::Cockpit,
                Screen::Events,
                Screen::Board,
                Screen::Checkpoints,
                Screen::Workforce,
                Screen::Sessions,
                Screen::Workflows,
                Screen::Templates,
                Screen::Runs,
            ],
        ),
        (
            "Code",
            &[Screen::Repos, Screen::Conflicts, Screen::Delivery],
        ),
        (
            "Govern",
            &[
                Screen::Budgets,
                Screen::Vault,
                Screen::Trust,
                Screen::Audit,
                Screen::Memory,
            ],
        ),
        (
            "System",
            &[Screen::Library, Screen::Fleet, Screen::Settings],
        ),
    ];

    pub fn icon(self) -> &'static str {
        match self {
            Screen::Cockpit => icons::GAUGE,
            Screen::Events => icons::PULSE,
            Screen::Board => icons::KANBAN,
            Screen::Checkpoints => icons::SEAL_CHECK,
            Screen::Workforce => icons::USERS_THREE,
            Screen::Sessions => icons::TERMINAL_WINDOW,
            Screen::Workflows => icons::TREE_STRUCTURE,
            Screen::Templates => icons::STACK,
            Screen::Runs => icons::PLAY_CIRCLE,
            Screen::Repos => icons::GIT_BRANCH,
            Screen::Conflicts => icons::GIT_MERGE,
            Screen::Delivery => icons::CHART_BAR,
            Screen::Budgets => icons::WALLET,
            Screen::Vault => icons::LOCK_SIMPLE,
            Screen::Trust => icons::SHIELD_CHECK,
            Screen::Audit => icons::SCROLL,
            Screen::Memory => icons::BRAIN,
            Screen::Library => icons::BOOKS,
            Screen::Fleet => icons::HARD_DRIVES,
            Screen::Settings => icons::GEAR_SIX,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Screen::Cockpit => "Cockpit",
            Screen::Events => "Events",
            Screen::Board => "Board",
            Screen::Checkpoints => "Checkpoints",
            Screen::Workforce => "Workforce",
            Screen::Sessions => "Sessions",
            Screen::Workflows => "Workflows",
            Screen::Templates => "Templates",
            Screen::Runs => "Runs",
            Screen::Repos => "Repositories",
            Screen::Budgets => "Budgets",
            Screen::Vault => "Vault",
            Screen::Trust => "Trust",
            Screen::Audit => "Audit",
            Screen::Memory => "Memory",
            Screen::Conflicts => "Conflicts",
            Screen::Delivery => "Delivery",
            Screen::Library => "Library",
            Screen::Fleet => "Fleet",
            Screen::Settings => "Settings",
        }
    }

    /// Whether the screen is built (each screen module's READY).
    fn ready(self) -> bool {
        use screens::*;
        match self {
            Screen::Events | Screen::Fleet => true,
            Screen::Cockpit => cockpit::READY,
            Screen::Board => board::READY,
            Screen::Checkpoints => checkpoints::READY,
            Screen::Workforce => workforce::READY,
            Screen::Sessions => sessions::READY,
            Screen::Workflows => workflows::READY,
            Screen::Templates => templates::READY,
            Screen::Runs => runs::READY,
            Screen::Repos => repos::READY,
            Screen::Budgets => budgets::READY,
            Screen::Vault => vault::READY,
            Screen::Trust => trust::READY,
            Screen::Audit => audit::READY,
            Screen::Memory => memory::READY,
            Screen::Conflicts => conflicts::READY,
            Screen::Delivery => delivery::READY,
            Screen::Library => library::READY,
            Screen::Settings => settings::READY,
        }
    }

    /// Maps a web console path (old links in mail and GitHub comments) to a screen.
    pub fn from_web_path(path: &str) -> Option<Screen> {
        let p = path.trim_start_matches('/').trim_start_matches("resolve/");
        let p = p.split(['?', '#']).next().unwrap_or("");
        Some(match p {
            "factory" => Screen::Cockpit,
            "factory-events" => Screen::Events,
            "factory-board" | "belai-kanban" => Screen::Board,
            "factory-checkpoints" => Screen::Checkpoints,
            "factory-workforce" | "factory-agent" | "belai-agents" => Screen::Workforce,
            "factory-session" | "belai-sessions" => Screen::Sessions,
            "factory-workflows" => Screen::Workflows,
            "factory-templates" => Screen::Templates,
            "factory-runs" | "factory-run" => Screen::Runs,
            "factory-repos" => Screen::Repos,
            "factory-budgets" => Screen::Budgets,
            "vault" => Screen::Vault,
            "factory-trust" => Screen::Trust,
            "factory-audit" | "belai-audit" => Screen::Audit,
            "factory-memory" => Screen::Memory,
            "factory-conflicts" => Screen::Conflicts,
            "belai-library" => Screen::Library,
            "belai-hosts" | "belai-fleet" => Screen::Fleet,
            "factory-settings" => Screen::Settings,
            p if p.starts_with("factory-delivery") => Screen::Delivery,
            _ => return None,
        })
    }
}

/// Every screen's state, built once at startup.
struct Screens {
    cockpit: screens::cockpit::Screen,
    board: screens::board::Screen,
    checkpoints: screens::checkpoints::Screen,
    workforce: screens::workforce::Screen,
    sessions: screens::sessions::Screen,
    workflows: screens::workflows::Screen,
    templates: screens::templates::Screen,
    runs: screens::runs::Screen,
    repos: screens::repos::Screen,
    budgets: screens::budgets::Screen,
    vault: screens::vault::Screen,
    trust: screens::trust::Screen,
    audit: screens::audit::Screen,
    memory: screens::memory::Screen,
    conflicts: screens::conflicts::Screen,
    delivery: screens::delivery::Screen,
    library: screens::library::Screen,
    settings: screens::settings::Screen,
}

macro_rules! each_screen {
    ($self:ident, $screen:expr, |$s:ident| $body:expr) => {
        match $screen {
            Screen::Cockpit => {
                let $s = &mut $self.cockpit;
                $body
            }
            Screen::Board => {
                let $s = &mut $self.board;
                $body
            }
            Screen::Checkpoints => {
                let $s = &mut $self.checkpoints;
                $body
            }
            Screen::Workforce => {
                let $s = &mut $self.workforce;
                $body
            }
            Screen::Sessions => {
                let $s = &mut $self.sessions;
                $body
            }
            Screen::Workflows => {
                let $s = &mut $self.workflows;
                $body
            }
            Screen::Templates => {
                let $s = &mut $self.templates;
                $body
            }
            Screen::Runs => {
                let $s = &mut $self.runs;
                $body
            }
            Screen::Repos => {
                let $s = &mut $self.repos;
                $body
            }
            Screen::Budgets => {
                let $s = &mut $self.budgets;
                $body
            }
            Screen::Vault => {
                let $s = &mut $self.vault;
                $body
            }
            Screen::Trust => {
                let $s = &mut $self.trust;
                $body
            }
            Screen::Audit => {
                let $s = &mut $self.audit;
                $body
            }
            Screen::Memory => {
                let $s = &mut $self.memory;
                $body
            }
            Screen::Conflicts => {
                let $s = &mut $self.conflicts;
                $body
            }
            Screen::Delivery => {
                let $s = &mut $self.delivery;
                $body
            }
            Screen::Library => {
                let $s = &mut $self.library;
                $body
            }
            Screen::Settings => {
                let $s = &mut $self.settings;
                $body
            }
            Screen::Events | Screen::Fleet => {}
        }
    };
}

impl Screens {
    fn new(m: &Model) -> Self {
        use screens::*;
        Self {
            cockpit: cockpit::Screen::new(m),
            board: board::Screen::new(m),
            checkpoints: checkpoints::Screen::new(m),
            workforce: workforce::Screen::new(m),
            sessions: sessions::Screen::new(m),
            workflows: workflows::Screen::new(m),
            templates: templates::Screen::new(m),
            runs: runs::Screen::new(m),
            repos: repos::Screen::new(m),
            budgets: budgets::Screen::new(m),
            vault: vault::Screen::new(m),
            trust: trust::Screen::new(m),
            audit: audit::Screen::new(m),
            memory: memory::Screen::new(m),
            conflicts: conflicts::Screen::new(m),
            delivery: delivery::Screen::new(m),
            library: library::Screen::new(m),
            settings: settings::Screen::new(m),
        }
    }

    fn ui(&mut self, screen: Screen, ui: &mut egui::Ui, m: &Model) {
        each_screen!(self, screen, |s| s.ui(ui, m))
    }

    fn focus(&mut self, screen: Screen, id: &str) {
        each_screen!(self, screen, |s| s.focus(id))
    }
}

pub struct App {
    model: Model,
    screen: Screen,
    fleet: fleet::Fleet,
    events: events::Events,
    screens: Screens,
    toasts: Vec<(Notice, f64)>,
    theme: theme::Mode,
    /// The dark/light state last applied, to re-apply only on change.
    applied_dark: Option<bool>,
    /// The stored theme, reloaded when settings are saved elsewhere.
    stored_theme: loader::Loader<Option<String>>,
    /// (live sessions, things that need you) for the nav counts.
    nav_counts: loader::Loader<(usize, usize)>,
    /// The stored value last seen, so a local toggle is not undone by a
    /// stale read.
    last_stored: Option<String>,
}

impl App {
    pub fn new(model: Model) -> Self {
        icons::install(&model.egui_ctx());
        let mut kinds: Vec<&str> = kls_core::needs::RELOAD_ON.to_vec();
        kinds.push("session.");
        let nav_counts = loader::Loader::new(&model, &kinds, |core| async move {
            let c = core.counts().await?;
            let n = core.needs_you().await?.len();
            Ok((c.live_sessions, n))
        });
        let stored_theme = loader::Loader::new(&model, &["settings."], |core| async move {
            core.store().setting(theme::SETTING).await
        });
        let screens = Screens::new(&model);
        let core = model.core().clone();
        let theme = model
            .rt
            .block_on(async move { core.store().setting(theme::SETTING).await })
            .ok()
            .flatten()
            .map(|s| theme::Mode::parse(&s))
            .unwrap_or(theme::Mode::Dark);
        Self {
            model,
            screen: Screen::Cockpit,
            fleet: fleet::Fleet::new(),
            events: events::Events::default(),
            screens,
            toasts: Vec::new(),
            theme,
            applied_dark: None,
            stored_theme,
            nav_counts,
            last_stored: None,
        }
    }

    fn set_theme(&mut self, mode: theme::Mode) {
        self.theme = mode;
        self.applied_dark = None;
        let value = mode.as_str().to_string();
        self.model.act(move |m| async move {
            m.core().store().set_setting(theme::SETTING, &value).await?;
            Ok(None)
        });
    }

    pub fn open(&mut self, screen: Screen) {
        self.screen = screen;
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = ui.input(|i| i.time);
        for n in self.model.take_notices() {
            match n {
                Notice::Token { token, expires_at } => {
                    self.fleet.minted = Some((token, expires_at))
                }
                other => self.toasts.push((other, now + 5.0)),
            }
        }
        self.toasts.retain(|(_, until)| *until > now);
        if let Some((screen, focus)) = self.model.take_nav() {
            self.screen = screen;
            if let Some(id) = focus {
                if screen == Screen::Events {
                    self.events.focus(&id);
                }
                if screen == Screen::Fleet {
                    crate::screens::host::open_link(ui.ctx(), &id);
                }
                self.screens.focus(screen, &id);
            }
        }
        let snap = self.model.snapshot();

        if let Some(Ok(Some(stored))) = self.stored_theme.get() {
            let mode = theme::Mode::parse(&stored);
            if mode != self.theme && self.last_stored.as_deref() != Some(stored.as_str()) {
                self.theme = mode;
                self.applied_dark = None;
            }
            self.last_stored = Some(stored);
        }
        let dark = theme::is_dark(self.theme, ui.ctx());
        if self.applied_dark != Some(dark) {
            theme::apply(ui.ctx(), self.theme);
            self.applied_dark = Some(dark);
        }
        let pal = theme::palette(ui.ctx());

        let (live, needs) = match self.nav_counts.get() {
            Some(Ok(c)) => c,
            _ => (0, 0),
        };
        let online = snap.online.len();
        let joined = snap.nodes.iter().filter(|n| n.revoked_at.is_none()).count();
        let count = |s: Screen| -> String {
            match s {
                Screen::Sessions if live > 0 => live.to_string(),
                Screen::Checkpoints if needs > 0 => needs.to_string(),
                Screen::Fleet if joined > 0 => format!("{online}/{joined}"),
                _ => String::new(),
            }
        };

        egui::Panel::left("nav")
            .resizable(false)
            .exact_size(232.0)
            .frame(
                egui::Frame::new()
                    .fill(pal.panel)
                    .inner_margin(egui::Margin::symmetric(14, 18))
                    .stroke(egui::Stroke::new(1.0, pal.line)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(icons::FLOW_ARROW)
                            .color(pal.gold)
                            .size(24.0),
                    );
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("Keleustes").strong().size(17.0));
                        let port = snap.listen_addr.rsplit(':').next().unwrap_or("");
                        ui.label(
                            egui::RichText::new(format!("{} · :{port}", snap.server_name))
                                .monospace()
                                .size(11.0)
                                .color(pal.muted),
                        );
                    });
                });
                ui.add_space(14.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (group, screens) in Screen::GROUPS {
                        ui.label(kit::caps(group, pal.muted));
                        ui.add_space(4.0);
                        for s in *screens {
                            if nav_item(ui, *s, self.screen == *s, &count(*s)).clicked() {
                                self.screen = *s;
                            }
                        }
                        ui.add_space(12.0);
                    }
                });
            });

        egui::Panel::top("topbar")
            .frame(
                egui::Frame::new()
                    .fill(pal.panel)
                    .inner_margin(egui::Margin::symmetric(24, 10))
                    .stroke(egui::Stroke::new(1.0, pal.line)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let group = Screen::GROUPS
                        .iter()
                        .find(|(_, ss)| ss.contains(&self.screen))
                        .map(|(g, _)| *g)
                        .unwrap_or("");
                    ui.label(kit::caps(
                        &format!("{group} / {}", self.screen.title()),
                        pal.muted,
                    ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (icon, label, next) = if dark {
                            (icons::MOON, "Dark", theme::Mode::Light)
                        } else {
                            (icons::SUN, "Light", theme::Mode::Dark)
                        };
                        if kit::ghost_button(ui, &icons::label(icon, label))
                            .on_hover_text(if dark {
                                "Switch to light theme"
                            } else {
                                "Switch to dark theme"
                            })
                            .clicked()
                        {
                            self.set_theme(next);
                        }
                        ui.add_space(8.0);
                        let c = if online > 0 { pal.green } else { pal.muted };
                        if kit::chip(ui, &format!("● {online} of {joined} nodes online"), c)
                            .interact(egui::Sense::click())
                            .clicked()
                        {
                            self.screen = Screen::Fleet;
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(pal.bg)
                    .inner_margin(egui::Margin::symmetric(26, 22)),
            )
            .show(ui, |ui| {
                kit::grid_background(ui, ui.max_rect().expand(26.0));
                if let Some(err) = &snap.error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                match self.screen {
                    Screen::Fleet => self.fleet.ui(ui, &self.model, &snap),
                    Screen::Events => self.events.ui(ui, &self.model, &snap),
                    s => self.screens.ui(s, ui, &self.model),
                }
            });

        if !self.toasts.is_empty() {
            egui::Area::new(egui::Id::unique("toasts"))
                .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -12.0])
                .show(ui.ctx(), |ui| {
                    for (n, _) in &self.toasts {
                        egui::Frame::popup(ui.style()).show(ui, |ui| match n {
                            Notice::Error(e) => {
                                ui.colored_label(ui.visuals().error_fg_color, e);
                            }
                            Notice::Info(i) => {
                                ui.label(i);
                            }
                            Notice::Token { .. } => {}
                        });
                    }
                });
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }
    }
}

/// One navigation row: icon and name, an optional count on the right; the
/// current screen gets the panel tint and a gold left edge.
fn nav_item(ui: &mut egui::Ui, s: Screen, active: bool, count: &str) -> egui::Response {
    let pal = theme::palette(ui.ctx());
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    if active {
        painter.rect_filled(rect, 0.0, pal.panel2);
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(2.0, rect.height())),
            0.0,
            pal.gold,
        );
    } else if resp.hovered() {
        painter.rect_filled(rect, 0.0, pal.panel2.gamma_multiply(0.6));
    }
    let fg = if active || resp.hovered() {
        pal.text
    } else {
        pal.muted
    };
    let fg = if s.ready() {
        fg
    } else {
        fg.gamma_multiply(0.6)
    };
    let font = egui::FontId::proportional(14.5);
    painter.text(
        rect.left_center() + egui::vec2(12.0, 0.0),
        egui::Align2::LEFT_CENTER,
        icons::label(s.icon(), s.title()),
        font,
        fg,
    );
    if !count.is_empty() {
        painter.text(
            rect.right_center() - egui::vec2(8.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            count,
            egui::FontId::monospace(11.0),
            pal.muted,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::Screen;

    #[test]
    fn nav_groups_cover_every_screen_once() {
        let grouped: Vec<Screen> = Screen::GROUPS
            .iter()
            .flat_map(|(_, s)| s.iter().copied())
            .collect();
        assert_eq!(grouped.len(), Screen::ALL.len());
        for s in Screen::ALL {
            assert_eq!(grouped.iter().filter(|g| **g == s).count(), 1, "{s:?}");
        }
    }

    #[test]
    fn web_paths() {
        assert_eq!(
            Screen::from_web_path("/resolve/factory"),
            Some(Screen::Cockpit)
        );
        assert_eq!(
            Screen::from_web_path("/resolve/factory-board?card=K-1"),
            Some(Screen::Board)
        );
        assert_eq!(
            Screen::from_web_path("resolve/factory-delivery-people"),
            Some(Screen::Delivery)
        );
        assert_eq!(
            Screen::from_web_path("/resolve/belai-hosts"),
            Some(Screen::Fleet)
        );
        assert_eq!(Screen::from_web_path("/resolve/unknown"), None);
    }
}

//! A2A in session detail: the messages to and from one session, the peer
//! directory, and a composer that sends as the owner (`keleustes:owner`).
//! The kill switch and the blackout refuse a send like any agent's; message
//! text is other models' output and is shown as plain text.

use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme::palette;
use crate::{icons, kit};
use egui::RichText;
use kls_core::harness::{A2aPeer, OWNER_AGENT};
use kls_store::board::A2aMessage;

#[derive(Clone, Default)]
struct Data {
    messages: Vec<A2aMessage>,
    peers: Vec<A2aPeer>,
}

/// The A2A tab of one session.
pub struct A2aPanel {
    agent_id: String,
    data: Loader<Data>,
    draft: String,
}

/// Messages from or to `agent` (or a broadcast), oldest first.
pub fn messages_for(all: Vec<A2aMessage>, agent: &str) -> Vec<A2aMessage> {
    let node = agent.split(':').next().unwrap_or_default();
    all.into_iter()
        .filter(|m| {
            m.from == agent
                || m.to == agent
                || m.to.is_empty()
                || (m.to == format!("{node}:*"))
                || m.to == node
        })
        .collect()
}

impl A2aPanel {
    /// `agent_id` is `<nodeId>:<sessionId>`.
    pub fn new(model: &Model, agent_id: &str) -> Self {
        let id = agent_id.to_string();
        let data = Loader::new(model, &["a2a.", "session."], move |core| {
            let id = id.clone();
            async move {
                let all = core.store().a2a_after(0, 2000).await?;
                Ok(Data {
                    messages: messages_for(all, &id),
                    peers: core.a2a_peers("", 200).await?,
                })
            }
        });
        Self {
            agent_id: agent_id.into(),
            data,
            draft: String::new(),
        }
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let p = palette(ui.ctx());
        let data = match self.data.get() {
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
        let name = |id: &str| -> String {
            if id == OWNER_AGENT {
                return "you".into();
            }
            data.peers
                .iter()
                .find(|x| x.agent_id == id)
                .map(|x| format!("{} · {}", x.node_name, x.title))
                .unwrap_or_else(|| id.chars().take(24).collect())
        };
        egui::ScrollArea::vertical()
            .max_height(ui.available_height() - 64.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if data.messages.is_empty() {
                    kit::empty(ui, icons::BROADCAST, "No A2A messages");
                }
                for m in &data.messages {
                    let out = m.from == self.agent_id;
                    let who = if out {
                        format!(
                            "→ {}",
                            if m.to.is_empty() {
                                "all".into()
                            } else {
                                name(&m.to)
                            }
                        )
                    } else {
                        format!("← {}", name(&m.from))
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(who).small().color(p.violet));
                        ui.label(
                            RichText::new(crate::fmt::ago(kls_store::now_ms(), m.at))
                                .small()
                                .color(p.muted),
                        );
                    });
                    ui.label(RichText::new(&m.text).color(p.text));
                    ui.add_space(4.0);
                }
            });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.draft)
                    .hint_text("Message")
                    .desired_width(ui.available_width() - 90.0),
            );
            let can =
                !self.draft.trim().is_empty() && self.draft.len() <= kls_core::board::A2A_TEXT_MAX;
            if ui
                .add_enabled(
                    can,
                    egui::Button::new(icons::label(icons::PAPER_PLANE_TILT, "Send")),
                )
                .clicked()
            {
                let (to, text) = (self.agent_id.clone(), std::mem::take(&mut self.draft));
                model.act(move |m| async move {
                    let s = m.core().send_owner_a2a(&to, &text, "").await?;
                    Ok(Some(if s.delivered {
                        Notice::Info("A2A sent".into())
                    } else {
                        Notice::Error(format!("A2A refused: {}", s.refused))
                    }))
                });
            }
        });
    }
}

/// The agent ids (`<nodeId>:<sessionId>`) of a crew's live workers.
pub fn crew_agents(
    statuses: &[(String, i64, kls_core::belai::status::NodeStatus)],
    crew: &str,
) -> Vec<String> {
    let mut out: Vec<String> = statuses
        .iter()
        .flat_map(|(node, _, st)| {
            st.workers
                .iter()
                .filter(|w| w.crew == crew && !w.session_id.is_empty())
                .map(move |w| format!("{node}:{}", w.session_id))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Messages between a crew's members (or from the owner to one), oldest
/// first.
pub fn crew_messages(all: Vec<A2aMessage>, agents: &[String]) -> Vec<A2aMessage> {
    all.into_iter()
        .filter(|m| agents.contains(&m.from) || agents.contains(&m.to))
        .collect()
}

#[derive(Clone, Default)]
struct CrewData {
    agents: Vec<String>,
    messages: Vec<A2aMessage>,
    /// The A2A blackout: no messages are read or sent.
    blackout: bool,
}

/// One crew's A2A thread: its members' messages and a composer that sends
/// to every member as the owner. Under the A2A blackout nothing is read or
/// sent (core refuses a send, audited, like any agent's).
pub struct CrewThread {
    crew: String,
    data: Loader<CrewData>,
    draft: String,
}

impl CrewThread {
    pub fn new(model: &Model, crew: &str) -> Self {
        let name = crew.to_string();
        let data = Loader::new(model, &["a2a.", "node.status", "factory."], move |core| {
            let name = name.clone();
            async move {
                if core.factory_control().await?.a2a_blackout {
                    return Ok(CrewData {
                        blackout: true,
                        ..Default::default()
                    });
                }
                let agents = crew_agents(&core.node_statuses().await?, &name);
                let all = core.store().a2a_after(0, 2000).await?;
                Ok(CrewData {
                    messages: crew_messages(all, &agents),
                    agents,
                    blackout: false,
                })
            }
        });
        Self {
            crew: crew.into(),
            data,
            draft: String::new(),
        }
    }

    pub fn crew(&self) -> &str {
        &self.crew
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let p = palette(ui.ctx());
        let data = match self.data.get() {
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
        if data.blackout {
            ui.label(RichText::new(icons::label(icons::PROHIBIT, "A2A blackout")).color(p.amber));
            return;
        }
        ui.label(
            RichText::new(icons::label(
                icons::USERS_THREE,
                &format!("{} members live", data.agents.len()),
            ))
            .small()
            .color(p.muted),
        );
        egui::ScrollArea::vertical()
            .id_salt(("crew_thread", &self.crew))
            .max_height(260.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if data.messages.is_empty() {
                    kit::empty(ui, icons::BROADCAST, "No A2A messages");
                }
                for m in &data.messages {
                    ui.horizontal_wrapped(|ui| {
                        let from = if m.from == OWNER_AGENT {
                            "you"
                        } else {
                            &m.from
                        };
                        let to = if m.to.is_empty() { "all" } else { &m.to };
                        ui.label(
                            RichText::new(format!("{from} → {to}"))
                                .small()
                                .color(p.violet),
                        );
                        ui.label(
                            RichText::new(crate::fmt::ago(kls_store::now_ms(), m.at))
                                .small()
                                .color(p.muted),
                        );
                    });
                    ui.label(RichText::new(&m.text).color(p.text));
                    ui.add_space(4.0);
                }
            });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.draft)
                    .hint_text("Message the crew")
                    .desired_width(ui.available_width() - 90.0),
            );
            let can = !data.agents.is_empty()
                && !self.draft.trim().is_empty()
                && self.draft.len() <= kls_core::board::A2A_TEXT_MAX;
            if ui
                .add_enabled(
                    can,
                    egui::Button::new(icons::label(icons::PAPER_PLANE_TILT, "Send")),
                )
                .clicked()
            {
                let (to, text) = (data.agents.clone(), std::mem::take(&mut self.draft));
                let thread = format!("crew:{}", self.crew);
                model.act(move |m| async move {
                    let mut refused = 0;
                    for a in &to {
                        match m.core().send_owner_a2a(a, &text, &thread).await {
                            Ok(s) if s.delivered => {}
                            _ => refused += 1,
                        }
                    }
                    Ok(Some(if refused == 0 {
                        Notice::Info(format!("A2A sent to {}", to.len()))
                    } else {
                        Notice::Error(format!("A2A refused for {refused} of {}", to.len()))
                    }))
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(from: &str, to: &str) -> A2aMessage {
        A2aMessage {
            from: from.into(),
            to: to.into(),
            ..Default::default()
        }
    }

    #[test]
    fn thread_for_one_session() {
        let all = vec![
            m("n1:s1", "n2:s2"),
            m("n2:s2", "n1:s1"),
            m(OWNER_AGENT, "n1:s1"),
            m("n3:s3", "n3:s4"),
            m("n3:s3", ""),
            m("n3:s3", "n1:*"),
        ];
        assert_eq!(messages_for(all, "n1:s1").len(), 5);
    }

    #[test]
    fn crew_thread_is_its_members() {
        use kls_core::belai::status::{NodeStatus, Worker};
        let w = |crew: &str, s: &str| Worker {
            crew: crew.into(),
            session_id: s.into(),
            ..Default::default()
        };
        let st = vec![(
            "n1".to_string(),
            0,
            NodeStatus {
                workers: vec![w("fixers", "s1"), w("fixers", "s2"), w("other", "s3")],
                ..Default::default()
            },
        )];
        let agents = crew_agents(&st, "fixers");
        assert_eq!(agents, ["n1:s1", "n1:s2"]);
        let all = vec![
            m("n1:s1", "n1:s2"),
            m("n1:s3", "n9:s9"),
            m(OWNER_AGENT, "n1:s2"),
        ];
        assert_eq!(crew_messages(all, &agents).len(), 2);
    }
}

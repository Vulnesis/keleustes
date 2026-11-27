//! The card drawer's GitHub tab: the linked issue's state, labels and
//! comments, Sync now and Unlink, or (no issue yet) Open issue, which
//! mirrors the card to a new GitHub issue and links it.

use crate::events::clock;
use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme::{self, palette};
use egui::RichText;
use kls_core::board::{IssueComment, OWNER};
use kls_store::board::Card;
use kls_store::github::GhIssue;

/// What the tab shows.
#[derive(Clone, Debug, Default)]
pub struct IssueView {
    pub issue: Option<GhIssue>,
    pub comments: Vec<IssueComment>,
    /// The card's repository mirrors issues onto the board.
    pub mirrored: bool,
}

pub fn loader(model: &Model, card_id: &str) -> Loader<IssueView> {
    let id = card_id.to_string();
    Loader::new(
        model,
        &["card.", "github.issue", "github.repo"],
        move |core| {
            let id = id.clone();
            async move {
                let Some(card) = core.card(&id).await? else {
                    return Ok(IssueView::default());
                };
                let issue = core.card_issue(&card).await?;
                let comments = match &issue {
                    Some(i) => core.issue_comments(&i.repo, i.number, 20).await?,
                    None => Vec::new(),
                };
                let repo = issue
                    .as_ref()
                    .map(|i| i.repo.clone())
                    .unwrap_or(card.repo.clone());
                let mirrored = !repo.is_empty()
                    && core
                        .store()
                        .gh_repo(&repo)
                        .await?
                        .is_some_and(|r| r.mirror_issues);
                Ok(IssueView {
                    issue,
                    comments,
                    mirrored,
                })
            }
        },
    )
}

/// The issue body a card mirrors to: its body and a back-reference.
pub fn issue_body(c: &Card) -> String {
    let mut b = c.body.trim().to_string();
    if !b.is_empty() {
        b.push_str("\n\n");
    }
    b.push_str(&format!("Board card {}", c.key));
    b
}

pub fn tab(ui: &mut egui::Ui, model: &Model, view: &Loader<IssueView>, card: &Card) {
    let p = palette(ui.ctx());
    let v = match view.get() {
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
    let Some(i) = v.issue else {
        no_issue(ui, model, card, v.mirrored);
        return;
    };
    let now = kls_store::now_ms();
    ui.horizontal_wrapped(|ui| {
        let (icon, color) = if i.state == "open" {
            (icons::CIRCLE, p.green)
        } else {
            (icons::CHECK_CIRCLE, p.violet)
        };
        kit::chip(ui, &icons::label(icon, &i.state), color);
        ui.label(RichText::new(format!("{}#{}", i.repo, i.number)).strong());
        if !i.author.is_empty() {
            ui.label(RichText::new(format!("@{}", i.author)).color(p.muted));
        }
    });
    if !i.title.is_empty() {
        ui.add(egui::Label::new(RichText::new(&i.title).size(14.0)).wrap());
    }
    if !i.labels.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(kit::caps(&icons::label(icons::TAG, "Labels"), p.muted));
            for l in &i.labels {
                kit::tag(ui, l);
            }
        });
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(kit::mono(
            &format!(
                "{} comments · updated {} · synced {}",
                i.comments,
                fmt::ago(now, i.updated_at),
                fmt::ago(now, i.synced_at)
            ),
            12.0,
            p.muted,
        ));
    });
    ui.horizontal_wrapped(|ui| {
        if theme::primary_button(ui, &icons::label(icons::ARROWS_CLOCKWISE, "Sync now"))
            .on_hover_text("Read the repository's issues again")
            .clicked()
        {
            let repo = i.repo.clone();
            model.act(move |m| async move {
                let n = kls_forge::service(m.core()).sync_issues(&repo).await?;
                Ok(Some(Notice::Info(format!("{repo}: {n} changed"))))
            });
        }
        if i.html_url.starts_with("https://")
            && kit::ghost_button(ui, &icons::label(icons::GITHUB_LOGO, "Open"))
                .on_hover_text(&i.html_url)
                .clicked()
        {
            ui.ctx().open_url(egui::OpenUrl::new_tab(&i.html_url));
        }
        if kit::danger_button(ui, &icons::label(icons::X_CIRCLE, "Unlink"))
            .on_hover_text("The card stops following the issue")
            .clicked()
        {
            let id = card.id.clone();
            model.act(move |m| async move {
                m.core().unlink_card_issue(&id, OWNER).await?;
                Ok(Some(Notice::Info("Unlinked".into())))
            });
        }
    });
    ui.add_space(4.0);
    ui.label(kit::caps(
        &icons::label(icons::CHAT_CIRCLE_DOTS, "Comments"),
        p.muted,
    ));
    if v.comments.is_empty() {
        ui.label(RichText::new("None mirrored").color(p.muted));
    }
    for c in &v.comments {
        kit::card(ui).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("@{}", c.author)).strong());
                if c.internal == Some(true) {
                    kit::chip(ui, "team", p.green);
                } else if !c.association.is_empty() {
                    kit::chip(ui, &c.association.to_lowercase(), p.muted);
                }
                ui.label(kit::mono(&clock(now, c.at), 11.0, p.muted));
            });
            let body: String = c.body.chars().take(600).collect();
            ui.add(egui::Label::new(RichText::new(body).size(13.0)).wrap());
        });
    }
}

fn no_issue(ui: &mut egui::Ui, model: &Model, card: &Card, mirrored: bool) {
    let p = palette(ui.ctx());
    ui.label(RichText::new(icons::label(icons::GITHUB_LOGO, "No issue linked")).color(p.muted));
    if card.repo.is_empty() {
        ui.label(RichText::new("Set the card's repo to open an issue").color(p.muted));
        return;
    }
    if mirrored {
        ui.label(
            RichText::new(icons::label(
                icons::INFO,
                "Issues of this repo are mirrored",
            ))
            .size(12.0)
            .color(p.muted),
        );
    }
    if theme::primary_button(ui, &icons::label(icons::PLUS, "Open issue"))
        .on_hover_text(format!("Mirror the card to a new issue in {}", card.repo))
        .clicked()
    {
        let card = card.clone();
        model.act(move |m| async move {
            let gh = kls_forge::service(m.core());
            let c = gh.client_for(&card.repo).await?;
            let (n, _url) =
                kls_forge::sync::open_issue(&c, &card, &issue_body(&card), "user").await?;
            Ok(Some(Notice::Info(format!("Opened {}#{n}", card.repo))))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_body_names_the_card() {
        let c = Card {
            key: "K-1".into(),
            body: "Steps".into(),
            ..Default::default()
        };
        assert_eq!(issue_body(&c), "Steps\n\nBoard card K-1");
        assert_eq!(issue_body(&Card::default()), "Board card ");
    }
}

//! Board rules: the ticket standard (vdb-site `factory_ticket.go`, golden
//! `testdata/ticket-standard.golden.json`, shared with belai
//! `internal/ticketrules`) and the memory cap per scope (vdb-site
//! `belai_memory.go`: at most 500 live notes, oldest tombstoned).

use crate::Core;
use anyhow::Result;
use serde::{Deserialize, Serialize};

pub const TICKET_SECTIONS: [&str; 6] = [
    "User story",
    "Context",
    "Success criteria",
    "Definition of done",
    "Todo",
    "Out of scope",
];
pub const TICKET_TITLE_MAX: usize = 70;
/// Live notes a memory scope keeps; older ones are tombstoned.
pub const MEMORY_SCOPE_CAP: usize = 500;

/// One gap in a ticket.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TicketIssue {
    pub code: String,
    pub section: String,
    pub message: String,
}

fn heading(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("##")?;
    let first = rest.chars().next()?;
    if !first.is_whitespace() {
        return None;
    }
    let name = rest.trim();
    (!name.is_empty()).then_some(name)
}

fn list_item(line: &str) -> bool {
    let l = line.trim_start();
    let rest = if let Some(r) = l.strip_prefix(['-', '*']) {
        r
    } else {
        let digits = l.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return false;
        }
        match l[digits..].strip_prefix('.') {
            Some(r) => r,
            None => return false,
        }
    };
    rest.starts_with(char::is_whitespace) && !rest.trim().is_empty()
}

fn todo_item(line: &str) -> bool {
    let l = line.trim_start();
    let Some(rest) = l.strip_prefix(['-', '*']) else {
        return false;
    };
    if !rest.starts_with(char::is_whitespace) {
        return false;
    }
    let rest = rest.trim_start();
    let Some(rest) = rest
        .strip_prefix("[ ]")
        .or_else(|| rest.strip_prefix("[x]"))
        .or_else(|| rest.strip_prefix("[X]"))
    else {
        return false;
    };
    rest.starts_with(char::is_whitespace) && !rest.trim().is_empty()
}

fn has_email(s: &str) -> bool {
    let b = s.as_bytes();
    let local = |c: u8| c.is_ascii_alphanumeric() || b"._%+-".contains(&c);
    for (i, _) in s.match_indices('@') {
        if i == 0 || !local(b[i - 1]) {
            continue;
        }
        let dom: String = s[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-')
            .collect();
        if let Some((_, tld)) = dom.rsplit_once('.')
            && tld.len() >= 2
            && tld.chars().take(2).all(|c| c.is_ascii_alphabetic())
        {
            return true;
        }
        // `a@b.c.dev.`: the regex backtracks to the last valid dot.
        if dom
            .split('.')
            .skip(1)
            .any(|p| p.len() >= 2 && p.chars().take(2).all(|c| c.is_ascii_alphabetic()))
        {
            return true;
        }
    }
    false
}

fn has_ipv4(s: &str) -> bool {
    let b = s.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() && (i == 0 || !word(b[i - 1])) {
            // Up to four dotted groups of 1-3 digits.
            let mut j = i;
            let mut groups = 0;
            loop {
                let start = j;
                while j < b.len() && b[j].is_ascii_digit() && j - start < 3 {
                    j += 1;
                }
                if j == start {
                    break;
                }
                groups += 1;
                if groups == 4 {
                    if j >= b.len() || !word(b[j]) {
                        return true;
                    }
                    break;
                }
                if j < b.len() && b[j] == b'.' {
                    j += 1;
                } else {
                    break;
                }
            }
        }
        i += 1;
    }
    false
}

/// Every gap of a card against the ticket standard, in fixing order.
pub fn ticket_issues(title: &str, body: &str) -> Vec<TicketIssue> {
    let mut out = Vec::new();
    let mut add = |code: &str, section: &str, message: String| {
        out.push(TicketIssue {
            code: code.into(),
            section: section.into(),
            message,
        })
    };
    let title = title.trim();
    if title.is_empty() {
        add(
            "title_missing",
            "",
            "Give the ticket a title that names the thing.".into(),
        );
    } else if title.chars().count() > TICKET_TITLE_MAX {
        add(
            "title_too_long",
            "",
            "Keep the title under 70 characters.".into(),
        );
    }
    // Sections by "## " heading; the text runs to the next heading.
    let mut order: Vec<String> = Vec::new();
    let mut sections: std::collections::HashMap<String, String> = Default::default();
    let mut current: Option<String> = None;
    let mut buf = String::new();
    let mut flush = |cur: &Option<String>, buf: &mut String, order: &mut Vec<String>| {
        if let Some(k) = cur {
            if !sections.contains_key(k) {
                order.push(k.clone());
            }
            sections.insert(k.clone(), buf.trim().to_string());
        }
        buf.clear();
    };
    for line in body.split('\n') {
        if let Some(h) = heading(line) {
            flush(&current, &mut buf, &mut order);
            current = Some(h.to_lowercase());
        } else if current.is_some() {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    flush(&current, &mut buf, &mut order);
    let mut last: i64 = -1;
    for s in TICKET_SECTIONS {
        let key = s.to_lowercase();
        let Some(text) = sections.get(&key) else {
            add("section_missing", s, format!("Add the {s} section."));
            continue;
        };
        let pos = order
            .iter()
            .position(|k| *k == key)
            .map_or(-1, |p| p as i64);
        if pos < last {
            add(
                "section_order",
                s,
                format!("{s} is out of order. Use: {}.", TICKET_SECTIONS.join(", ")),
            );
        }
        last = pos;
        if text.is_empty() {
            add(
                "section_empty",
                s,
                format!("Fill in the {s} section, or write \"Unknown:\" and the question to ask."),
            );
            continue;
        }
        match s {
            "User story" => {
                let l = text.to_lowercase();
                if !l.starts_with("as ") && !l.starts_with("unknown:") {
                    add(
                        "user_story_shape",
                        s,
                        "Write the user story as \"As a [role], I want [capability], so that [outcome].\"".into(),
                    );
                }
            }
            "Success criteria" => {
                let n = text.lines().filter(|l| list_item(l)).count();
                if !(2..=6).contains(&n) {
                    add(
                        "criteria_count",
                        s,
                        "List 2 to 6 success criteria, each one testable.".into(),
                    );
                }
            }
            "Todo" => {
                let n = text.lines().filter(|l| todo_item(l)).count();
                if !(3..=12).contains(&n) {
                    add(
                        "todo_count",
                        s,
                        "List 3 to 12 todo items as \"- [ ] step\", each under a day of work."
                            .into(),
                    );
                }
            }
            "Out of scope" if text.trim().is_empty() => {
                add(
                    "section_empty",
                    s,
                    "Say at least one thing this ticket will not do.".into(),
                );
            }
            _ => {}
        }
    }
    let whole = format!("{title}\n{body}");
    if has_email(&whole) {
        add(
            "private_email",
            "",
            "Remove the email address; name a role or a team instead.".into(),
        );
    }
    if has_ipv4(&whole) {
        add("private_ip", "", "Remove the IP address.".into());
    }
    if whole.contains("/home/") || whole.contains("/Users/") || whole.contains(":\\Users\\") {
        add(
            "private_path",
            "",
            "Remove the local path; use a path relative to the repository.".into(),
        );
    }
    out
}

/// The card's gaps against the ticket standard (every card write runs it;
/// the codes ride on the `card.created` / `card.updated` event as `ticket`).
pub fn card_ticket_gaps(c: &kls_store::board::Card) -> Vec<TicketIssue> {
    ticket_issues(&c.title, &c.body)
}

/// The gap codes of a card, in fixing order.
pub fn gap_codes(c: &kls_store::board::Card) -> Vec<String> {
    card_ticket_gaps(c).into_iter().map(|i| i.code).collect()
}

impl Core {
    /// Tombstones the oldest live notes of `scope` past the cap. Returns how many.
    pub async fn enforce_memory_cap(&self, scope: &str) -> Result<usize> {
        let mut live = self.store().notes(Some(scope), "", false, 100_000).await?;
        if live.len() <= MEMORY_SCOPE_CAP {
            return Ok(0);
        }
        live.sort_by(|a, b| a.updated_at.cmp(&b.updated_at).then(a.id.cmp(&b.id)));
        let extra = live.len() - MEMORY_SCOPE_CAP;
        let mut n = 0;
        for note in live.into_iter().take(extra) {
            if self.delete_note(&note.id).await.is_ok() {
                n += 1;
            }
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Golden {
        sections: Vec<String>,
        cases: Vec<Case>,
    }
    #[derive(Deserialize)]
    struct Case {
        name: String,
        title: String,
        body: String,
        codes: Vec<String>,
    }

    #[test]
    fn ticket_standard_golden() {
        let g: Golden =
            serde_json::from_str(include_str!("testdata/ticket-standard.golden.json")).unwrap();
        assert_eq!(g.sections, TICKET_SECTIONS);
        for c in g.cases {
            let got: Vec<String> = ticket_issues(&c.title, &c.body)
                .into_iter()
                .map(|i| i.code)
                .collect();
            assert_eq!(got, c.codes, "{}", c.name);
        }
    }

    /// Every card write runs the check: the codes on the write's event match
    /// the golden cases.
    #[tokio::test]
    async fn card_writes_carry_golden_ticket_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        let g: Golden =
            serde_json::from_str(include_str!("testdata/ticket-standard.golden.json")).unwrap();
        let codes_of = |ev: &kls_store::Event| -> Vec<String> {
            let v: serde_json::Value = serde_json::from_str(&ev.data_json).unwrap();
            serde_json::from_value(v["detail"]["ticket"].clone()).unwrap_or_default()
        };
        let mut rx = core.subscribe();
        for c in g.cases.iter().filter(|c| !c.title.trim().is_empty()) {
            // Created with the case, then edited back to it from a blank body.
            let card = core
                .create_card(
                    crate::board::CardDraft {
                        title: c.title.clone(),
                        body: c.body.clone(),
                        ..Default::default()
                    },
                    "owner",
                )
                .await
                .unwrap();
            let ev = rx.recv().await.unwrap();
            assert_eq!(ev.kind, "card.created");
            assert_eq!(codes_of(&ev), c.codes, "create {}", c.name);
            assert_eq!(gap_codes(&card), c.codes, "{}", c.name);
            if c.body.is_empty() {
                continue;
            }
            let blank = core
                .update_card(
                    &card.id,
                    card.version,
                    crate::board::CardPatch {
                        body: Some(String::new()),
                        ..Default::default()
                    },
                    "owner",
                )
                .await
                .unwrap();
            let _ = rx.recv().await.unwrap();
            core.update_card(
                &blank.id,
                blank.version,
                crate::board::CardPatch {
                    body: Some(c.body.clone()),
                    ..Default::default()
                },
                "owner",
            )
            .await
            .unwrap();
            let ev = rx.recv().await.unwrap();
            assert_eq!(ev.kind, "card.updated");
            assert_eq!(codes_of(&ev), c.codes, "update {}", c.name);
        }
    }

    #[test]
    fn private_data_detectors() {
        assert!(has_email("mail ana@acme.io now"));
        assert!(!has_email("@team"));
        assert!(has_ipv4("host 10.0.0.12 down"));
        assert!(!has_ipv4("version 1.2.3"));
        assert!(!has_ipv4("v10.0.0.1"));
    }

    #[tokio::test]
    async fn memory_cap_tombstones_the_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        for i in 0..(MEMORY_SCOPE_CAP + 3) {
            core.push_note(kls_store::board::MemoryNote {
                id: format!("n{i:04}"),
                scope: "acme/api".into(),
                text: format!("note {i}"),
                created_at: 1_000 + i as i64,
                updated_at: 1_000 + i as i64,
                ..Default::default()
            })
            .await
            .unwrap();
        }
        let live = core.notes(Some("acme/api"), "", false).await.unwrap();
        assert_eq!(live.len(), MEMORY_SCOPE_CAP, "push_note enforces the cap");
        assert!(!live.iter().any(|n| n.id == "n0000" || n.id == "n0002"));
        assert_eq!(core.enforce_memory_cap("acme/api").await.unwrap(), 0);
    }
}

//! Console session actions (belai-counterparts #45): compact, relay, undo,
//! rewind, commit, btw and rename, queued as session commands.
//!
//! Each one is checked here before it is queued: the kill switch first
//! (undo and rewind only put files back, so they pass it like the other
//! wind-down commands), the A2A blackout for an agent's request, the name
//! must be one the session lists in `controls_allowed`, and the arguments
//! must fit (rewind names a checkpoint the session listed). Queued and
//! refused requests are both audited with their origin; free text (a focus,
//! a question, a title, a message) is audited by size and hash only. The
//! host admits every text it takes through its own classifier.

use super::{InboxBody, InboxItem, Refusal};
use crate::Core;
use crate::control;
use anyhow::Result;
use kls_store::audit::LedgerDraft;
use serde_json::{Value, json};

/// The console session actions, in menu order.
pub const ACTIONS: [&str; 7] = [
    "compact", "relay", "undo", "rewind", "commit", "btw", "rename",
];
/// Actions that only put back what a prompt changed: allowed under the
/// kill switch.
pub const ACTION_WIND_DOWN: [&str; 2] = ["undo", "rewind"];

/// Event kind: an action was queued or refused. Data:
/// `{sessionId, name, outcome, reason, origin, itemId}`.
pub const SESSION_ACTION: &str = "session.action";

/// Longest free text an action carries (a question, a focus, a note).
const MAX_TEXT: usize = 4000;
/// Longest session title.
const MAX_TITLE: usize = 120;

/// Whether `name` is a console session action.
pub fn is_action(name: &str) -> bool {
    ACTIONS.contains(&name)
}

/// Checks an action's arguments against the session's checkpoints.
pub fn check_args(
    name: &str,
    args: &[String],
    checkpoints: &[kls_store::belai::Checkpoint],
) -> Result<(), String> {
    let text: usize = args.iter().map(|a| a.len() + 1).sum();
    let any_control = args.iter().any(|a| a.chars().any(char::is_control));
    match name {
        "undo" if !args.is_empty() => Err("undo takes no argument".into()),
        "rewind" => match args {
            [id] if checkpoints.iter().any(|c| &c.id == id) => Ok(()),
            [_] => Err("the session lists no such checkpoint".into()),
            _ => Err("rewind names one checkpoint".into()),
        },
        "btw" | "rename" if args.iter().all(|a| a.trim().is_empty()) => {
            Err(format!("{name} needs text"))
        }
        "rename" if text > MAX_TITLE + 1 => Err(format!("a title is at most {MAX_TITLE} chars")),
        _ if text > MAX_TEXT => Err(format!("the text is at most {MAX_TEXT} chars")),
        _ if any_control => Err("the text holds a control character".into()),
        _ => Ok(()),
    }
}

/// What an audit line keeps of the arguments.
fn args_audit(name: &str, args: &[String]) -> Value {
    if name == "rewind" {
        return json!(args);
    }
    let joined = args.join(" ");
    json!({ "bytes": joined.len(), "sha256": super::start::sha256_hex(&joined) })
}

impl Core {
    /// Queues console action `name` for `session_id` as `origin` (see the
    /// module docs for the checks). Returns the inbox item.
    pub async fn queue_action(
        &self,
        session_id: &str,
        name: &str,
        args: &[String],
        origin: &str,
    ) -> Result<InboxItem> {
        let out = self.check_action(session_id, name, args, origin).await;
        let node = self
            .store()
            .session(session_id)
            .await?
            .map(|s| s.node_id)
            .unwrap_or_default();
        let item = match out {
            Ok(()) => {
                self.queue_inbox(
                    session_id,
                    InboxBody::Command {
                        name: name.into(),
                        args: args.to_vec(),
                    },
                )
                .await
            }
            Err(e) => Err(e),
        };
        let (outcome, why, item_id) = match &item {
            Ok(i) => ("queued", String::new(), i.id.clone()),
            Err(e) => ("refused", format!("{e:#}"), String::new()),
        };
        let data = json!({
            "sessionId": session_id, "name": name, "args": args_audit(name, args),
            "outcome": outcome, "reason": why, "origin": origin, "itemId": item_id,
        });
        let _ = self.emit(SESSION_ACTION, session_id, &data).await;
        let draft = LedgerDraft {
            kind: "session".into(),
            actor: origin.into(),
            actor_kind: if outcome == "refused" { "rule" } else { "user" }.into(),
            msg: format!("/{name} for session {session_id} by {origin}: {outcome}"),
            subject: json!({ "session": session_id, "action": name }),
            payload: data,
            node_id: node,
            session_id: session_id.into(),
            ..Default::default()
        };
        for i in 1..=5u64 {
            match self.audit_record(draft.clone()).await {
                Ok(_) => break,
                Err(e) if i == 5 => tracing::warn!("audit of a session action: {e:#}"),
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(40 * i)).await,
            }
        }
        item
    }

    async fn check_action(
        &self,
        session_id: &str,
        name: &str,
        args: &[String],
        origin: &str,
    ) -> Result<()> {
        let invalid = |why: String| anyhow::Error::new(Refusal::Invalid(why));
        if !is_action(name) {
            return Err(invalid(format!("{name:?} is not a session action")));
        }
        if !ACTION_WIND_DOWN.contains(&name) {
            control::check(self, "command", session_id, origin).await?;
        }
        if !(origin == "owner" || origin.starts_with("owner:") || origin.starts_with("owner@")) {
            control::check_a2a(self, origin, session_id, origin).await?;
        }
        let x = self.store().session_extras(session_id).await?;
        let allowed = x
            .as_ref()
            .and_then(|x| x.accepts.as_ref())
            .is_some_and(|a| {
                a.controls_allowed
                    .iter()
                    .any(|c| c.trim_start_matches('/') == name)
            });
        if !allowed {
            return Err(invalid(format!("the session does not take /{name}")));
        }
        let cps = x.map(|x| x.checkpoints).unwrap_or_default();
        check_args(name, args, &cps).map_err(invalid)
    }
}

/// One transcript entry's belai identity: its type, entry id and parent id
/// (`meta_json` "type", "id", "parentId").
fn belai_link(e: &kls_store::sessions::Entry) -> Option<(String, String, String)> {
    let v: Value = serde_json::from_str(&e.meta_json).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let id = s("id");
    (!id.is_empty()).then(|| (s("type"), id, s("parentId")))
}

/// The entries a rewind (or a belai `/tree` move) took off the conversation,
/// by `idx`. belai keeps a transcript append-only: a rewind writes a `branch`
/// line parented to the point it returns to, and every later line parents to
/// the one before it. The conversation is the chain from the newest line back
/// to the first, read the way belai reads it (`session.ActivePath`); a belai
/// line off that chain was rewound. A transcript with no `branch` line, or
/// whose chain reaches a line not synced yet, marks nothing.
pub fn rewound(entries: &[kls_store::sessions::Entry]) -> std::collections::HashSet<i64> {
    use std::collections::{HashMap, HashSet};
    let links: Vec<(i64, (String, String, String))> = entries
        .iter()
        .filter_map(|e| belai_link(e).map(|l| (e.idx, l)))
        .collect();
    if !links.iter().any(|(_, (t, _, _))| t == "branch") {
        return HashSet::new();
    }
    let parent: HashMap<&str, &str> = links
        .iter()
        .map(|(_, (_, id, p))| (id.as_str(), p.as_str()))
        .collect();
    let Some((_, (_, leaf, _))) = links.iter().max_by_key(|(idx, _)| *idx) else {
        return HashSet::new();
    };
    let mut on: HashSet<&str> = HashSet::new();
    let mut id = leaf.as_str();
    while !id.is_empty() && on.insert(id) {
        match parent.get(id) {
            Some(p) => id = p,
            // A parent not synced yet: the chain cannot be read, so nothing is
            // marked rather than everything before the gap.
            None => return HashSet::new(),
        }
    }
    links
        .iter()
        .filter(|(_, (_, id, _))| !on.contains(id.as_str()))
        .map(|(idx, _)| *idx)
        .collect()
}

/// The prompt a rewind to `checkpoint` returns for editing: the text of the
/// user entry the checkpoint names (belai puts it back in the TUI's editor;
/// the console puts it in the composer). Untrusted, like every entry.
pub fn rewind_prompt(
    entries: &[kls_store::sessions::Entry],
    checkpoint: &kls_store::belai::Checkpoint,
) -> Option<String> {
    if checkpoint.entry_id.is_empty() {
        return None;
    }
    entries.iter().find_map(|e| {
        let (t, id, _) = belai_link(e)?;
        (t == "user" && id == checkpoint.entry_id).then(|| e.text.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kls_store::sessions::Entry;

    fn entry(idx: i64, typ: &str, id: &str, parent: &str, text: &str) -> Entry {
        Entry {
            idx,
            role: if typ == "user" { "user" } else { "system" }.into(),
            text: text.into(),
            at: 0,
            meta_json: json!({ "type": typ, "id": id, "parentId": parent }).to_string(),
        }
    }

    // A rewind to before u2: the branch line parents to a1, so u2, its reply
    // and the line that followed leave the conversation; the branch line and
    // what comes after it stay. The prompt goes back for editing.
    #[test]
    fn rewound_follows_the_branch_line() {
        let mut es = vec![
            entry(0, "session_meta", "m", "", ""),
            entry(1, "user", "u1", "m", "first"),
            entry(2, "assistant", "a1", "u1", "ok"),
            entry(3, "user", "u2", "a1", "second"),
            entry(4, "assistant", "a2", "u2", "done"),
            entry(5, "checkpoints", "c", "a2", ""),
            entry(6, "branch", "b", "a1", ""),
            entry(7, "system", "s", "b", "console: rewind c1"),
        ];
        let got = rewound(&es);
        assert_eq!(got, [3, 4, 5].into_iter().collect());
        let cp = kls_store::belai::Checkpoint {
            id: "c1".into(),
            entry_id: "u2".into(),
            ..Default::default()
        };
        assert_eq!(rewind_prompt(&es, &cp).as_deref(), Some("second"));
        let none = kls_store::belai::Checkpoint {
            entry_id: "a1".into(),
            ..cp.clone()
        };
        assert_eq!(rewind_prompt(&es, &none), None);

        // A line from Keleustes with no belai id is never marked.
        es.push(Entry {
            idx: 8,
            role: "system".into(),
            text: "note".into(),
            at: 0,
            meta_json: String::new(),
        });
        assert_eq!(rewound(&es), [3, 4, 5].into_iter().collect());
    }

    // No branch line: nothing is rewound. A chain broken by an unsynced line:
    // nothing is marked rather than everything before the gap.
    #[test]
    fn rewound_marks_nothing_without_a_readable_branch() {
        let flat = vec![
            entry(0, "user", "u1", "", "first"),
            entry(1, "assistant", "a1", "u1", "ok"),
        ];
        assert!(rewound(&flat).is_empty());
        let gap = vec![
            entry(0, "user", "u1", "", "first"),
            entry(1, "assistant", "a1", "u1", "ok"),
            entry(3, "branch", "b", "missing", ""),
        ];
        assert!(rewound(&gap).is_empty());
    }
}

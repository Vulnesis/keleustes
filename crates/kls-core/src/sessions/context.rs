//! A session's factory context (WP5): the run step it serves, the budgets
//! that limit it, its branch's pull request checks, its cards, and the
//! agent's todo list from the transcript. Read only.

use super::Entry;
use crate::Core;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StepFact {
    pub run_id: String,
    pub run_status: String,
    pub title: String,
    pub status: String,
    pub node_type: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetFact {
    pub name: String,
    pub pct: f64,
    pub action: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PrFact {
    pub repo: String,
    pub number: i64,
    pub title: String,
    pub checks_state: String,
    pub failing: Vec<String>,
    pub url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CardFact {
    pub id: String,
    pub key: String,
    pub title: String,
    pub status: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Todo {
    pub text: String,
    /// "pending", "in_progress", "completed".
    pub status: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionContext {
    pub step: Option<StepFact>,
    pub budgets: Vec<BudgetFact>,
    pub pulls: Vec<PrFact>,
    pub cards: Vec<CardFact>,
    pub todo: Vec<Todo>,
}

impl SessionContext {
    pub fn is_empty(&self) -> bool {
        self.step.is_none()
            && self.budgets.is_empty()
            && self.pulls.is_empty()
            && self.cards.is_empty()
            && self.todo.is_empty()
    }
}

fn todos_in(v: &Value) -> Option<Vec<Todo>> {
    let list = v
        .get("todos")
        .or_else(|| v.pointer("/input/todos"))
        .or_else(|| v.pointer("/args/todos"))?
        .as_array()?;
    Some(
        list.iter()
            .filter_map(|t| {
                let text = t
                    .get("content")
                    .or_else(|| t.get("text"))
                    .or_else(|| t.get("title"))
                    .and_then(Value::as_str)?;
                Some(Todo {
                    text: text.chars().take(200).collect(),
                    status: t
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("pending")
                        .to_string(),
                })
            })
            .take(50)
            .collect(),
    )
}

/// The latest todo list in a transcript: a `TodoWrite` call (or any entry
/// whose meta or JSON text carries `todos`).
pub fn latest_todo(entries: &[Entry]) -> Vec<Todo> {
    for e in entries.iter().rev() {
        for src in [&e.meta_json, &e.text] {
            if let Ok(v) = serde_json::from_str::<Value>(src)
                && let Some(t) = todos_in(&v)
            {
                return t;
            }
        }
    }
    Vec::new()
}

impl Core {
    /// What the factory knows around one session.
    pub async fn session_context(&self, session_id: &str) -> Result<SessionContext> {
        let store = self.store();
        let Some(s) = store.session(session_id).await? else {
            return Ok(SessionContext::default());
        };
        let mut c = SessionContext::default();
        if !s.run_id.is_empty()
            && let Some(run) = store.run(&s.run_id).await?
        {
            let steps = store.run_steps(&s.run_id).await?;
            let step = steps
                .iter()
                .rev()
                .find(|st| st.session_id.as_deref() == Some(session_id))
                .or_else(|| steps.iter().rev().find(|st| st.status == "running"))
                .or(steps.last());
            c.step = Some(StepFact {
                run_id: run.id.clone(),
                run_status: run.status.clone(),
                title: step.map(|x| x.title.clone()).unwrap_or_default(),
                status: step.map(|x| x.status.clone()).unwrap_or_default(),
                node_type: step.map(|x| x.node_type.clone()).unwrap_or_default(),
            });
        }
        let spend = crate::vault::Spend {
            provider: s.provider.clone(),
            model: s.model.clone(),
            agent: s.agent.clone(),
            repo: s.repo.clone(),
            ..Default::default()
        };
        if let Ok(st) = crate::vault::budget_statuses(self).await {
            c.budgets = st
                .into_iter()
                .filter(|b| crate::vault::budget_applies(&b.budget, &spend))
                .map(|b| BudgetFact {
                    name: b.budget.name.clone(),
                    pct: b.pct,
                    action: b.budget.action_at_limit.clone(),
                })
                .take(6)
                .collect();
        }
        if !s.branch.is_empty() && !s.repo.is_empty() {
            let repo = s.repo.trim_end_matches(".git");
            c.pulls = store
                .gh_pulls(None, Some("open"))
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|p| {
                    p.head_ref == s.branch && (repo.ends_with(&p.repo) || p.repo.ends_with(repo))
                })
                .map(|p| PrFact {
                    repo: p.repo,
                    number: p.number,
                    title: p.title,
                    checks_state: p.checks_state,
                    failing: p.checks.failing,
                    url: p.html_url,
                })
                .take(4)
                .collect();
        }
        if !s.card_id.is_empty()
            && let Some(card) = store.card(&s.card_id).await?
        {
            c.cards.push(CardFact {
                id: card.id,
                key: card.key,
                title: card.title,
                status: card.status,
            });
        }
        let tail = store
            .entries(session_id, (s.next_index - 300).max(0), 300)
            .await?;
        c.todo = latest_todo(&tail);
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_from_a_todowrite_call() {
        let e = |meta: &str, text: &str| Entry {
            meta_json: meta.into(),
            text: text.into(),
            role: "tool".into(),
            ..Default::default()
        };
        let entries = vec![
            e(
                r#"{"tool":"TodoWrite","input":{"todos":[{"content":"old","status":"completed"}]}}"#,
                "",
            ),
            e(
                "",
                r#"{"todos":[{"content":"write tests","status":"in_progress"},{"content":"ship"}]}"#,
            ),
            e("", "plain text"),
        ];
        let t = latest_todo(&entries);
        assert_eq!(t.len(), 2);
        assert_eq!(
            (t[0].text.as_str(), t[0].status.as_str()),
            ("write tests", "in_progress")
        );
        assert_eq!(t[1].status, "pending");
        assert!(latest_todo(&entries[2..]).is_empty());
    }

    #[tokio::test]
    async fn context_of_a_bare_session_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        core.session_upsert(
            "n1",
            crate::sessions::Session {
                id: "s1".into(),
                ..Default::default()
            },
        )
        .await
        .ok();
        assert!(core.session_context("s1").await.unwrap().is_empty());
        assert!(core.session_context("nope").await.unwrap().is_empty());
    }
}

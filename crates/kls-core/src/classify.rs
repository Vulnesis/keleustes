//! Done-card classification (vdb-site `belai_kanban_classify.go`, docs
//! `belai-kanban-classify.md`): a card that reaches done with a pull request
//! is tagged once as `sast`, `sca`, `secrets`, `iac`, `container`,
//! `malware`, `cloud` or `defect`.
//!
//! Keleustes calls no model itself: the question goes to a node as a model
//! job (`{"task": "systemone", state, questions}`), and the answer comes back
//! as `{answers: {category: {choice, probabilities}}, provider, model}`.
//! Rust applies the rule: an answer that is missing, not one of the eight, or
//! below [`MIN_PROB`] leaves the card alone (the next write to it asks
//! again). A stored class (`keleustes.class` in the card's meta, or belai's
//! `belai.agent.class`) or a category label stops a repeat.
//!
//! Every attempt checks the kill switch (`job`) and the A2A blackout first,
//! and writes an audit entry with its origin and outcome, refusals included.

use crate::Core;
use crate::board::{CardPatch, LABELS_MAX};
use anyhow::{Result, bail};
use kls_store::audit::LedgerDraft;
use kls_store::board::Card;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Mutex;

/// The choices, in the order the model is asked. All but DEFECT are
/// scanner categories.
pub const CATEGORIES: [&str; 8] = [
    "SAST",
    "SCA",
    "SECRETS",
    "IAC",
    "CONTAINER",
    "MALWARE",
    "CLOUD",
    "DEFECT",
];

pub const DESCRIPTIONS: [(&str, &str); 8] = [
    (
        "SAST",
        "fixes a vulnerability in first-party source code that a static analysis scanner reported",
    ),
    (
        "SCA",
        "fixes a vulnerable third-party dependency or package version that a software composition scanner reported",
    ),
    (
        "SECRETS",
        "removes or rotates a hard-coded credential, token or key that a secrets scanner reported",
    ),
    (
        "IAC",
        "fixes an insecure infrastructure-as-code or configuration finding (Terraform, Kubernetes, CloudFormation)",
    ),
    (
        "CONTAINER",
        "fixes a vulnerability or misconfiguration in a container image or Dockerfile that an image scanner reported",
    ),
    (
        "MALWARE",
        "removes or replaces a malicious or compromised package or file that a malware scanner reported",
    ),
    (
        "CLOUD",
        "fixes a cloud account or runtime posture finding that a cloud scanner reported",
    ),
    (
        "DEFECT",
        "an ordinary bug fix or change that does not answer any scanner finding",
    ),
];

pub const QUESTION: &str = "category";
/// A sanity floor, not a calibrated threshold: eight options make a uniform
/// answer 0.125.
pub const MIN_PROB: f64 = 0.3;
pub const TIMEOUT_MS: i64 = 30_000;
/// The actor on the card write and the audit entry.
pub const ACTOR: &str = "classify";
/// `"off"` stops the automatic run on card writes (on demand still works).
pub const SETTING: &str = "board.classify";

pub mod kinds {
    /// Data `{key, tag, confidence, provider, model, nodeId, origin}`.
    pub const CARD_CLASSIFIED: &str = "card.classified";
}

const QUESTION_TEXT: &str = "A pull request was merged for this finished task. Which kind of change is it? \
Pick a scanner category only when the change fixes something a scanner of that kind reported \
(a scanner finding is usually named in the state); pick DEFECT for any other fix or change.";

/// A stored classification (the marker that stops a repeat).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Class {
    pub tag: String,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub at: i64,
    /// What asked: `card.write`, `owner` ...
    #[serde(default)]
    pub origin: String,
}

/// What an attempt did.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Not eligible (not done, no pull request, already classified).
    Skipped,
    Classified(Class),
    /// No usable answer (missing, unknown, or below the floor); the card is untouched.
    Undecided {
        choice: String,
        confidence: f64,
    },
    /// Another attempt or writer got there first.
    Raced,
}

/// The label for a category, or `None` when it is not one.
pub fn class_tag(s: &str) -> Option<String> {
    let l = s.trim().to_ascii_lowercase();
    CATEGORIES
        .iter()
        .any(|c| c.eq_ignore_ascii_case(&l))
        .then_some(l)
}

fn meta(c: &Card) -> Value {
    let v: Value = serde_json::from_str(&c.meta_json).unwrap_or_else(|_| json!({}));
    if v.is_object() { v } else { json!({}) }
}

/// The card's stored class, if any.
pub fn class_of(c: &Card) -> Option<Class> {
    let m = meta(c);
    ["/keleustes/class", "/belai/agent/class"]
        .iter()
        .find_map(|p| m.pointer(p).filter(|v| v.is_object()).cloned())
        .and_then(|v| serde_json::from_value(v).ok())
}

/// The whole rule for when a card is asked: done, live, with a pull
/// request, and not already classified or labelled with a category.
pub fn eligible(c: &Card) -> bool {
    !c.deleted
        && c.status == "done"
        && !c.pr_url.trim().is_empty()
        && class_of(c).is_none()
        && !c.labels.iter().any(|l| class_tag(l).is_some())
}

/// The model job's payload for a card.
pub fn request(c: &Card) -> Value {
    let body: String = c.body.chars().take(2000).collect();
    let mut state = json!({
        "title": c.title,
        "description": body,
        "pullRequest": c.pr_url,
        "hasScannerFinding": false,
    });
    if !c.repo.is_empty() {
        state["repository"] = json!(c.repo);
    }
    if !c.branch.is_empty() {
        state["branch"] = json!(c.branch);
    }
    if !c.labels.is_empty() {
        state["labels"] = json!(c.labels);
    }
    let descriptions: serde_json::Map<String, Value> = DESCRIPTIONS
        .iter()
        .map(|(k, v)| (k.to_string(), json!(v)))
        .collect();
    json!({
        "task": "systemone",
        "state": state,
        "questions": {
            QUESTION: {
                "type": "choice",
                "question": QUESTION_TEXT,
                "options": CATEGORIES,
                "descriptions": descriptions,
            }
        }
    })
}

/// The winning category and its probability out of a model answer. `None`
/// when the answer is missing or not one of the options; the probability is
/// 1.0 when the answer carries none.
pub fn pick(out: &Value) -> Option<(String, f64)> {
    let a = out.pointer(&format!("/answers/{QUESTION}"))?;
    let choice = a
        .get("choice")
        .or_else(|| a.get("answer"))
        .and_then(Value::as_str)?;
    let tag = class_tag(choice)?;
    let probs = a.get("probabilities").and_then(Value::as_object);
    let p = match probs {
        Some(m) if !m.is_empty() => m
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(choice))
            .and_then(|(_, v)| v.as_f64())
            .unwrap_or(0.0),
        _ => a.get("confidence").and_then(Value::as_f64).unwrap_or(1.0),
    };
    Some((tag, p))
}

/// The labels with `tag` added, keeping it within the label cap.
pub fn with_class_tag(labels: &[String], tag: &str) -> Vec<String> {
    let mut out: Vec<String> = labels.iter().filter(|l| *l != tag).cloned().collect();
    out.truncate(LABELS_MAX - 1);
    out.push(tag.to_string());
    out
}

/// Card ids with an attempt in flight in this process.
static IN_FLIGHT: Mutex<Option<HashSet<String>>> = Mutex::new(None);

struct Flight(String);

impl Flight {
    fn take(id: &str) -> Option<Self> {
        let mut g = IN_FLIGHT.lock().unwrap();
        g.get_or_insert_with(HashSet::new)
            .insert(id.to_string())
            .then(|| Flight(id.to_string()))
    }
}

impl Drop for Flight {
    fn drop(&mut self) {
        if let Some(s) = IN_FLIGHT.lock().unwrap().as_mut() {
            s.remove(&self.0);
        }
    }
}

/// After a card write: asks about a card that just became eligible, in the
/// background (never blocks the write). Needs a node server; the
/// [`SETTING`] `"off"` turns it off.
pub fn after_write(core: &Core, old: &Card, new: &Card) {
    if !eligible(new) || (eligible(old) && old.version == new.version) {
        return;
    }
    if core.gateway().is_err() || tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    let core = core.clone();
    let id = new.id.clone();
    tokio::spawn(async move {
        let off = core
            .store()
            .setting(SETTING)
            .await
            .ok()
            .flatten()
            .is_some_and(|v| v.trim() == "off");
        if off {
            return;
        }
        if let Err(e) = core.classify_card(&id, "card.write").await {
            tracing::debug!(card = %id, "classify: {e:#}");
        }
    });
}

impl Core {
    async fn classify_audit(
        &self,
        card: &Card,
        origin: &str,
        outcome: &str,
        node_id: &str,
        data: Value,
    ) -> Result<()> {
        self.audit_record(LedgerDraft {
            at: 0,
            kind: "card".into(),
            actor: ACTOR.into(),
            actor_kind: "rule".into(),
            msg: format!("card.classify {} {outcome}", card.key),
            subject: json!({ "action": "card.classify", "cardId": card.id, "key": card.key }),
            payload: json!({ "outcome": outcome, "origin": origin, "data": data }),
            origin: "factory".into(),
            node_id: node_id.into(),
            session_id: String::new(),
            ref_key: None,
        })
        .await?;
        Ok(())
    }

    /// Classifies one done card (see the module docs). `origin` is what
    /// asked (`card.write`, `owner`). A card that is not eligible is a no-op.
    pub async fn classify_card(&self, id: &str, origin: &str) -> Result<Outcome> {
        let Some(card) = self.card(id).await? else {
            bail!("no such card {id}");
        };
        if !eligible(&card) {
            return Ok(Outcome::Skipped);
        }
        let Some(_flight) = Flight::take(&card.id) else {
            return Ok(Outcome::Raced);
        };
        // The kill switch and the A2A blackout, first.
        if let Err(e) = crate::control::check(self, "job", &card.key, origin).await {
            self.classify_audit(
                &card,
                origin,
                "refused",
                "",
                json!({ "reason": format!("{e}") }),
            )
            .await?;
            return Err(e);
        }
        if let Err(e) = crate::control::check_a2a(self, ACTOR, "systemone", origin).await {
            self.classify_audit(
                &card,
                origin,
                "refused",
                "",
                json!({ "reason": format!("{e}") }),
            )
            .await?;
            return Err(e);
        }
        let gw = self.gateway()?;
        let out = gw
            .job(
                None,
                "model",
                &request(&card).to_string(),
                crate::JobNeeds::default(),
                TIMEOUT_MS,
            )
            .await;
        let out = match out {
            Ok(o) if o.status == "ok" => o,
            Ok(o) => {
                self.classify_audit(
                    &card,
                    origin,
                    "failed",
                    &o.node_id,
                    json!({ "status": o.status, "error": o.error }),
                )
                .await?;
                bail!("the node could not classify {} ({})", card.key, o.status);
            }
            Err(e) => {
                self.classify_audit(
                    &card,
                    origin,
                    "failed",
                    "",
                    json!({ "error": format!("{e:#}") }),
                )
                .await?;
                return Err(e);
            }
        };
        let v: Value = serde_json::from_str(&out.output_json).unwrap_or(Value::Null);
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let picked = pick(&v);
        let (tag, p) = match picked {
            Some((t, p)) if p >= MIN_PROB => (t, p),
            other => {
                let (choice, confidence) = other.unwrap_or_default();
                self.classify_audit(
                    &card,
                    origin,
                    "undecided",
                    &out.node_id,
                    json!({ "choice": choice, "confidence": confidence, "floor": MIN_PROB }),
                )
                .await?;
                return Ok(Outcome::Undecided { choice, confidence });
            }
        };
        let class = Class {
            tag: tag.clone(),
            confidence: p,
            provider: s("provider"),
            model: s("model"),
            node_id: out.node_id.clone(),
            at: kls_store::now_ms(),
            origin: origin.into(),
        };
        // Re-read: apply only while the card is still eligible.
        let mut cur = card;
        for _ in 0..2 {
            if !eligible(&cur) {
                return Ok(Outcome::Raced);
            }
            let mut m = meta(&cur);
            if !m.get("keleustes").is_some_and(Value::is_object) {
                m["keleustes"] = json!({});
            }
            m["keleustes"]["class"] = serde_json::to_value(&class)?;
            let p = CardPatch {
                labels: Some(with_class_tag(&cur.labels, &tag)),
                meta_json: Some(m.to_string()),
                ..Default::default()
            };
            match self.update_card(&cur.id, cur.version, p, ACTOR).await {
                Ok(c) => {
                    self.emit(
                        kinds::CARD_CLASSIFIED,
                        &c.id,
                        &json!({ "key": c.key, "tag": class.tag, "confidence": class.confidence,
                                 "provider": class.provider, "model": class.model,
                                 "nodeId": class.node_id, "origin": origin, "actor": ACTOR }),
                    )
                    .await?;
                    self.classify_audit(
                        &c,
                        origin,
                        "classified",
                        &class.node_id,
                        serde_json::to_value(&class)?,
                    )
                    .await?;
                    return Ok(Outcome::Classified(class));
                }
                Err(e) => match e.downcast_ref::<crate::board::VersionConflict>() {
                    Some(vc) => cur = vc.current.clone(),
                    None => return Err(e),
                },
            }
        }
        Ok(Outcome::Raced)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::CardDraft;
    use crate::{DispatchSpec, JobNeeds, JobOutcome, NodeGateway};
    use std::sync::Arc;

    struct Fake {
        jobs: Mutex<Vec<String>>,
        reply: Mutex<String>,
    }

    #[async_trait::async_trait]
    impl NodeGateway for Fake {
        async fn online(&self) -> Vec<String> {
            vec!["n1".into()]
        }
        async fn dispatch(&self, _: &str, _: DispatchSpec) -> Result<String> {
            Ok("d".into())
        }
        async fn job(
            &self,
            _: Option<&str>,
            kind: &str,
            payload: &str,
            _: JobNeeds,
            _: i64,
        ) -> Result<JobOutcome> {
            assert_eq!(kind, "model");
            self.jobs.lock().unwrap().push(payload.into());
            Ok(JobOutcome {
                status: "ok".into(),
                output_json: self.reply.lock().unwrap().clone(),
                node_id: "n1".into(),
                ..Default::default()
            })
        }
    }

    fn answer(choice: &str, p: f64) -> String {
        json!({ "answers": { "category": { "choice": choice,
                "probabilities": { choice: p, "DEFECT": 1.0 - p } } },
                "provider": "anthropic", "model": "m1" })
        .to_string()
    }

    async fn setup(reply: String) -> (Core, Arc<Fake>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        let fake = Arc::new(Fake {
            jobs: Mutex::new(Vec::new()),
            reply: Mutex::new(reply),
        });
        core.set_gateway(fake.clone());
        (core, fake, dir)
    }

    async fn done_card(core: &Core, title: &str) -> Card {
        core.create_card(
            CardDraft {
                title: title.into(),
                status: "done".into(),
                pr_url: "https://github.com/acme/api/pull/9".into(),
                labels: vec!["build".into()],
                ..Default::default()
            },
            "owner",
        )
        .await
        .unwrap()
    }

    async fn audits(core: &Core, outcome: &str) -> usize {
        core.store()
            .ledger(&kls_store::audit::LedgerQuery {
                text: Some("card.classify".into()),
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap()
            .iter()
            .filter(|e| e.msg.ends_with(outcome))
            .count()
    }

    #[test]
    fn rule_parts() {
        assert_eq!(class_tag(" SCA "), Some("sca".into()));
        assert_eq!(class_tag("bug"), None);
        assert_eq!(
            pick(&serde_json::from_str(&answer("IAC", 0.8)).unwrap()),
            Some(("iac".into(), 0.8))
        );
        assert_eq!(
            pick(&json!({"answers": {"category": {"choice": "nope"}}})),
            None
        );
        let labels: Vec<String> = (0..40).map(|i| format!("l{i}")).collect();
        let l = with_class_tag(&labels, "sast");
        assert_eq!(l.len(), LABELS_MAX);
        assert_eq!(l.last().unwrap(), "sast");
        let q = request(&Card {
            title: "Bump lodash".into(),
            ..Default::default()
        });
        assert_eq!(q["task"], "systemone");
        assert_eq!(
            q["questions"]["category"]["options"]
                .as_array()
                .unwrap()
                .len(),
            8
        );
    }

    #[tokio::test]
    async fn classifies_once_with_provenance_and_audit() {
        let (core, fake, _d) = setup(answer("SCA", 0.82)).await;
        let c = done_card(&core, "Bump lodash").await;
        let out = core.classify_card(&c.id, "owner").await.unwrap();
        let Outcome::Classified(cls) = out else {
            panic!("{out:?}")
        };
        assert_eq!(cls.tag, "sca");
        let c = core.card(&c.id).await.unwrap().unwrap();
        assert!(c.labels.contains(&"sca".to_string()) && c.labels.contains(&"build".to_string()));
        let stored = class_of(&c).unwrap();
        assert_eq!(
            (stored.node_id.as_str(), stored.model.as_str()),
            ("n1", "m1")
        );
        assert_eq!(stored.origin, "owner");
        assert_eq!(audits(&core, "classified").await, 1);
        // Asked once: again by hand, and through card writes.
        assert_eq!(
            core.classify_card(&c.id, "owner").await.unwrap(),
            Outcome::Skipped
        );
        let c = core
            .move_card(&c.id, c.version, "review", "owner")
            .await
            .unwrap();
        core.move_card(&c.id, c.version, "done", "owner")
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert_eq!(fake.jobs.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_write_into_done_asks_in_the_background() {
        let (core, fake, _d) = setup(answer("SECRETS", 0.9)).await;
        let c = core
            .create_card(
                CardDraft {
                    title: "Rotate key".into(),
                    status: "review".into(),
                    pr_url: "https://github.com/acme/api/pull/3".into(),
                    ..Default::default()
                },
                "owner",
            )
            .await
            .unwrap();
        core.move_card(&c.id, c.version, "done", "owner")
            .await
            .unwrap();
        let mut tagged = false;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let c = core.card(&c.id).await.unwrap().unwrap();
            if c.labels.contains(&"secrets".to_string()) {
                tagged = true;
                break;
            }
        }
        assert!(tagged);
        assert_eq!(fake.jobs.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn below_the_floor_leaves_the_card_alone() {
        let (core, fake, _d) = setup(answer("SAST", 0.29)).await;
        let c = done_card(&core, "Fix injection").await;
        let out = core.classify_card(&c.id, "owner").await.unwrap();
        assert!(matches!(out, Outcome::Undecided { confidence, .. } if confidence < MIN_PROB));
        let after = core.card(&c.id).await.unwrap().unwrap();
        assert_eq!(after.version, c.version);
        assert_eq!(after.labels, c.labels);
        assert!(class_of(&after).is_none());
        assert_eq!(audits(&core, "undecided").await, 1);
        // An unknown answer is undecided too, and the next ask tries again.
        *fake.reply.lock().unwrap() = answer("WHATEVER", 0.99);
        assert!(matches!(
            core.classify_card(&c.id, "owner").await.unwrap(),
            Outcome::Undecided { .. }
        ));
        assert_eq!(fake.jobs.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn refused_under_the_kill_switch() {
        let (core, fake, _d) = setup(answer("SCA", 0.9)).await;
        let c = done_card(&core, "Bump").await;
        crate::control::pause(&core, "test").await.unwrap();
        let e = core.classify_card(&c.id, "owner").await.unwrap_err();
        assert!(crate::control::is_blocked(&e));
        assert!(fake.jobs.lock().unwrap().is_empty());
        assert_eq!(audits(&core, "refused").await, 1);
        assert!(class_of(&core.card(&c.id).await.unwrap().unwrap()).is_none());
    }

    #[tokio::test]
    async fn refused_under_the_a2a_blackout() {
        let (core, fake, _d) = setup(answer("SCA", 0.9)).await;
        let c = done_card(&core, "Bump").await;
        crate::control::set_a2a_blackout(&core, true, "test", "owner")
            .await
            .unwrap();
        let e = core.classify_card(&c.id, "owner").await.unwrap_err();
        assert!(format!("{e}").contains("A2A withheld"));
        assert!(fake.jobs.lock().unwrap().is_empty());
        assert_eq!(audits(&core, "refused").await, 1);
    }

    #[tokio::test]
    async fn not_eligible_without_a_pull_request() {
        let (core, fake, _d) = setup(answer("SCA", 0.9)).await;
        let c = core
            .create_card(
                CardDraft {
                    title: "No PR".into(),
                    status: "done".into(),
                    ..Default::default()
                },
                "owner",
            )
            .await
            .unwrap();
        assert_eq!(
            core.classify_card(&c.id, "owner").await.unwrap(),
            Outcome::Skipped
        );
        assert!(fake.jobs.lock().unwrap().is_empty());
    }
}

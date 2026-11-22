//! Sessions service: transcript mirroring and per-session inbox.
//! Owner: M1 sessions workstream.

use crate::server::{Hub, authenticate, internal};
use kls_core::sessions::{AuditLine, Entry, InboxBody, InboxItem, Refusal, Session, kinds};
use kls_proto::v1::{self as pb, inbox_item, sessions_server::Sessions};
use std::pin::Pin;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

pub struct Svc {
    pub hub: Hub,
}

type InboxStream = Pin<Box<dyn Stream<Item = Result<pb::InboxItem, Status>> + Send + 'static>>;
type AttachmentStream =
    Pin<Box<dyn Stream<Item = Result<pb::AttachmentPart, Status>> + Send + 'static>>;

/// The wire form of an attachment reference (#53).
pub fn attachment_pb(a: kls_core::attachments::AttachmentRef) -> pb::Attachment {
    pb::Attachment {
        name: a.name,
        mime: a.mime,
        sha256: a.sha256,
        size: a.size as i64,
    }
}

/// Most entries or audit lines one call may carry.
const MAX_BATCH: usize = 1000;

/// Maps a domain error to a gRPC status: refusals keep their meaning,
/// anything else is logged and reported as internal.
pub(crate) fn status(e: anyhow::Error) -> Status {
    match e.downcast_ref::<Refusal>() {
        Some(r @ Refusal::NotFound(_)) => Status::not_found(r.to_string()),
        Some(r @ Refusal::NotOwner) => Status::permission_denied(r.to_string()),
        Some(r @ Refusal::Ended) => Status::failed_precondition(r.to_string()),
        Some(r @ Refusal::Invalid(_)) => Status::invalid_argument(r.to_string()),
        None => internal(e),
    }
}

fn session_of(u: pb::SessionUpsert) -> Session {
    Session {
        id: u.session_id,
        title: u.title,
        cwd: u.cwd,
        repo: u.repo,
        branch: u.branch,
        provider: u.provider,
        model: u.model,
        kind: u.kind,
        state: u.state,
        mode: u.mode,
        agent: u.agent,
        dispatch_id: u.dispatch_id,
        card_id: u.card_id,
        run_id: u.run_id,
        started_at: u.started_at_ms,
        updated_at: u.updated_at_ms,
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cost_usd: if u.cost_usd.is_finite() {
            u.cost_usd
        } else {
            0.0
        },
        pending_question_id: u.pending_question_id,
        pending_question: u.pending_question,
        ..Default::default()
    }
}

pub(crate) fn usage_of(u: &pb::ModelUsage) -> kls_core::belai::ModelUsage {
    kls_core::belai::ModelUsage {
        provider: u.provider.clone(),
        model: u.model.clone(),
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cache_read_tokens: u.cache_read_tokens,
        cache_write_tokens: u.cache_write_tokens,
        cost_usd: u.cost_usd,
    }
}

/// The structured parts of an upsert (WP-A). A host that predates them
/// sends only `pending_question_*`, read here as a free-text ask.
fn extras_of(u: &pb::SessionUpsert) -> kls_core::belai::ExtrasUpdate {
    use kls_core::belai::{Accepts, AskOption, ExtrasUpdate, GitFacts, PendingAsk};
    let pending_ask = match &u.pending_ask {
        Some(a) => Some(PendingAsk {
            id: a.id.clone(),
            kind: a.kind.clone(),
            title: a.title.clone(),
            tool: a.tool.clone(),
            options: a
                .options
                .iter()
                .map(|o| AskOption {
                    id: o.id.clone(),
                    label: o.label.clone(),
                })
                .collect(),
            questions_json: a.questions_json.clone(),
            asked_at: a.asked_at_ms,
            relay_json: a.relay_json.clone(),
        }),
        None if !u.pending_question_id.is_empty() => Some(PendingAsk {
            id: u.pending_question_id.clone(),
            title: u.pending_question.clone(),
            ..Default::default()
        }),
        None => None,
    };
    ExtrasUpdate {
        pending_ask,
        accepts: u.accepts.as_ref().map(|a| Accepts {
            prompts: a.prompts,
            answers: a.answers,
            controls: a.controls,
            shell: a.shell,
            controls_allowed: a.controls_allowed.clone(),
            commands: a.commands.clone(),
        }),
        git: u.git.as_ref().map(|g| GitFacts {
            commit: g.commit.clone(),
            dirty: g.dirty,
            ahead: g.ahead,
            behind: g.behind,
            detached: g.detached,
        }),
        usage: u.usage.iter().map(usage_of).collect(),
        counts: None,
        legacy: Some((u.input_tokens, u.output_tokens, u.cost_usd)),
        harness: (!u.harness.is_empty()).then(|| kls_core::belai::SessionHarness {
            harness: u.harness.clone(),
            version: u.harness_version.clone(),
            session_id: u.harness_session_id.clone(),
            transcript_source: u.transcript_source.clone(),
            end: None,
        }),
        harness_end: None,
        checkpoints: Some(u.checkpoints.iter().map(checkpoint_of).collect()),
        plan_sha256: None,
    }
}

/// The audit key of a host stream; the id is belai's stream id.
fn stream_key(id: &str) -> Result<String, Status> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(Status::invalid_argument(
            "an audit batch names a session or a stream",
        ));
    }
    Ok(format!("stream:{id}"))
}

fn index(i: u64) -> Result<i64, Status> {
    i64::try_from(i).map_err(|_| Status::invalid_argument("index out of range"))
}

/// The protocol form of a stored inbox item.
pub fn inbox_pb(item: &InboxItem) -> anyhow::Result<pb::InboxItem> {
    let body = match InboxBody::of(item)? {
        InboxBody::Prompt { text, attachments } => inbox_item::Body::Prompt(pb::Prompt {
            text,
            attachments: attachments.into_iter().map(attachment_pb).collect(),
        }),
        InboxBody::Answer {
            question_id,
            text,
            decision,
            payload_json,
        } => inbox_item::Body::Answer(pb::Answer {
            question_id,
            text,
            decision,
            payload_json,
        }),
        InboxBody::Command { name, args } => inbox_item::Body::Command(pb::Command { name, args }),
    };
    Ok(pb::InboxItem {
        seq: item.seq as u64,
        id: item.id.clone(),
        session_id: item.session_id.clone(),
        body: Some(body),
    })
}

#[tonic::async_trait]
impl Sessions for Svc {
    async fn upsert(&self, req: Request<pb::SessionUpsert>) -> Result<Response<pb::Ok>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let u = req.into_inner();
        let upd = extras_of(&u);
        let plan = crate::status::plan_limits_of(&u.plan_limits);
        let mut s = session_of(u);
        let sid = s.id.clone();
        let core = self.hub.core();
        // A dispatched session belongs to the dispatch's card and run, even
        // when its host does not say so.
        if !s.dispatch_id.is_empty()
            && (s.card_id.is_empty() || s.run_id.is_empty())
            && let Ok(Some(d)) = core.store().dispatch(&s.dispatch_id).await
            && d.node_id == node
        {
            if s.card_id.is_empty() {
                s.card_id = d.card_id;
            }
            if s.run_id.is_empty() {
                s.run_id = d.run_id;
            }
        }
        core.session_upsert(&node, s).await.map_err(status)?;
        core.session_extras_apply(&node, &sid, upd)
            .await
            .map_err(status)?;
        // Row 48: the plan windows this session last saw.
        core.session_plan_limits_apply(&node, &sid, plan)
            .await
            .map_err(status)?;
        Ok(Response::new(pb::Ok {}))
    }

    async fn append(
        &self,
        req: Request<pb::SessionEntries>,
    ) -> Result<Response<pb::AppendResult>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        if r.entries.len() > MAX_BATCH {
            return Err(Status::invalid_argument("too many entries in one call"));
        }
        let entries = r
            .entries
            .into_iter()
            .map(|e| {
                Ok(Entry {
                    idx: index(e.index)?,
                    role: e.role,
                    text: e.text,
                    at: e.at_ms,
                    meta_json: e.meta_json,
                })
            })
            .collect::<Result<Vec<_>, Status>>()?;
        let out = self
            .hub
            .core()
            .session_append(&node, &r.session_id, &entries)
            .await
            .map_err(status)?;
        Ok(Response::new(pb::AppendResult {
            next_index: out.next_index.max(0) as u64,
        }))
    }

    async fn end(&self, req: Request<pb::SessionEnd>) -> Result<Response<pb::Ok>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        let reason: String = r.reason.chars().take(500).collect();
        self.hub
            .core()
            .session_end(&node, &r.session_id, r.ended_at_ms, &reason)
            .await
            .map_err(status)?;
        let harness_end =
            (r.termination_verified || r.empty_result || r.adapter_error || r.timing.is_some())
                .then(|| {
                    let t = r.timing.unwrap_or_default();
                    kls_store::belai::HarnessEnd {
                        termination_verified: r.termination_verified,
                        empty_result: r.empty_result,
                        adapter_error: r.adapter_error,
                        startup_ms: t.startup_ms,
                        ready_ms: t.ready_ms,
                        first_reply_ms: t.first_reply_ms,
                        wall_ms: t.wall_ms,
                    }
                });
        let plan = (!r.plan_sha256.is_empty()).then(|| r.plan_sha256.clone());
        if !r.usage.is_empty()
            || r.turns > 0
            || r.tool_calls > 0
            || harness_end.is_some()
            || plan.is_some()
        {
            let mut upd = kls_core::belai::ExtrasUpdate {
                usage: r.usage.iter().map(usage_of).collect(),
                counts: (!r.usage.is_empty() || r.turns > 0 || r.tool_calls > 0)
                    .then_some((r.turns as i64, r.tool_calls as i64)),
                harness_end,
                plan_sha256: plan,
                ..Default::default()
            };
            // An end carries no ask; keep the stored (now cleared) state.
            upd.accepts = None;
            self.hub
                .core()
                .session_extras_apply(&node, &r.session_id, upd)
                .await
                .map_err(status)?;
        }
        Ok(Response::new(pb::Ok {}))
    }

    type InboxStream = InboxStream;

    async fn inbox(
        &self,
        req: Request<pb::InboxRequest>,
    ) -> Result<Response<Self::InboxStream>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        let core = self.hub.core().clone();
        core.owned_session(&node, &r.session_id)
            .await
            .map_err(status)?;
        let after = index(r.after)?;
        let sid = r.session_id;
        let (tx, rx) = mpsc::channel::<Result<pb::InboxItem, Status>>(64);
        tokio::spawn(async move {
            // Subscribe before the first read so nothing queued meanwhile is missed.
            let mut bus = core.subscribe();
            let mut last = after;
            loop {
                let items = match core.store().inbox_unacked_after(&sid, last).await {
                    Ok(items) => items,
                    Err(e) => {
                        let _ = tx.send(Err(internal(e))).await;
                        return;
                    }
                };
                for item in items {
                    // Stamped before the send so the stored state never lags what
                    // the session has seen; an unacked item is replayed anyway.
                    if let Err(e) = core.inbox_delivered(&sid, item.seq).await {
                        tracing::warn!("inbox delivered: {e:#}");
                    }
                    match inbox_pb(&item) {
                        Ok(msg) => {
                            if tx.send(Ok(msg)).await.is_err() {
                                return;
                            }
                        }
                        Err(e) => tracing::warn!(item = %item.id, "skipping inbox item: {e:#}"),
                    }
                    last = item.seq;
                }
                loop {
                    tokio::select! {
                        _ = tx.closed() => return,
                        ev = bus.recv() => match ev {
                            Ok(ev) if ev.kind == kinds::SESSION_INBOX && ev.subject == sid => break,
                            Ok(_) => continue,
                            Err(RecvError::Lagged(_)) => break,
                            Err(RecvError::Closed) => return,
                        },
                    }
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn ack(&self, req: Request<pb::InboxAck>) -> Result<Response<pb::Ok>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        self.hub
            .core()
            .inbox_ack(&node, &r.session_id, &r.item_id, &r.status, &r.detail)
            .await
            .map_err(status)?;
        Ok(Response::new(pb::Ok {}))
    }

    type FetchAttachmentStream = AttachmentStream;

    /// #53: an attachment's bytes, for a dispatch to this node or an inbox
    /// item of its session that lists it (`kls_core::attachments::authorize`:
    /// kill switch, ownership, audit). Streamed in 64 KiB parts.
    async fn fetch_attachment(
        &self,
        req: Request<pb::AttachmentRequest>,
    ) -> Result<Response<Self::FetchAttachmentStream>, Status> {
        use tokio::io::AsyncReadExt;
        let node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        let core = self.hub.core().clone();
        let a = kls_core::attachments::authorize(
            &core,
            &node,
            &r.sha256,
            &r.dispatch_id,
            &r.session_id,
            &r.item_id,
        )
        .await
        .map_err(|e| {
            if kls_core::control::is_blocked(&e) {
                Status::failed_precondition(e.to_string())
            } else {
                status(e)
            }
        })?;
        let mut file = core.blobs().open(&a.sha256).await.map_err(internal)?;
        let (tx, rx) = mpsc::channel::<Result<pb::AttachmentPart, Status>>(4);
        tokio::spawn(async move {
            let size = a.size as i64;
            let mut first = true;
            let mut sent = 0u64;
            loop {
                let mut buf = vec![0u8; kls_core::attachments::CHUNK_BYTES];
                let n = match file.read(&mut buf).await {
                    Ok(n) => n,
                    Err(e) => {
                        let _ = tx.send(Err(Status::internal(e.to_string()))).await;
                        return;
                    }
                };
                buf.truncate(n);
                sent += n as u64;
                let last = n == 0 || sent >= a.size;
                let part = pb::AttachmentPart {
                    chunk: buf,
                    size: if first { size } else { 0 },
                    last,
                };
                first = false;
                if tx.send(Ok(part)).await.is_err() || last {
                    return;
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn audit_head(
        &self,
        req: Request<pb::AuditStreamRequest>,
    ) -> Result<Response<pb::AuditStreamHead>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let key = stream_key(&req.into_inner().stream_id)?;
        let last_index = self
            .hub
            .core()
            .store()
            .audit_head(&node, &key)
            .await
            .map_err(internal)?;
        Ok(Response::new(pb::AuditStreamHead { last_index }))
    }

    async fn audit(&self, req: Request<pb::AuditBatch>) -> Result<Response<pb::Ok>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        if r.lines.len() > MAX_BATCH {
            return Err(Status::invalid_argument("too many audit lines in one call"));
        }
        // A host stream (WP-C) is kept under "stream:<id>".
        let key = if r.session_id.is_empty() {
            stream_key(&r.stream_id)?
        } else {
            r.session_id.clone()
        };
        let lines = r
            .lines
            .into_iter()
            .map(|l| {
                Ok(AuditLine {
                    node_id: node.clone(),
                    session_id: key.clone(),
                    idx: index(l.index)?,
                    at: l.at_ms,
                    kind: l.kind,
                    json: l.json,
                    received_at: 0,
                })
            })
            .collect::<Result<Vec<_>, Status>>()?;
        // Stored raw; M6 hash-chains them into the ledger.
        self.hub
            .core()
            .record_audit(&node, &key, &lines)
            .await
            .map_err(status)?;
        Ok(Response::new(pb::Ok {}))
    }
}

/// One checkpoint a session listed (#45).
fn checkpoint_of(c: &pb::Checkpoint) -> kls_core::belai::Checkpoint {
    kls_core::belai::Checkpoint {
        id: c.id.clone(),
        at_ms: c.at_ms,
        label: c.label.clone(),
        entry_index: i64::try_from(c.entry_index).unwrap_or(0),
        entry_id: c.entry_id.clone(),
        files: c.files,
    }
}

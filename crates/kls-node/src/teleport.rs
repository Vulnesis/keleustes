//! Teleport service: session bundles between hosts. Owner: M6 workstream.
//!
//! `Upload` streams a bundle from its origin node: the first part carries
//! the teleport id (empty when the node starts a teleport itself) and the
//! header; chunks follow; the part with `last` ends it. The bytes are
//! hashed as they arrive, capped at the header's size (and at
//! [`TELEPORT_MAX_BYTES`]), checked against the header's SHA-256, and kept
//! in the content-addressed blob store under `<data>/artifacts/`. A failed
//! upload leaves the teleport `requested`, so the node can try again.
//!
//! `Download` streams a ready bundle to the node it is addressed to, and
//! only that node, until the teleport expires.

use crate::server::{Hub, authenticate, internal};
use kls_core::Core;
use kls_core::artifacts::BlobError;
use kls_core::audit::{
    HARNESS_TELEPORT_REFUSAL, TELEPORT_MAX_BYTES, TeleportHeader, TeleportRefusal,
};
use kls_core::gateway::LibraryRef;
use kls_core::hosts::{self, Origin};
use kls_proto::v1::{self as pb, teleport_server::Teleport};
use std::pin::Pin;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

pub struct Svc {
    pub hub: Hub,
}

/// Bytes per download part.
pub const CHUNK: usize = 256 * 1024;

type PartStream = Pin<Box<dyn Stream<Item = Result<pb::TeleportPart, Status>> + Send + 'static>>;

/// Maps a domain error to a gRPC status.
fn status(e: anyhow::Error) -> Status {
    if let Some(r) = e.downcast_ref::<TeleportRefusal>() {
        return match r {
            TeleportRefusal::NotFound => Status::not_found(r.to_string()),
            TeleportRefusal::NotAddressed => Status::permission_denied(r.to_string()),
            TeleportRefusal::WrongState(_) => Status::failed_precondition(r.to_string()),
            TeleportRefusal::Invalid(_) => Status::invalid_argument(r.to_string()),
        };
    }
    if let Some(b) = e.downcast_ref::<BlobError>() {
        return match b {
            BlobError::TooLarge { .. } => Status::resource_exhausted(b.to_string()),
            BlobError::Mismatch { .. } => Status::data_loss(b.to_string()),
        };
    }
    internal(e)
}

/// Most profiles and crews one teleport names (belai's maxTeleportBackups).
pub const MAX_NEEDS: usize = 24;

/// The settings key a teleport's needs are kept under (the teleport table
/// has no column for them).
pub fn needs_key(teleport_id: &str) -> String {
    format!("teleport.needs.{teleport_id}")
}

/// A profile or crew name as belai writes one: letters, digits and
/// `-_.`, at most 64 characters. Anything else is dropped.
fn need_name(s: &str) -> Option<String> {
    let s = s.trim();
    let ok = !s.is_empty()
        && s.len() <= 64
        && !s.starts_with(['.', '-'])
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    ok.then(|| s.to_string())
}

/// The profiles and crews a teleported session needs: the header's (kind
/// `profile` or `crew`, names only, never a built-in) plus the session's own
/// agent profile, de-duplicated and capped at [`MAX_NEEDS`]. The header is
/// the origin node's and untrusted, so ids, versions and hashes from it are
/// dropped; Keleustes resolves them against its own library.
pub fn clean_needs(header: &[pb::LibraryRef], session_agent: &str) -> Vec<LibraryRef> {
    let mut out: Vec<LibraryRef> = Vec::new();
    let own =
        (!session_agent.is_empty()).then(|| ("profile".to_string(), session_agent.to_string()));
    let named = header.iter().map(|r| (r.kind.clone(), r.name.clone()));
    for (kind, name) in own.into_iter().chain(named) {
        let kind = match kind.as_str() {
            "profile" | "agent" => "profile",
            "crew" => "crew",
            _ => continue,
        };
        if name.trim().starts_with("belai:") {
            continue; // built-ins ship with belai
        }
        let Some(name) = need_name(&name) else {
            continue;
        };
        if out.iter().any(|r| r.kind == kind && r.name == name) {
            continue;
        }
        if out.len() >= MAX_NEEDS {
            break;
        }
        out.push(LibraryRef {
            kind: kind.into(),
            name,
            ..Default::default()
        });
    }
    out
}

/// Who a teleport's library dispatches come from: the person who asked for
/// the teleport, through the teleport (rule 1).
pub fn teleport_origin(teleport_id: &str, why: &str) -> Origin {
    Origin {
        actor: "owner".into(),
        surface: format!("teleport:{teleport_id}"),
        reason: why.into(),
        ..Default::default()
    }
}

/// Keeps a stored teleport's needs and asks its origin to back them up
/// into the library (belai's `teleport_backup`), so the target can install
/// them. A refusal (kill switch, a node that does not take the dispatch) is
/// audited by `library_dispatch` and never fails the upload.
async fn record_needs(core: &Core, t: &kls_store::audit::Teleport, needs: &[LibraryRef]) {
    if needs.is_empty() {
        return;
    }
    let json = serde_json::to_string(needs).unwrap_or_default();
    if let Err(e) = core.store().set_setting(&needs_key(&t.id), &json).await {
        tracing::warn!(teleport = %t.id, "record teleport needs: {e:#}");
        return;
    }
    match hosts::library_dispatch(
        core,
        &t.from_node,
        "library_backup",
        needs.to_vec(),
        false,
        teleport_origin(&t.id, "teleport needs: back up"),
    )
    .await
    {
        Ok(id) => tracing::info!(teleport = %t.id, dispatch = %id, "teleport needs backup asked"),
        Err(e) => tracing::info!(teleport = %t.id, "teleport needs backup refused: {e:#}"),
    }
}

/// The needs stored for a teleport.
pub async fn stored_needs(core: &Core, teleport_id: &str) -> Vec<LibraryRef> {
    core.store()
        .setting(&needs_key(teleport_id))
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Each need as the library holds it (id, latest version, SHA-256 of the
/// document); a need the library lacks keeps its name only.
pub async fn resolve_needs(core: &Core, needs: &[LibraryRef]) -> Vec<LibraryRef> {
    let mut out = Vec::new();
    for n in needs {
        let kind = if n.kind == "profile" {
            kls_core::vault::AGENT_KIND
        } else {
            n.kind.as_str()
        };
        let mut r = n.clone();
        if let Ok(Some(item)) = core.store().library_item_by_name(kind, &n.name).await
            && let Ok(vs) = core.store().library_versions(&item.id).await
            && let Some(v) = vs.into_iter().max_by_key(|v| v.version)
        {
            r.id = item.id;
            r.version = v.version;
            r.sha256 = kls_core::belai::library::sha256_hex(v.body.as_bytes());
        }
        out.push(r);
    }
    out
}

/// After the target took the whole bundle: installs the needs the library
/// holds there with a `library_install` dispatch (the A2A blackout and the
/// kill switch are enforced, and every outcome audited, by
/// `library_dispatch`). A need the library lacks is left to the session.
async fn install_needs(core: &Core, t: &kls_store::audit::Teleport, needs: Vec<LibraryRef>) {
    let held: Vec<LibraryRef> = needs.into_iter().filter(|n| !n.id.is_empty()).collect();
    if held.is_empty() {
        return;
    }
    match hosts::library_dispatch(
        core,
        &t.to_node,
        "library_install",
        held,
        false,
        teleport_origin(&t.id, "teleport needs: install"),
    )
    .await
    {
        Ok(id) => tracing::info!(teleport = %t.id, dispatch = %id, "teleport needs install sent"),
        Err(e) => tracing::info!(teleport = %t.id, "teleport needs install refused: {e:#}"),
    }
}

fn ref_pb(r: LibraryRef) -> pb::LibraryRef {
    pb::LibraryRef {
        kind: r.kind,
        id: r.id,
        name: r.name,
        version: r.version,
        sha256: r.sha256,
    }
}

fn header_of(h: &pb::TeleportHeader) -> TeleportHeader {
    TeleportHeader {
        session_id: h.session_id.clone(),
        from_node: h.from_node.clone(),
        to_node: h.to_node.clone(),
        title: h.title.clone(),
        size: h.size,
        sha256: h.sha256.to_ascii_lowercase(),
    }
}

#[tonic::async_trait]
impl Teleport for Svc {
    async fn upload(
        &self,
        req: Request<Streaming<pb::TeleportPart>>,
    ) -> Result<Response<pb::TeleportReceipt>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let core = self.hub.core().clone();
        let mut parts = req.into_inner();
        let first = parts
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("empty upload"))?;
        let h = first
            .header
            .as_ref()
            .map(header_of)
            .ok_or_else(|| Status::invalid_argument("the first part carries the header"))?;
        let header_needs = first
            .header
            .as_ref()
            .map(|x| x.needs.clone())
            .unwrap_or_default();
        // C4.7: a third-party harness session never travels, whoever asks.
        let session_id = if first.teleport_id.is_empty() {
            h.session_id.clone()
        } else {
            core.store()
                .teleport(&first.teleport_id)
                .await
                .map_err(internal)?
                .map(|t| t.session_id)
                .unwrap_or_default()
        };
        let session = if session_id.is_empty() {
            None
        } else {
            core.store().session(&session_id).await.map_err(internal)?
        };
        if let Some(s) = session.as_ref().filter(|s| s.kind == "harness") {
            let e = core
                .teleport_refuse_harness(&s.id, &node, &h.to_node, &format!("node:{node}"))
                .await;
            if !first.teleport_id.is_empty() {
                let _ = core
                    .teleport_fail(&first.teleport_id, HARNESS_TELEPORT_REFUSAL)
                    .await;
            }
            return Err(status(e));
        }
        let t = core
            .teleport_begin_upload(&node, &first.teleport_id, &h)
            .await
            .map_err(status)?;
        let cap = if h.size > 0 {
            h.size as u64
        } else {
            TELEPORT_MAX_BYTES
        };
        let mut w = core.blobs().writer(cap).await.map_err(internal)?;
        let mut next = Some(first);
        let mut done = false;
        let r: Result<(), Status> = async {
            loop {
                let p = match next.take() {
                    Some(p) => p,
                    None => match parts.message().await? {
                        Some(p) => p,
                        None => break,
                    },
                };
                if !p.teleport_id.is_empty() && p.teleport_id != t.id {
                    return Err(Status::invalid_argument("parts name another teleport"));
                }
                if !p.chunk.is_empty() {
                    w.write(&p.chunk).await.map_err(status)?;
                }
                if p.last {
                    done = true;
                    break;
                }
            }
            if !done {
                return Err(Status::invalid_argument(
                    "upload ended before the last part",
                ));
            }
            if h.size > 0 && w.size() != h.size as u64 {
                return Err(Status::invalid_argument(format!(
                    "bundle is {} bytes, header said {}",
                    w.size(),
                    h.size
                )));
            }
            Ok(())
        }
        .await;
        if let Err(e) = r {
            w.abort().await;
            return Err(e);
        }
        let blob = w
            .finish(Some(h.sha256.as_str()).filter(|s| !s.is_empty()))
            .await
            .map_err(status)?;
        let t = core
            .teleport_uploaded(&t.id, &blob, &h.title)
            .await
            .map_err(status)?;
        tracing::info!(teleport = %t.id, size = t.size, to = %t.to_node, "teleport bundle stored");
        let agent = session.map(|s| s.agent).unwrap_or_default();
        let needs = clean_needs(&header_needs, &agent);
        record_needs(&core, &t, &needs).await;
        Ok(Response::new(pb::TeleportReceipt {
            teleport_id: t.id,
            size: t.size,
        }))
    }

    type DownloadStream = PartStream;

    async fn download(
        &self,
        req: Request<pb::TeleportRequest>,
    ) -> Result<Response<Self::DownloadStream>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let id = req.into_inner().teleport_id;
        let core = self.hub.core().clone();
        let t = core
            .teleport_for_download(&node, &id)
            .await
            .map_err(status)?;
        let mut file = core
            .blobs()
            .open(&t.sha256)
            .await
            .map_err(|_| Status::not_found("the bundle is gone"))?;
        let needs = resolve_needs(&core, &stored_needs(&core, &t.id).await).await;
        let first = t.status == "ready";
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            let mut header = Some(pb::TeleportHeader {
                session_id: t.session_id.clone(),
                from_node: t.from_node.clone(),
                to_node: t.to_node.clone(),
                title: t.title.clone(),
                size: t.size,
                sha256: t.sha256.clone(),
                // The profile and crews the session ran under, as the
                // library holds them (#17).
                needs: needs.iter().cloned().map(ref_pb).collect(),
            });
            let mut left = t.size.max(0) as u64;
            let mut buf = vec![0u8; CHUNK];
            loop {
                let want = (left as usize).min(CHUNK);
                let mut got = 0;
                while got < want {
                    match file.read(&mut buf[got..want]).await {
                        Ok(0) => break,
                        Ok(n) => got += n,
                        Err(e) => {
                            let _ = tx
                                .send(Err(Status::internal(format!("read bundle: {e}"))))
                                .await;
                            return;
                        }
                    }
                }
                if got < want {
                    let _ = tx
                        .send(Err(Status::data_loss(
                            "the bundle is shorter than recorded",
                        )))
                        .await;
                    return;
                }
                left -= got as u64;
                let last = left == 0;
                let part = pb::TeleportPart {
                    teleport_id: t.id.clone(),
                    header: header.take(),
                    chunk: buf[..got].to_vec(),
                    last,
                };
                if tx.send(Ok(part)).await.is_err() {
                    return; // the node went away
                }
                if last {
                    break;
                }
            }
            if let Err(e) = core.teleport_downloaded(&t.id).await {
                tracing::warn!(teleport = %t.id, "record download: {e:#}");
            }
            // Once, on the first whole delivery; a repeat download reinstalls nothing.
            if first {
                install_needs(&core, &t, needs).await;
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

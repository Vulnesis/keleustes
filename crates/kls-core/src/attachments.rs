//! Attachments on remote prompts and session dispatches (belai-counterparts
//! #53), and the files an agent profile carries (sf-parity "Builder extras:
//! files"). Owner: agent B (2026-11-22).
//!
//! **Prompt and dispatch attachments.** The bytes are stored once in the
//! content-addressed blob store (`<data>/artifacts`, [`crate::artifacts`]);
//! a prompt or a dispatch carries references only ([`AttachmentRef`]: name,
//! media type, SHA-256, size), so neither the inbox nor the node outbox ever
//! holds the bytes. The host reads each one with `Sessions.FetchAttachment`,
//! which serves only a hash that a dispatch to that node, or an inbox item of
//! that node's session, lists ([`authorize`]), and nothing while the factory
//! is paused or stopped. Every queue and every read is audited with the hash,
//! the media type and the size, never the bytes.
//!
//! Caps (belai enforces the same, `internal/rc/attachments.go`):
//! [`MAX_ATTACHMENTS`] per prompt or dispatch; a PNG or JPEG image at most
//! [`MAX_IMAGE_BYTES`]; a text file (UTF-8, no NUL byte) at most
//! [`MAX_TEXT_BYTES`]; [`MAX_TOTAL_BYTES`] in all. Anything else is refused
//! here, before it is stored.
//!
//! **Agent files.** Text files attached to a profile in the builder (the
//! website's "Agent files"): at most [`AGENT_FILE_MAX_BYTES`] a file,
//! [`AGENT_FILES_MAX`] files and [`AGENT_FILES_TOTAL_MAX`] a profile, a path
//! that cannot climb out of where the host keeps it, and nothing that holds a
//! private key or a known token. A publish copies them into the library
//! version, and `Library.Fetch` ships them with the profile; belai writes
//! them into the profile's own directory (`agentfiles.Install`).

use crate::Core;
use crate::artifacts::is_sha256;
use crate::control;
use crate::gateway::DispatchSpec;
use crate::hosts::Origin;
use crate::sessions::{InboxBody, InboxItem, Refusal};
use anyhow::{Context, Result, anyhow, bail};
use kls_store::attach::AgentFile;
use kls_store::audit::LedgerDraft;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Most attachments on one prompt or dispatch (belai's eight images).
pub const MAX_ATTACHMENTS: usize = 8;
/// Largest image (the host reduces it with imageguard).
pub const MAX_IMAGE_BYTES: u64 = 8 << 20;
/// Largest text file (belai reads an `@` file up to 64 KiB).
pub const MAX_TEXT_BYTES: u64 = 64 << 10;
/// All attachments of one prompt or dispatch together.
pub const MAX_TOTAL_BYTES: u64 = 16 << 20;
/// Longest attachment name.
pub const MAX_NAME_BYTES: usize = 255;
/// Size of one `AttachmentPart` chunk.
pub const CHUNK_BYTES: usize = 64 << 10;
/// The `Capabilities.dispatch_features` entry a host lists to take them.
pub const FEATURE: &str = "attachments";

pub const MIME_PNG: &str = "image/png";
pub const MIME_JPEG: &str = "image/jpeg";
pub const MIME_TEXT: &str = "text/plain";

/// Largest agent file (belai `agentfiles.MaxFileBytes`).
pub const AGENT_FILE_MAX_BYTES: usize = 256 << 10;
/// Most files one agent carries.
pub const AGENT_FILES_MAX: usize = 32;
/// All of one agent's files together.
pub const AGENT_FILES_TOTAL_MAX: usize = 2 << 20;
/// Longest listed path.
pub const AGENT_FILE_PATH_MAX: usize = 1024;

/// Event kinds of this module.
pub mod kinds {
    /// Bytes were stored as an attachment. Data: `{sha256, name, mime, size, origin}`.
    pub const ATTACHMENT_STORED: &str = "attachment.stored";
    /// A node read an attachment. Data: `{sha256, nodeId, dispatchId, sessionId, itemId}`.
    pub const ATTACHMENT_SERVED: &str = "attachment.served";
    /// An attachment was refused. Data: `{reason, origin}`.
    pub const ATTACHMENT_REFUSED: &str = "attachment.refused";
    /// An agent file was added or removed. Data: `{agentId, path, sha256, size, op, origin}`.
    pub const AGENT_FILE: &str = "agent.file";
}

/// One attachment, by reference (the protocol's `Attachment`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRef {
    pub name: String,
    pub mime: String,
    pub sha256: String,
    pub size: u64,
}

/// Whether `b` is text a model may read: UTF-8 with no NUL byte.
pub fn is_text(b: &[u8]) -> bool {
    !b.is_empty() && !b.contains(&0) && std::str::from_utf8(b).is_ok()
}

/// The media type of `b` from its first bytes: PNG, JPEG or text; `None`
/// for anything else.
pub fn sniff(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(MIME_PNG)
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(MIME_JPEG)
    } else if is_text(b) {
        Some(MIME_TEXT)
    } else {
        None
    }
}

/// The cap for a media type, or `None` when it is not one we send.
pub fn cap_for(mime: &str) -> Option<u64> {
    match mime {
        MIME_PNG | MIME_JPEG => Some(MAX_IMAGE_BYTES),
        MIME_TEXT => Some(MAX_TEXT_BYTES),
        _ => None,
    }
}

/// The display name for an attachment: the last path segment, one line,
/// at most [`MAX_NAME_BYTES`].
pub fn clean_name(raw: &str) -> Result<String, String> {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if base.is_empty() || base == "." || base == ".." {
        return Err("an attachment needs a file name".into());
    }
    if base.len() > MAX_NAME_BYTES {
        return Err(format!(
            "an attachment name is at most {MAX_NAME_BYTES} bytes"
        ));
    }
    if base.chars().any(|c| c.is_control()) {
        return Err("an attachment name is one line of text".into());
    }
    Ok(base.to_string())
}

/// Checks a list of references against the caps.
pub fn check_list(list: &[AttachmentRef]) -> Result<(), String> {
    if list.len() > MAX_ATTACHMENTS {
        return Err(format!("at most {MAX_ATTACHMENTS} attachments"));
    }
    let mut total = 0u64;
    let mut seen = std::collections::BTreeSet::new();
    for a in list {
        if clean_name(&a.name).as_deref() != Ok(a.name.as_str()) {
            return Err(format!("{:?} is not an attachment name", a.name));
        }
        let cap = cap_for(&a.mime).ok_or_else(|| {
            format!(
                "{}: only PNG and JPEG images and UTF-8 text files can be attached",
                a.name
            )
        })?;
        if a.size == 0 || a.size > cap {
            return Err(format!("{} is larger than {} KiB", a.name, cap >> 10));
        }
        if !is_sha256(&a.sha256) {
            return Err(format!("{} has no SHA-256", a.name));
        }
        if !seen.insert(a.sha256.as_str()) {
            return Err(format!("{} is attached twice", a.name));
        }
        total += a.size;
    }
    if total > MAX_TOTAL_BYTES {
        return Err(format!(
            "attachments total at most {} MiB",
            MAX_TOTAL_BYTES >> 20
        ));
    }
    Ok(())
}

/// The references for an audit line or an event (no bytes).
pub fn audit_json(list: &[AttachmentRef]) -> Value {
    json!(list)
}

fn sha256_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

async fn audit(core: &Core, d: LedgerDraft) {
    // Always audited: retry a write that raced another.
    for i in 1..=5u64 {
        match core.audit_record(d.clone()).await {
            Ok(_) => break,
            Err(e) if i == 5 => tracing::warn!("audit of an attachment: {e:#}"),
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(40 * i)).await,
        }
    }
}

async fn refused(core: &Core, subject: &str, origin: &str, why: &str, node_id: &str) {
    let _ = core
        .emit(
            kinds::ATTACHMENT_REFUSED,
            subject,
            &json!({ "reason": why, "origin": origin }),
        )
        .await;
    audit(
        core,
        LedgerDraft {
            kind: "attachment".into(),
            actor: origin.into(),
            actor_kind: "rule".into(),
            msg: format!("attachment refused for {subject}: {why}"),
            subject: json!({ "target": subject }),
            payload: json!({ "outcome": "refused", "reason": why, "origin": origin }),
            node_id: node_id.into(),
            ..Default::default()
        },
    )
    .await;
}

/// Stores `bytes` as an attachment named `name` (its last path segment).
/// Refused when it is not a PNG, a JPEG or UTF-8 text, or past its cap.
pub async fn store_bytes(
    core: &Core,
    name: &str,
    bytes: &[u8],
    origin: &Origin,
) -> Result<AttachmentRef> {
    let name = clean_name(name).map_err(|e| anyhow!(e))?;
    let Some(mime) = sniff(bytes) else {
        bail!("{name}: only PNG and JPEG images and UTF-8 text files can be attached");
    };
    let cap = cap_for(mime).unwrap_or(0);
    if bytes.len() as u64 > cap {
        bail!("{name} is larger than {} KiB", cap >> 10);
    }
    let blob = core.blobs().put(bytes).await?;
    let a = AttachmentRef {
        name,
        mime: mime.into(),
        sha256: blob.sha256,
        size: blob.size,
    };
    core.emit(
        kinds::ATTACHMENT_STORED,
        &a.sha256,
        &json!({ "sha256": a.sha256, "name": a.name, "mime": a.mime, "size": a.size,
                 "origin": origin.label() }),
    )
    .await?;
    Ok(a)
}

/// Reads the file at `path` (a regular file, within the largest cap) and
/// stores it with [`store_bytes`].
pub async fn store_file(core: &Core, path: &Path, origin: &Origin) -> Result<AttachmentRef> {
    let meta = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    if !meta.is_file() {
        bail!("{} is not a file", path.display());
    }
    if meta.len() > MAX_IMAGE_BYTES {
        bail!(
            "{} is larger than {} MiB",
            path.display(),
            MAX_IMAGE_BYTES >> 20
        );
    }
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    store_bytes(core, &name, &bytes, origin).await
}

/// Every reference names a blob held here, at its listed size.
pub async fn check_stored(core: &Core, list: &[AttachmentRef]) -> Result<(), String> {
    let blobs = core.blobs();
    for a in list {
        let held = match blobs.path(&a.sha256) {
            Ok(p) => tokio::fs::metadata(p).await.ok().map(|m| m.len()),
            Err(_) => None,
        };
        if held != Some(a.size) {
            return Err(format!("{} is not stored here", a.name));
        }
    }
    Ok(())
}

/// Checks a dispatch's attachments before it leaves: the caps, the blobs,
/// a session dispatch only. `Ok` for none.
pub async fn check_spec(core: &Core, spec: &DispatchSpec) -> Result<(), String> {
    if spec.attachments.is_empty() {
        return Ok(());
    }
    if !matches!(spec.kind.as_str(), "" | "session") {
        return Err("attachments apply to a session dispatch".into());
    }
    if spec.prompt.trim().is_empty() {
        return Err("attachments ride on a prompt".into());
    }
    check_list(&spec.attachments)?;
    check_stored(core, &spec.attachments).await
}

/// Queues a prompt with `list` attached for `origin`. With nothing attached
/// it is [`Core::queue_prompt_from`]. Refused (audited) under the kill
/// switch, under the A2A blackout for any origin but the owner, past the caps, for a blob not held here, and for a session whose
/// host does not take attachments.
pub async fn queue_prompt(
    core: &Core,
    session_id: &str,
    text: &str,
    list: Vec<AttachmentRef>,
    origin: &str,
) -> Result<InboxItem> {
    if list.is_empty() {
        return core.queue_prompt_from(session_id, text, origin).await;
    }
    control::check(core, "prompt", session_id, origin).await?;
    // Anything but the owner sending files to a session is agent-to-agent:
    // held by the A2A blackout as well.
    let owner = origin == "owner" || origin.starts_with("owner:") || origin.starts_with("owner@");
    if !owner && let Err(e) = control::check_a2a(core, origin, session_id, origin).await {
        refused(core, session_id, origin, &e.to_string(), "").await;
        return Err(e);
    }
    let s = core
        .store()
        .session(session_id)
        .await?
        .ok_or_else(|| anyhow::Error::new(Refusal::NotFound("session")))?;
    let why = match check_list(&list) {
        Err(why) => Some(why),
        Ok(()) => check_stored(core, &list).await.err(),
    };
    let why = match why {
        Some(w) => Some(w),
        None => {
            let takes = core
                .store()
                .node(&s.node_id)
                .await?
                .is_some_and(|n| n.host.capabilities.takes_feature(FEATURE));
            (!takes).then(|| {
                "the session's host does not take attachments (update Belai on the host)".into()
            })
        }
    };
    if let Some(why) = why {
        refused(core, session_id, origin, &why, &s.node_id).await;
        return Err(anyhow::Error::new(Refusal::Invalid(why)));
    }
    let item = core
        .queue_inbox(
            session_id,
            InboxBody::Prompt {
                text: text.to_string(),
                attachments: list.clone(),
            },
        )
        .await?;
    audit(
        core,
        LedgerDraft {
            kind: "attachment".into(),
            actor: origin.into(),
            actor_kind: if origin.starts_with("owner") {
                "user"
            } else {
                "agent"
            }
            .into(),
            msg: format!(
                "prompt with {} attachment(s) queued for session {session_id} by {origin}",
                list.len()
            ),
            subject: json!({ "session": session_id, "item": item.id }),
            payload: json!({
                "outcome": "queued", "attachments": audit_json(&list),
                "textSha256": sha256_hex(text.as_bytes()), "origin": origin,
            }),
            node_id: s.node_id.clone(),
            session_id: session_id.into(),
            ..Default::default()
        },
    )
    .await;
    Ok(item)
}

/// What `node` may read: the attachment `sha256` that the dispatch
/// `dispatch_id` to it, or the inbox item `item_id` of its session
/// `session_id`, lists. Refused while the factory is paused or stopped.
/// Every read is audited.
pub async fn authorize(
    core: &Core,
    node: &str,
    sha256: &str,
    dispatch_id: &str,
    session_id: &str,
    item_id: &str,
) -> Result<AttachmentRef> {
    if !is_sha256(sha256) {
        return Err(anyhow::Error::new(Refusal::Invalid(
            "that is not a sha256".into(),
        )));
    }
    let origin = format!("host:{node}");
    if let Err(e) = control::check(core, "attachment", sha256, &origin).await {
        refused(core, sha256, &origin, &e.to_string(), node).await;
        return Err(e);
    }
    let store = core.store();
    let listed = if !dispatch_id.is_empty() {
        let d = store
            .dispatch(dispatch_id)
            .await?
            .ok_or_else(|| anyhow::Error::new(Refusal::NotFound("dispatch")))?;
        if d.node_id != node {
            return Err(anyhow::Error::new(Refusal::NotOwner));
        }
        serde_json::from_str::<DispatchSpec>(&d.spec_json)
            .map(|s| s.attachments)
            .unwrap_or_default()
    } else {
        core.owned_session(node, session_id).await?;
        let item = store
            .inbox_item(item_id)
            .await?
            .ok_or_else(|| anyhow::Error::new(Refusal::NotFound("inbox item")))?;
        if item.session_id != session_id {
            return Err(anyhow::Error::new(Refusal::NotOwner));
        }
        match InboxBody::of(&item)? {
            InboxBody::Prompt { attachments, .. } => attachments,
            _ => Vec::new(),
        }
    };
    let a = listed
        .into_iter()
        .find(|a| a.sha256 == sha256)
        .ok_or_else(|| anyhow::Error::new(Refusal::NotFound("attachment")))?;
    if !core.blobs().exists(sha256).await {
        return Err(anyhow::Error::new(Refusal::NotFound("attachment")));
    }
    let data = json!({ "sha256": sha256, "nodeId": node, "dispatchId": dispatch_id,
                       "sessionId": session_id, "itemId": item_id });
    core.emit(kinds::ATTACHMENT_SERVED, sha256, &data).await?;
    audit(
        core,
        LedgerDraft {
            kind: "attachment".into(),
            actor: origin.clone(),
            actor_kind: "host".into(),
            msg: format!("{} ({}, {} B) read by {node}", a.name, a.mime, a.size),
            subject: json!({ "sha256": sha256, "dispatch": dispatch_id, "session": session_id, "item": item_id }),
            payload: json!({ "outcome": "served", "attachment": a }),
            origin: origin.clone(),
            node_id: node.into(),
            session_id: session_id.into(),
            ..Default::default()
        },
    )
    .await;
    Ok(a)
}

// ---- agent files ----------------------------------------------------------------

/// Whether `p` is a path an agent may list for a file (belai
/// `knowledge.OwnedRelPath`): one line, relative, under `~/` or absolute, no
/// `..` segment, naming something, at most [`AGENT_FILE_PATH_MAX`] bytes.
pub fn valid_agent_path(p: &str) -> bool {
    if p.is_empty()
        || p != p.trim()
        || p.len() > AGENT_FILE_PATH_MAX
        || p.chars().any(|c| c.is_control())
        || p.contains('\\')
    {
        return false;
    }
    if p.split('/').any(|s| s == "..") {
        return false;
    }
    let rest = p
        .strip_prefix("~/")
        .unwrap_or(p)
        .trim_start_matches('/')
        .trim_end_matches('/');
    !rest.split('/').all(|s| s.is_empty() || s == ".")
}

/// A listed path as compared: duplicate slashes and `./` folded, no
/// trailing slash.
pub fn clean_agent_path(p: &str) -> String {
    let (lead, rest) = if let Some(r) = p.strip_prefix("~/") {
        ("~/", r)
    } else if let Some(r) = p.strip_prefix('/') {
        ("/", r)
    } else {
        ("", p)
    };
    let segs: Vec<&str> = rest
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    format!("{lead}{}", segs.join("/"))
}

/// An agent's files, by path.
pub async fn agent_files(core: &Core, agent_id: &str) -> Result<Vec<AgentFile>> {
    core.store().agent_files(agent_id).await
}

async fn agent_file_audit(
    core: &Core,
    f: &AgentFile,
    op: &str,
    origin: &Origin,
    outcome: &str,
    why: &str,
) {
    let data = json!({
        "agentId": f.agent_id, "path": f.path, "sha256": f.sha256, "size": f.size_bytes,
        "op": op, "origin": origin, "outcome": outcome, "reason": why,
    });
    let _ = core.emit(kinds::AGENT_FILE, &f.agent_id, &data).await;
    audit(
        core,
        LedgerDraft {
            kind: "agent".into(),
            actor: origin.actor.clone(),
            actor_kind: if outcome == "refused" { "rule" } else { "user" }.into(),
            msg: format!(
                "agent file {op} {} on {} by {} outcome={outcome}",
                f.path,
                f.agent_id,
                origin.label()
            ),
            subject: json!({ "agent": f.agent_id, "path": f.path }),
            payload: data,
            ..Default::default()
        },
    )
    .await;
}

/// Attaches `content` to the agent at `path` (replacing a file there).
/// Text only, within the caps, no private key or token. Audited either way.
pub async fn add_agent_file(
    core: &Core,
    agent_id: &str,
    path: &str,
    content: &[u8],
    origin: &Origin,
) -> Result<AgentFile> {
    let path = clean_agent_path(path.trim());
    let mut f = AgentFile {
        agent_id: agent_id.to_string(),
        path: path.clone(),
        sha256: sha256_hex(content),
        size_bytes: content.len() as i64,
        added_by: origin.label(),
        added_at: 0,
    };
    let why = async {
        if core.store().agent_profile(agent_id).await?.is_none() {
            return Ok::<_, anyhow::Error>(Some("no such agent".to_string()));
        }
        if !valid_agent_path(&path) {
            return Ok(Some(format!(
                "{path:?} is not a path a file can be kept under (one line, no \"..\")"
            )));
        }
        if !is_text(content) {
            return Ok(Some(format!("{path} is not UTF-8 text")));
        }
        if content.len() > AGENT_FILE_MAX_BYTES {
            return Ok(Some(format!(
                "{path} is larger than {} KiB",
                AGENT_FILE_MAX_BYTES >> 10
            )));
        }
        if kls_libsearch::holds_secret(std::str::from_utf8(content).unwrap_or("")) {
            return Ok(Some(format!(
                "{path} holds a private key or a token, so it is not kept"
            )));
        }
        let others: Vec<AgentFile> = core
            .store()
            .agent_files(agent_id)
            .await?
            .into_iter()
            .filter(|o| o.path != path)
            .collect();
        if others.len() + 1 > AGENT_FILES_MAX {
            return Ok(Some(format!(
                "an agent carries at most {AGENT_FILES_MAX} files"
            )));
        }
        let total: i64 = others.iter().map(|o| o.size_bytes).sum::<i64>() + f.size_bytes;
        if total as usize > AGENT_FILES_TOTAL_MAX {
            return Ok(Some(format!(
                "an agent's files total at most {} MiB",
                AGENT_FILES_TOTAL_MAX >> 20
            )));
        }
        Ok(None)
    }
    .await?;
    if let Some(why) = why {
        agent_file_audit(core, &f, "add", origin, "refused", &why).await;
        bail!("{why}");
    }
    let blob = core.blobs().put(content).await?;
    f.sha256 = blob.sha256;
    let saved = core.store().put_agent_file(&f).await?;
    agent_file_audit(core, &saved, "add", origin, "saved", "").await;
    Ok(saved)
}

/// Reads a local file and attaches it at `path` (its file name when empty).
pub async fn add_agent_file_from(
    core: &Core,
    agent_id: &str,
    file: &Path,
    path: &str,
    origin: &Origin,
) -> Result<AgentFile> {
    let meta = tokio::fs::metadata(file)
        .await
        .with_context(|| format!("read {}", file.display()))?;
    if !meta.is_file() || meta.len() as usize > AGENT_FILE_MAX_BYTES {
        bail!(
            "{} is not a file of at most {} KiB",
            file.display(),
            AGENT_FILE_MAX_BYTES >> 10
        );
    }
    let content = tokio::fs::read(file).await?;
    let path = if path.trim().is_empty() {
        file.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        path.to_string()
    };
    add_agent_file(core, agent_id, &path, &content, origin).await
}

/// Removes the file at `path`. Allowed under the kill switch (it only takes
/// something away); audited.
pub async fn remove_agent_file(
    core: &Core,
    agent_id: &str,
    path: &str,
    origin: &Origin,
) -> Result<bool> {
    let Some(f) = core
        .store()
        .agent_files(agent_id)
        .await?
        .into_iter()
        .find(|f| f.path == path)
    else {
        return Ok(false);
    };
    let gone = core.store().delete_agent_file(agent_id, path).await?;
    if gone {
        agent_file_audit(core, &f, "remove", origin, "removed", "").await;
    }
    Ok(gone)
}

/// The agent's files with their bytes, each checked against its hash, for
/// the library version a publish saves.
pub async fn agent_file_bundle(core: &Core, agent_id: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let blobs = core.blobs();
    let mut out = Vec::new();
    for f in core.store().agent_files(agent_id).await? {
        let data = blobs
            .read(&f.sha256)
            .await
            .with_context(|| format!("agent file {}", f.path))?;
        if sha256_hex(&data) != f.sha256 {
            bail!("agent file {} does not match its hash", f.path);
        }
        out.push((f.path, data));
    }
    Ok(out)
}

#[cfg(test)]
mod tests;

//! Library distribution (WP-E). A `library_install` or `library_backup`
//! dispatch names items by reference; the node fetches each document with
//! `Library.Fetch`, which is answered only for a dispatch addressed to that
//! node, recent, and naming the item (the website's rule). A backup is
//! stored only for a backup dispatch that named it. `Reconcile` compares a
//! host's hashes with the library and never answers "install" for hooks or
//! MCP servers, which reach a host only through an explicit install.
//!
//! A profile travels as Keleustes's agent JSON (`vault::agent_to_library`'s
//! shape). A host's own backup keeps its full markdown under
//! `meta.belaiMarkdown`, so a profile backed up from one host installs on
//! another exactly as it was; the host validates either form as a profile.

use crate::Core;
use crate::gateway::{DispatchSpec, LibraryRef};
use crate::sessions::Refusal;
use anyhow::Result;
use kls_store::audit::LedgerDraft;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// A fetch or backup must come this soon after its dispatch.
const FETCH_WINDOW_MS: i64 = 60 * 60 * 1000;
/// Largest document accepted from a host.
const MAX_DOC: usize = 1024 * 1024;

/// Kinds the host never gets from Reconcile (an explicit install only).
pub const INSTALL_ONLY: [&str; 2] = ["hook", "mcp"];

/// Setting that turns the automatic sync's dispatches off (`off`); on when
/// unset. Reconcile verdicts are answered either way.
pub const AUTO_SYNC: &str = "library.auto_sync";

/// The automatic sync queues the same set for a node at most this often.
pub const SYNC_EVERY_MS: i64 = 30 * 60 * 1000;

fn denied(why: &str) -> anyhow::Error {
    anyhow::Error::new(Refusal::NotOwner).context(why.to_string())
}

fn invalid(why: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Refusal::Invalid(why.into()))
}

/// The library kind a reference maps to ("profile" is Keleustes's "agent").
fn library_kind(kind: &str) -> &str {
    if kind == "profile" {
        crate::vault::AGENT_KIND
    } else {
        kind
    }
}

pub fn sha256_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// Advert entries that are a host's catalogue, not library items: a
/// knowledge index and an installed plugin. Reconcile lists them, never
/// offers to push or install them.
pub const CATALOGUE_KINDS: [&str; 2] = ["knowledge", "plugin"];

/// One file of a hook bundle as it goes to a host.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BundleFile {
    pub path: String,
    pub data: Vec<u8>,
    pub sha256: String,
}

/// The hash of a bundle manifest: each `path NUL sha256 LF`, in path order.
pub fn manifest_hash(entries: &[kls_store::libsearch::BundleEntry]) -> String {
    let mut h = Sha256::new();
    for e in entries {
        h.update(e.path.as_bytes());
        h.update([0]);
        h.update(e.sha256.as_bytes());
        h.update(b"\n");
    }
    hex::encode(h.finalize())
}

/// The settings key a teleport's needs (a JSON `Vec<LibraryRef>`) are kept
/// under; the teleport table has no column for them.
pub fn teleport_needs_key(teleport_id: &str) -> String {
    format!("teleport.needs.{teleport_id}")
}

fn confirm_key(node: &str, item: &str, version: i64) -> String {
    format!("library.files_confirmed:{node}:{item}:{version}")
}

impl Core {
    /// The dispatch a fetch or backup serves, if it is addressed to `node`,
    /// of `kind`, recent, and names `r`.
    async fn library_dispatch(
        &self,
        node: &str,
        dispatch_id: &str,
        kind: &str,
        r: &LibraryRef,
    ) -> Result<DispatchSpec> {
        let Some(d) = self.store().dispatch(dispatch_id).await? else {
            return Err(denied("unknown dispatch"));
        };
        if d.node_id != node || d.kind != kind {
            return Err(denied("the dispatch is not this node's"));
        }
        if kls_store::now_ms() - d.created_at > FETCH_WINDOW_MS {
            return Err(denied("the dispatch is too old"));
        }
        let spec: DispatchSpec = serde_json::from_str(&d.spec_json)?;
        let named = spec.library.iter().any(|x| {
            x.kind == r.kind
                && ((!x.id.is_empty() && x.id == r.id) || (!x.name.is_empty() && x.name == r.name))
        });
        if !named {
            return Err(denied("the dispatch does not name that item"));
        }
        Ok(spec)
    }

    /// A document for a delivered `library_install` dispatch that names it.
    pub async fn library_fetch(
        &self,
        node: &str,
        dispatch_id: &str,
        r: &LibraryRef,
    ) -> Result<(LibraryRef, Vec<u8>)> {
        let (out, doc, _) = self.library_fetch_files(node, dispatch_id, r).await?;
        Ok((out, doc))
    }

    /// [`Core::library_fetch`] with a hook bundle's files `(path, bytes,
    /// sha256)`. A bundle that carries files is answered only after the
    /// owner's second confirmation for this node and version
    /// ([`Core::confirm_bundle_files`]) of exactly these files; otherwise it
    /// is refused (audited). Refused under the kill switch.
    pub async fn library_fetch_files(
        &self,
        node: &str,
        dispatch_id: &str,
        r: &LibraryRef,
    ) -> Result<(LibraryRef, Vec<u8>, Vec<BundleFile>)> {
        self.library_dispatch(node, dispatch_id, "library_install", r)
            .await?;
        crate::control::check(
            self,
            "library_install",
            &format!("{} {}", r.kind, r.name),
            &format!("host:{node}"),
        )
        .await?;
        self.library_doc(node, "dispatch", dispatch_id, r).await
    }

    /// A profile or crew a teleport needs, for `Library.Fetch(teleport_id)`
    /// (WP-E, requests-governance.md P4): answered only for the node the
    /// teleport is addressed to while it is `ready` or `delivered` and not
    /// expired, and only for a need stored with it (kind and name). Refused
    /// under the kill switch and, like a profile or crew install, under the
    /// A2A blackout. Audited with `subject.teleport`.
    pub async fn library_fetch_teleport(
        &self,
        node: &str,
        teleport_id: &str,
        r: &LibraryRef,
    ) -> Result<(LibraryRef, Vec<u8>, Vec<BundleFile>)> {
        if !matches!(r.kind.as_str(), "profile" | "crew") {
            return Err(denied("a teleport needs profiles and crews only"));
        }
        let t = self
            .teleport_for_download(node, teleport_id)
            .await
            .map_err(|e| {
                denied(&format!(
                    "the teleport is not this node's to fetch for: {e}"
                ))
            })?;
        let needs: Vec<LibraryRef> = self
            .store()
            .setting(&teleport_needs_key(&t.id))
            .await?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let target = format!("{} {}", r.kind, r.name);
        crate::control::check(self, "library_install", &target, &format!("host:{node}")).await?;
        if crate::control::state(self).await?.a2a_blackout {
            // Emits the audited a2a.withheld event.
            let _ =
                crate::control::check_a2a(self, "keleustes", node, &format!("host:{node}")).await;
            return Err(denied("the A2A blackout holds profile and crew installs"));
        }
        // Resolve first, so a ref by id is checked by the name it has here.
        let name = match (!r.id.is_empty()).then_some(&r.id) {
            Some(id) => self
                .store()
                .library_item(id)
                .await?
                .map(|i| i.name)
                .unwrap_or_default(),
            None => r.name.clone(),
        };
        if !needs.iter().any(|n| n.kind == r.kind && n.name == name) {
            return Err(denied("the teleport does not need that item"));
        }
        self.library_doc(node, "teleport", &t.id, r).await
    }

    /// The document (and files) for `r`, after the caller authorized it;
    /// `scope` names the dispatch or teleport it serves in the audit.
    async fn library_doc(
        &self,
        node: &str,
        scope: &str,
        scope_id: &str,
        r: &LibraryRef,
    ) -> Result<(LibraryRef, Vec<u8>, Vec<BundleFile>)> {
        let store = self.store();
        let item = match (!r.id.is_empty()).then_some(&r.id) {
            Some(id) => store.library_item(id).await?,
            None => {
                store
                    .library_item_by_name(library_kind(&r.kind), &r.name)
                    .await?
            }
        }
        .ok_or_else(|| anyhow::Error::new(Refusal::NotFound("library item")))?;
        if item.kind != library_kind(&r.kind) {
            return Err(invalid("the item is of another kind"));
        }
        let versions = store.library_versions(&item.id).await?;
        let v = if r.version > 0 {
            versions.into_iter().find(|v| v.version == r.version)
        } else {
            versions.into_iter().next()
        }
        .ok_or_else(|| anyhow::Error::new(Refusal::NotFound("library version")))?;
        let doc = v.body.into_bytes();
        let out = LibraryRef {
            kind: r.kind.clone(),
            id: item.id.clone(),
            name: item.name.clone(),
            version: v.version,
            sha256: sha256_hex(&doc),
        };
        let mut files = Vec::new();
        let mut manifest = String::new();
        if item.kind == "hook" {
            let entries = store.bundle_files(&item.id, v.version).await?;
            manifest = manifest_hash(&entries);
            if !entries.is_empty() {
                let confirmed = self
                    .bundle_confirmation(node, &item.id, v.version)
                    .await?
                    .is_some_and(|(sha, at)| {
                        sha == manifest && kls_store::now_ms() - at <= FETCH_WINDOW_MS
                    });
                if !confirmed {
                    self.audit_record(LedgerDraft {
                        kind: "library".into(),
                        actor: format!("node:{node}"),
                        actor_kind: "host".into(),
                        msg: format!(
                            "hook {} v{} refused: its files were not confirmed",
                            out.name, out.version
                        ),
                        subject: json!({ "item": out.id, scope: scope_id }),
                        payload: json!({ "files": entries.len(), "manifest": manifest, "outcome": "refused" }),
                        origin: format!("host:{node}"),
                        node_id: node.into(),
                        ..Default::default()
                    })
                    .await?;
                    return Err(invalid(
                        "the hook's files need the owner's second confirmation for this host",
                    ));
                }
            }
            for e in entries {
                let data = store
                    .library_blob(&e.sha256)
                    .await?
                    .ok_or_else(|| anyhow::Error::new(Refusal::NotFound("bundle file")))?;
                files.push(BundleFile {
                    path: e.path,
                    sha256: e.sha256,
                    data,
                });
            }
        } else if item.kind == crate::vault::AGENT_KIND {
            // The files an agent carries (agent B): text, hashed, saved with
            // the version by a publish. They are documents, not scripts, so
            // they need no second confirmation; the host checks each again.
            let entries = store.bundle_files(&item.id, v.version).await?;
            manifest = manifest_hash(&entries);
            for e in entries {
                let data = store
                    .library_blob(&e.sha256)
                    .await?
                    .ok_or_else(|| anyhow::Error::new(Refusal::NotFound("agent file")))?;
                files.push(BundleFile {
                    path: e.path,
                    sha256: e.sha256,
                    data,
                });
            }
        }
        self.audit_record(LedgerDraft {
            kind: "library".into(),
            actor: format!("node:{node}"),
            actor_kind: "host".into(),
            msg: format!(
                "{} {} v{} fetched for install",
                out.kind, out.name, out.version
            ),
            subject: json!({ "item": out.id, scope: scope_id }),
            payload: json!({ "kind": out.kind, "version": out.version, "sha256": out.sha256,
                             "files": files.len(), "manifest": manifest }),
            origin: format!("host:{node}"),
            node_id: node.into(),
            ..Default::default()
        })
        .await?;
        Ok((out, doc, files))
    }

    /// The owner's second confirmation that a hook bundle's files may go to
    /// `node`: recorded for the item's `version` (latest when 0) and the
    /// exact manifest it holds now, valid for an hour. Refused under the
    /// kill switch; audited with its origin. Returns the files confirmed.
    pub async fn confirm_bundle_files(
        &self,
        node: &str,
        item_id: &str,
        version: i64,
        origin: &str,
    ) -> Result<Vec<kls_store::libsearch::BundleEntry>> {
        crate::control::check(self, "library_install", node, origin).await?;
        let store = self.store();
        let item = store
            .library_item(item_id)
            .await?
            .ok_or_else(|| anyhow::Error::new(Refusal::NotFound("library item")))?;
        if item.kind != "hook" {
            return Err(invalid("only a hook bundle carries files"));
        }
        let version = if version > 0 {
            version
        } else {
            item.latest_version
        };
        let entries = store.bundle_files(&item.id, version).await?;
        let manifest = manifest_hash(&entries);
        store
            .set_setting(
                &confirm_key(node, &item.id, version),
                &format!("{manifest}:{}", kls_store::now_ms()),
            )
            .await?;
        self.audit_record(LedgerDraft {
            kind: "library".into(),
            actor: "owner".into(),
            actor_kind: "user".into(),
            msg: format!(
                "hook {} v{version}: {} file(s) confirmed for install",
                item.name,
                entries.len()
            ),
            subject: json!({ "item": item.id, "node": node }),
            payload: json!({ "version": version, "manifest": manifest, "by": origin,
                             "files": entries.iter().map(|e| json!({"path": e.path, "sha256": e.sha256})).collect::<Vec<_>>() }),
            origin: "factory".into(),
            node_id: node.into(),
            ..Default::default()
        })
        .await?;
        Ok(entries)
    }

    async fn bundle_confirmation(
        &self,
        node: &str,
        item_id: &str,
        version: i64,
    ) -> Result<Option<(String, i64)>> {
        let v = self
            .store()
            .setting(&confirm_key(node, item_id, version))
            .await?;
        Ok(v.and_then(|s| {
            let (sha, at) = s.rsplit_once(':')?;
            Some((sha.to_string(), at.parse().ok()?))
        }))
    }

    /// A host's copy for a `library_backup` dispatch that named it: a new
    /// library version (none when nothing changed). Audited.
    pub async fn library_backup(
        &self,
        node: &str,
        dispatch_id: &str,
        r: &LibraryRef,
        doc: &[u8],
    ) -> Result<LibraryRef> {
        // A host copy comes for a backup, or for an import of an item a scan
        // reported (WP-K), whose hash must be the one the scan saw.
        if self
            .library_dispatch(node, dispatch_id, "library_backup", r)
            .await
            .is_err()
        {
            let spec = self
                .library_dispatch(node, dispatch_id, "library_import", r)
                .await?;
            // The host hashes the item as it canonicalises it; Keleustes compares.
            if spec.sha256.is_empty() || spec.sha256 != r.sha256 {
                return Err(invalid("that item changed since the scan; scan again"));
            }
        }
        crate::control::check(
            self,
            "library_backup",
            &format!("{} {}", r.kind, r.name),
            &format!("host:{node}"),
        )
        .await?;
        if doc.is_empty() || doc.len() > MAX_DOC {
            return Err(invalid("the document is empty or too large"));
        }
        let kind = library_kind(&r.kind);
        if crate::belai::library::INSTALL_ONLY.contains(&kind) {
            return Err(invalid(format!(
                "a {kind} is installed from the library, never backed up"
            )));
        }
        let body =
            String::from_utf8(doc.to_vec()).map_err(|_| invalid("the document is not text"))?;
        if kind == crate::vault::AGENT_KIND {
            let v: Value = serde_json::from_str(&body)
                .map_err(|_| invalid("a profile backup is agent JSON"))?;
            if v.get("name")
                .and_then(Value::as_str)
                .is_none_or(|n| n != r.name)
            {
                return Err(invalid("the profile is not the one the dispatch named"));
            }
        }
        if kind == crate::vault::CREW_KIND {
            // Keleustes's crew JSON with the host's canonical form inside.
            let v: Value =
                serde_json::from_str(&body).map_err(|_| invalid("a crew backup is crew JSON"))?;
            if v.get("name")
                .and_then(Value::as_str)
                .is_none_or(|n| n != r.name)
                || !v.get("members").is_some_and(Value::is_array)
            {
                return Err(invalid("the crew is not the one the dispatch named"));
            }
        }
        if CATALOGUE_KINDS.contains(&kind) {
            return Err(invalid(format!("a {kind} is listed, never backed up")));
        }
        let node_name = self
            .store()
            .node(node)
            .await?
            .map(|n| n.name)
            .unwrap_or_default();
        let (id, version) = crate::vault::save_library(
            self,
            "",
            kind,
            &r.name,
            "",
            &body,
            &format!("backed up from node {node_name}"),
        )
        .await?;
        self.audit_record(LedgerDraft {
            kind: "library".into(),
            actor: format!("node:{node}"),
            actor_kind: "host".into(),
            msg: format!("{kind} {} backed up as v{version}", r.name),
            subject: json!({ "item": id, "dispatch": dispatch_id }),
            payload: json!({ "kind": kind, "version": version, "sha256": sha256_hex(doc) }),
            origin: format!("host:{node}"),
            node_id: node.into(),
            ..Default::default()
        })
        .await?;
        if kind == crate::vault::CREW_KIND {
            // Item 6: the crew lands in the Crews table too.
            self.crew_from_backup(node, &body, &id, version).await?;
        }
        Ok(LibraryRef {
            kind: r.kind.clone(),
            id,
            name: r.name.clone(),
            version,
            sha256: sha256_hex(doc),
        })
    }

    /// Puts a backed-up crew in Keleustes's Crews table (`vault::save_crew`,
    /// held to `crew_problems`). Idempotent by the hash of the host's
    /// canonical form (`meta.belaiCrew`, kept in `crews.backup_sha.<id>`): the
    /// same backup again changes nothing, a newer one updates a crew an
    /// earlier backup made, and a crew the owner made by hand is kept. Each
    /// outcome is audited with origin `host:<node>`. The caller has already
    /// passed the kill switch for this backup. Returns the outcome word.
    pub async fn crew_from_backup(
        &self,
        node: &str,
        body: &str,
        item_id: &str,
        version: i64,
    ) -> Result<&'static str> {
        let v: Value =
            serde_json::from_str(body).map_err(|_| invalid("a crew backup is crew JSON"))?;
        let canonical = v
            .pointer("/meta/belaiCrew")
            .and_then(Value::as_str)
            .unwrap_or(body);
        let sha = sha256_hex(canonical.as_bytes());
        let name = v.get("name").and_then(Value::as_str).unwrap_or("").trim();
        let members: Vec<kls_store::vault::CrewMember> = v
            .get("members")
            .cloned()
            .and_then(|m| serde_json::from_value(m).ok())
            .unwrap_or_default();
        let store = self.store();
        let existing = store.crews().await?.into_iter().find(|c| c.name == name);
        let key = |id: &str| format!("crews.backup_sha.{id}");
        let mut crew = kls_store::vault::Crew {
            name: name.to_string(),
            description: v
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            process: v
                .get("process")
                .and_then(Value::as_str)
                .unwrap_or("parallel")
                .to_string(),
            members,
            ..Default::default()
        };
        let (outcome, why) = match &existing {
            Some(c) => match store.setting(&key(&c.id)).await? {
                Some(s) if s == sha => ("current", String::new()),
                Some(_) => {
                    crew.id = c.id.clone();
                    ("updated", String::new())
                }
                None => ("kept", "a crew of that name was made in Keleustes".into()),
            },
            None => ("added", String::new()),
        };
        let (outcome, why, crew_id) = if matches!(outcome, "added" | "updated") {
            match crate::vault::save_crew(self, &crew).await {
                Ok(saved) => {
                    store.set_setting(&key(&saved.id), &sha).await?;
                    (outcome, why, saved.id)
                }
                Err(e) => ("refused", format!("{e:#}"), crew.id.clone()),
            }
        } else {
            (outcome, why, existing.map(|c| c.id).unwrap_or_default())
        };
        self.audit_record(LedgerDraft {
            kind: "library".into(),
            actor: format!("node:{node}"),
            actor_kind: "host".into(),
            msg: format!("crew {name} from the backup: {outcome}"),
            subject: json!({ "crew": crew_id, "item": item_id }),
            payload: json!({
                "outcome": outcome, "reason": why, "sha256": sha, "version": version,
                "into": "crews",
            }),
            origin: format!("host:{node}"),
            node_id: node.into(),
            ..Default::default()
        })
        .await?;
        Ok(outcome)
    }

    /// One verdict per item a host holds: "current" when the library's
    /// latest has the same hash, "push" when the library lacks it,
    /// "install" when the host holds an older library version unchanged
    /// (the library is ahead, so the host may take the latest), "diverged"
    /// otherwise. Never "install" for hooks or MCP servers: they reach a
    /// host only through an explicit install, so a behind one is "diverged".
    pub async fn library_reconcile(
        &self,
        items: &[LibraryRef],
    ) -> Result<Vec<(LibraryRef, String)>> {
        let store = self.store();
        let mut out = Vec::new();
        let same = |body: &str, sha: &str| {
            sha256_hex(body.as_bytes()) == sha || Self::markdown_hash(body).as_deref() == Some(sha)
        };
        for r in items.iter().take(2000) {
            let kind = library_kind(&r.kind);
            if CATALOGUE_KINDS.contains(&kind) {
                out.push((r.clone(), "listed".to_string()));
                continue;
            }
            let verdict = match store.library_item_by_name(kind, &r.name).await? {
                None => "push",
                Some(item) => match store.library_latest(&item.id).await? {
                    Some(v) if same(&v.body, &r.sha256) => "current",
                    Some(_) if INSTALL_ONLY.contains(&kind) || r.sha256.is_empty() => "diverged",
                    Some(_) => {
                        let older = store.library_versions(&item.id).await?;
                        if older.iter().skip(1).any(|v| same(&v.body, &r.sha256)) {
                            "install"
                        } else {
                            "diverged"
                        }
                    }
                    None => "diverged",
                },
            };
            out.push((r.clone(), verdict.to_string()));
        }
        Ok(out)
    }

    /// Whether the automatic sync queues dispatches ([`AUTO_SYNC`]).
    pub async fn library_auto_sync(&self) -> Result<bool> {
        Ok(self.store().setting(AUTO_SYNC).await?.as_deref() != Some("off"))
    }

    /// Turns the automatic sync on or off: stored, audited with who and
    /// where, and announced (`library.auto_sync`). Each dispatch it queues
    /// is still checked against the kill switch when it is queued.
    pub async fn set_library_auto_sync(&self, on: bool, actor: &str, origin: &str) -> Result<()> {
        let word = if on { "on" } else { "off" };
        self.store().set_setting(AUTO_SYNC, word).await?;
        self.audit_record(LedgerDraft {
            kind: "library".into(),
            actor: actor.into(),
            actor_kind: "user".into(),
            msg: format!("library automatic sync {word}"),
            subject: json!({ "setting": AUTO_SYNC }),
            payload: json!({ "value": word }),
            origin: origin.into(),
            ..Default::default()
        })
        .await?;
        self.emit(
            "library.auto_sync",
            AUTO_SYNC,
            &json!({ "value": word, "origin": origin }),
        )
        .await?;
        Ok(())
    }

    /// `Library.Reconcile` from `node` with the automatic sync (#20): the
    /// verdicts of [`Core::library_reconcile`], and for each "push" a
    /// `library_backup` and each "install" a `library_install` (overwrite:
    /// the host holds an older library version unchanged), queued through
    /// [`crate::hosts::library_dispatch_with`], which checks the kill switch
    /// and the A2A blackout and audits every outcome. Hooks and MCP servers
    /// never move this way. Off with the setting [`AUTO_SYNC`] = `off`. The
    /// same set is not queued twice within [`SYNC_EVERY_MS`]: the earlier
    /// dispatch id is answered again. Returns `(ref, verdict, dispatch id)`.
    pub async fn library_reconcile_sync(
        &self,
        node: &str,
        items: &[LibraryRef],
    ) -> Result<Vec<(LibraryRef, String, String)>> {
        let verdicts = self.library_reconcile(items).await?;
        let mut out: Vec<(LibraryRef, String, String)> = verdicts
            .into_iter()
            .map(|(r, v)| (r, v, String::new()))
            .collect();
        if !self.library_auto_sync().await? {
            return Ok(out);
        }
        for (verdict, kind, overwrite) in [
            ("push", "library_backup", false),
            ("install", "library_install", true),
        ] {
            let picked: Vec<usize> = out
                .iter()
                .enumerate()
                .filter(|(_, (r, v, _))| {
                    v == verdict
                        && !INSTALL_ONLY.contains(&library_kind(&r.kind))
                        && !CATALOGUE_KINDS.contains(&r.kind.as_str())
                })
                .map(|(i, _)| i)
                .take(50)
                .collect();
            if picked.is_empty() {
                continue;
            }
            let refs: Vec<LibraryRef> = picked
                .iter()
                .map(|&i| {
                    let r = &out[i].0;
                    // Names only: the host's ids and versions are its own.
                    LibraryRef {
                        kind: r.kind.clone(),
                        name: r.name.clone(),
                        sha256: r.sha256.clone(),
                        ..Default::default()
                    }
                })
                .collect();
            let mut names: Vec<String> = refs
                .iter()
                .map(|r| format!("{}/{}/{}", r.kind, r.name, r.sha256))
                .collect();
            names.sort();
            let digest = sha256_hex(names.join("\n").as_bytes());
            let key = format!("library.sync.{node}.{kind}");
            let now = kls_store::now_ms();
            let last: Option<Value> = self
                .store()
                .setting(&key)
                .await?
                .and_then(|s| serde_json::from_str(&s).ok());
            let recent = last.as_ref().and_then(|l| {
                let same = l["digest"].as_str() == Some(digest.as_str());
                let fresh = now - l["at"].as_i64().unwrap_or(0) < SYNC_EVERY_MS;
                (same && fresh).then(|| l["id"].as_str().unwrap_or_default().to_string())
            });
            let id = match recent {
                Some(id) => id,
                None => {
                    let origin = crate::hosts::Origin {
                        actor: "owner".into(),
                        surface: "library.sync".into(),
                        reason: format!("automatic sync: {verdict}"),
                        ..Default::default()
                    };
                    match crate::hosts::library_dispatch_with(
                        self, node, kind, refs, false, overwrite, origin,
                    )
                    .await
                    {
                        Ok(id) => {
                            let rec = json!({ "at": now, "digest": digest, "id": id });
                            self.store().set_setting(&key, &rec.to_string()).await?;
                            id
                        }
                        // Refused (kill switch, blackout, not offered):
                        // audited there; the host asks the owner instead.
                        Err(e) => {
                            tracing::info!(node, kind, "library sync not queued: {e:#}");
                            continue;
                        }
                    }
                }
            };
            for i in picked {
                out[i].2 = id.clone();
            }
        }
        Ok(out)
    }

    /// The hash of a backed-up profile's markdown (or crew's canonical
    /// JSON), which is what the host's advert carries, for "current".
    fn markdown_hash(body: &str) -> Option<String> {
        let v: Value = serde_json::from_str(body).ok()?;
        let md = v
            .pointer("/meta/belaiMarkdown")
            .or_else(|| v.pointer("/meta/belaiCrew"))?
            .as_str()?;
        Some(sha256_hex(md.as_bytes()))
    }
}

impl Core {
    /// What a `library_scan` dispatch to `node` found (WP-K): names, paths
    /// and hashes, stored so the owner can pick items to import. Audited.
    pub async fn library_scan_report(
        &self,
        node: &str,
        dispatch_id: &str,
        items: Vec<kls_store::belai::ScanItem>,
    ) -> Result<usize> {
        let Some(d) = self.store().dispatch(dispatch_id).await? else {
            return Err(denied("unknown dispatch"));
        };
        if d.node_id != node || d.kind != "library_scan" {
            return Err(denied("the dispatch is not this node's scan"));
        }
        let clean = |s: &str, n: usize| -> String {
            s.chars().filter(|c| !c.is_control()).take(n).collect()
        };
        let items: Vec<_> = items
            .into_iter()
            .take(2000)
            .map(|it| kls_store::belai::ScanItem {
                kind: clean(&it.kind, 32),
                name: clean(&it.name, 128),
                path: clean(&it.path, 1024),
                sha256: clean(&it.sha256, 64),
                harness: clean(&it.harness, 40),
                verdict: clean(&it.verdict, 160),
                ..Default::default()
            })
            .collect();
        self.store().put_scan(dispatch_id, node, &items).await?;
        self.emit(
            "library.scan",
            dispatch_id,
            &json!({ "nodeId": node, "items": items.len() }),
        )
        .await?;
        self.audit_record(LedgerDraft {
            kind: "library".into(),
            actor: format!("node:{node}"),
            actor_kind: "host".into(),
            msg: format!("library scan reported {} items", items.len()),
            subject: json!({ "dispatch": dispatch_id }),
            payload: json!({ "items": items.len() }),
            origin: format!("host:{node}"),
            node_id: node.into(),
            ..Default::default()
        })
        .await?;
        Ok(items.len())
    }

    /// Asks the node that ran `scan_dispatch` to import the item at `idx`
    /// (for a document, into agent `target`). The node re-reads the file and
    /// refuses it when its hash is not the one the scan saw.
    pub async fn library_import(
        &self,
        scan_dispatch: &str,
        idx: i64,
        target: &str,
    ) -> Result<String> {
        let it = self
            .store()
            .scan_items(scan_dispatch)
            .await?
            .into_iter()
            .find(|i| i.idx == idx)
            .ok_or_else(|| anyhow::Error::new(Refusal::NotFound("scan item")))?;
        self.gateway()?
            .dispatch(
                &it.node_id,
                DispatchSpec {
                    kind: "library_import".into(),
                    item_kind: it.kind.clone(),
                    scan_dispatch: scan_dispatch.into(),
                    path: it.path.clone(),
                    sha256: it.sha256.clone(),
                    target: target.into(),
                    library: vec![LibraryRef {
                        kind: it.kind.clone(),
                        name: it.name.clone(),
                        sha256: it.sha256.clone(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )
            .await
    }
}

#[cfg(test)]
mod crew_backup_tests {
    use super::*;

    fn doc(canonical: &str, count: i64) -> String {
        json!({
            "name": "builders", "description": "", "process": "parallel",
            "members": [{"agent": "crew-builder", "role": "", "count": count}],
            "meta": {"belaiCrew": canonical},
        })
        .to_string()
    }

    async fn crew_entries(c: &Core) -> Vec<kls_store::audit::LedgerEntry> {
        c.store()
            .ledger(&kls_store::audit::LedgerQuery {
                kinds: vec!["library".into()],
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.payload["into"] == "crews")
            .collect()
    }

    /// Item 6: a backed-up crew lands in the Crews table once per canonical
    /// form, an update replaces only a crew a backup made, and a crew the
    /// owner made stays. Every outcome is audited with the host as origin.
    #[tokio::test]
    async fn backed_up_crews_land_in_the_crews_table_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        assert_eq!(
            c.crew_from_backup("n1", &doc("{\"v\":1}", 2), "i", 1)
                .await
                .unwrap(),
            "added"
        );
        assert_eq!(
            c.crew_from_backup("n1", &doc("{\"v\":1}", 2), "i", 1)
                .await
                .unwrap(),
            "current"
        );
        let crews = c.store().crews().await.unwrap();
        assert_eq!(crews.len(), 1);
        assert_eq!(crews[0].members[0].count, 2);
        assert_eq!(
            c.crew_from_backup("n1", &doc("{\"v\":2}", 3), "i", 2)
                .await
                .unwrap(),
            "updated"
        );
        let crews = c.store().crews().await.unwrap();
        assert_eq!((crews.len(), crews[0].members[0].count), (1, 3));
        let e = crew_entries(&c).await;
        assert_eq!(e.len(), 3);
        assert!(
            e.iter()
                .all(|e| e.origin == "host:n1" && e.actor == "node:n1")
        );

        // A crew the owner made is kept.
        let dir2 = tempfile::tempdir().unwrap();
        let c2 = Core::open(dir2.path(), None).await.unwrap();
        crate::vault::save_crew(
            &c2,
            &kls_store::vault::Crew {
                name: "builders".into(),
                members: vec![kls_store::vault::CrewMember {
                    agent: "mine".into(),
                    count: 1,
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            c2.crew_from_backup("n1", &doc("{}", 2), "i", 1)
                .await
                .unwrap(),
            "kept"
        );
        assert_eq!(
            c2.store().crews().await.unwrap()[0].members[0].agent,
            "mine"
        );
    }

    /// Under the kill switch the backup (and so the Crews write) is refused.
    #[tokio::test]
    async fn crew_backup_is_refused_under_the_kill_switch() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        crate::control::stop(&c, "test").await.unwrap();
        let r = LibraryRef {
            kind: "crew".into(),
            name: "builders".into(),
            ..Default::default()
        };
        assert!(
            c.library_backup("n1", "d1", &r, doc("{}", 1).as_bytes())
                .await
                .is_err()
        );
        assert!(c.store().crews().await.unwrap().is_empty());
    }
}

//! Library service (WP-E): documents for a delivered library dispatch, a
//! host's backups, and the automatic sync's verdicts. Authorization and
//! audit live in `kls_core::belai::library`.

use crate::server::{Hub, authenticate, internal};
use crate::sessions::status;
use kls_core::gateway::LibraryRef;
use kls_proto::v1::{self as pb, library_server::Library};
use tonic::{Request, Response, Status};

pub struct Svc {
    pub hub: Hub,
}

/// A kill-switch refusal says so (the host reports it); others map as usual.
fn refusal(e: anyhow::Error) -> Status {
    if kls_core::control::is_blocked(&e) {
        return Status::failed_precondition(e.to_string());
    }
    status(e)
}

fn ref_of(r: Option<pb::LibraryRef>) -> Result<LibraryRef, Status> {
    let r = r.ok_or_else(|| Status::invalid_argument("ref is required"))?;
    Ok(LibraryRef {
        kind: r.kind,
        id: r.id,
        name: r.name,
        version: r.version,
        sha256: r.sha256,
    })
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

#[tonic::async_trait]
impl Library for Svc {
    async fn fetch(
        &self,
        req: Request<pb::LibraryFetch>,
    ) -> Result<Response<pb::LibraryDoc>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        let core = self.hub.core();
        let (out, doc, files) = match (r.dispatch_id.is_empty(), r.teleport_id.is_empty()) {
            (false, _) => {
                core.library_fetch_files(&node, &r.dispatch_id, &ref_of(r.r#ref)?)
                    .await
            }
            // A profile or crew a teleport to this node needs (#17).
            (true, false) => {
                core.library_fetch_teleport(&node, &r.teleport_id, &ref_of(r.r#ref)?)
                    .await
            }
            (true, true) => {
                return Err(Status::permission_denied(
                    "a fetch names the dispatch or teleport it serves",
                ));
            }
        }
        .map_err(refusal)?;
        Ok(Response::new(pb::LibraryDoc {
            dispatch_id: r.dispatch_id,
            r#ref: Some(ref_pb(out)),
            doc,
            // A hook bundle's scripts, each with its hash (WP-E #22).
            files: files
                .into_iter()
                .map(|f| pb::LibraryFile {
                    path: f.path,
                    data: f.data,
                    sha256: f.sha256,
                })
                .collect(),
        }))
    }

    async fn backup(
        &self,
        req: Request<pb::LibraryDoc>,
    ) -> Result<Response<pb::LibraryReceipt>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let d = req.into_inner();
        let stored = self
            .hub
            .core()
            .library_backup(&node, &d.dispatch_id, &ref_of(d.r#ref)?, &d.doc)
            .await
            .map_err(refusal)?;
        Ok(Response::new(pb::LibraryReceipt {
            stored: Some(ref_pb(stored)),
        }))
    }

    async fn scan_report(
        &self,
        req: Request<pb::LibraryScanReport>,
    ) -> Result<Response<pb::Ok>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        let at = |v: &Vec<String>, i: usize| v.get(i).cloned().unwrap_or_default();
        let items = r
            .found
            .iter()
            .enumerate()
            .map(|(i, f)| kls_store::belai::ScanItem {
                kind: f.kind.clone(),
                name: f.name.clone(),
                sha256: f.sha256.clone(),
                path: at(&r.paths, i),
                harness: at(&r.harnesses, i),
                verdict: at(&r.verdicts, i),
                ..Default::default()
            })
            .collect();
        self.hub
            .core()
            .library_scan_report(&node, &r.dispatch_id, items)
            .await
            .map_err(status)?;
        Ok(Response::new(pb::Ok {}))
    }

    async fn reconcile(
        &self,
        req: Request<pb::LibraryInventory>,
    ) -> Result<Response<pb::LibraryVerdicts>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let items: Vec<LibraryRef> = req
            .into_inner()
            .items
            .into_iter()
            .map(|r| ref_of(Some(r)))
            .collect::<Result<_, _>>()?;
        // The automatic sync (#20): push and install verdicts queue their
        // backup or install dispatch, named in `dispatch_id`.
        let v = self
            .hub
            .core()
            .library_reconcile_sync(&node, &items)
            .await
            .map_err(internal)?;
        Ok(Response::new(pb::LibraryVerdicts {
            verdicts: v
                .into_iter()
                .map(|(r, verdict, dispatch_id)| pb::LibraryVerdict {
                    r#ref: Some(ref_pb(r)),
                    verdict,
                    dispatch_id,
                })
                .collect(),
        }))
    }
}

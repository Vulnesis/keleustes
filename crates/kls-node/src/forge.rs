//! Forge service (WP-H): a node files a worker's forge request. Only the
//! request (the first part) is taken; a git bundle is neither accepted nor
//! stored, so Keleustes never holds or runs git on another host's data.

use crate::server::{Hub, authenticate};
use crate::sessions::status;
use kls_core::belai::forge::ForgeRequest;
use kls_proto::v1::{self as pb, forge_server::Forge};
use tonic::{Request, Response, Status, Streaming};

pub struct Svc {
    pub hub: Hub,
}

#[tonic::async_trait]
impl Forge for Svc {
    async fn file(
        &self,
        req: Request<Streaming<pb::ForgePart>>,
    ) -> Result<Response<pb::ForgeFiled>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let mut parts = req.into_inner();
        let first = parts
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("an empty forge request"))?;
        if !first.chunk.is_empty() || !first.last {
            return Err(Status::invalid_argument(
                "this Keleustes takes the request alone, without a bundle",
            ));
        }
        let r = first
            .request
            .ok_or_else(|| Status::invalid_argument("the first part carries the request"))?;
        let id = r.request_id.clone();
        let duplicate = self
            .hub
            .core()
            .forge_file(
                &node,
                ForgeRequest {
                    request_id: r.request_id,
                    item_id: r.item_id,
                    worker_id: r.worker_id,
                    kind: r.kind,
                    repo_owner: r.repo_owner,
                    repo_name: r.repo_name,
                    branch: r.branch,
                    base_sha: r.base_sha,
                    head_sha: r.head_sha,
                    bundle_sha256: r.bundle_sha256,
                    bundle_bytes: r.bundle_bytes,
                    failure: r.failure,
                    title: r.title,
                    ..Default::default()
                },
            )
            .await
            .map_err(status)?;
        Ok(Response::new(pb::ForgeFiled {
            request_id: id,
            duplicate,
        }))
    }
}

//! Jobs service: results of jobs and dispatches delivered on Link.
//! Owner: M1 sessions workstream (dispatch outcomes); jobs reused by M4/M5.

use crate::server::{Hub, authenticate};
use crate::sessions::status;
use kls_core::sessions::Completion;
use kls_proto::v1::{self as pb, jobs_server::Jobs};
use tonic::{Request, Response, Status};

pub struct Svc {
    pub hub: Hub,
}

#[tonic::async_trait]
impl Jobs for Svc {
    /// Resolves a job (waking its `NodeGateway::job` caller) or a dispatch
    /// (`job_id` is then the dispatch id and `session_id` the session it
    /// started). Only the node the work went to may report it.
    async fn complete(&self, req: Request<pb::JobResult>) -> Result<Response<pb::Ok>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        if r.job_id.is_empty() {
            return Err(Status::invalid_argument("job_id is required"));
        }
        self.hub
            .core()
            .complete(
                &node,
                &Completion {
                    job_id: r.job_id,
                    status: r.status,
                    output_json: r.output_json,
                    error: r.error,
                    session_id: r.session_id,
                },
            )
            .await
            .map_err(status)?;
        Ok(Response::new(pb::Ok {}))
    }
}

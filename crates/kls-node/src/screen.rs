//! Screen service (harnesses.md): the live terminal of a harness session.
//! The node streams its (already scrubbed) output up; Keleustes keeps the
//! last open frame and a bounded replay per session and fans output out to
//! UI subscribers. Keys never travel here: they go through the session's
//! inbox as `Command{name:"key"}`. With no viewer the node is told to pause.

use crate::server::{Hub, authenticate};
use crate::sessions::status;
use kls_proto::v1::{self as pb, screen_down, screen_server::Screen, screen_up};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

/// Replay kept per session.
const MAX_REPLAY: usize = 256 * 1024;
/// Largest output chunk accepted.
const MAX_CHUNK: usize = 32 * 1024;

struct State {
    node_id: String,
    replay: Mutex<(Vec<u8>, u64, Option<pb::ScreenSize>)>,
    tx: broadcast::Sender<Vec<u8>>,
    down: mpsc::Sender<Result<pb::ScreenDown, Status>>,
}

static SCREENS: LazyLock<Mutex<HashMap<String, Arc<State>>>> = LazyLock::new(Mutex::default);

/// What a viewer gets: the replay, the offset after it, its size, and the
/// live output.
pub struct Attached {
    pub replay: Vec<u8>,
    pub offset: u64,
    pub size: Option<(u32, u32)>,
    pub rx: broadcast::Receiver<Vec<u8>>,
}

impl Hub {
    /// Attaches a viewer to a harness session's screen (audited as the
    /// owner's), or `None` when the node streams none.
    pub fn screen(&self, session_id: &str) -> Option<Attached> {
        let st = SCREENS.lock().unwrap().get(session_id).cloned()?;
        let rx = st.tx.subscribe();
        let (replay, offset, size) = st.replay.lock().unwrap().clone();
        let _ = st.down.try_send(Ok(pb::ScreenDown {
            body: Some(screen_down::Body::Pause(false)),
        }));
        let core = self.core().clone();
        let sid = session_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = core.screen_attached(&sid, "owner").await {
                tracing::warn!("screen attached: {e:#}");
            }
        });
        Some(Attached {
            replay,
            offset,
            size: size.map(|s| (s.cols, s.rows)),
            rx,
        })
    }

    /// Asks the node to resize a session's terminal.
    pub fn screen_resize(&self, session_id: &str, cols: u32, rows: u32) -> bool {
        let Some(st) = SCREENS.lock().unwrap().get(session_id).cloned() else {
            return false;
        };
        st.down
            .try_send(Ok(pb::ScreenDown {
                body: Some(screen_down::Body::Resize(pb::ScreenSize {
                    cols: cols.clamp(20, 400),
                    rows: rows.clamp(5, 200),
                })),
            }))
            .is_ok()
    }

    /// Whether a session's screen is being streamed now.
    pub fn screen_live(&self, session_id: &str) -> bool {
        SCREENS.lock().unwrap().contains_key(session_id)
    }
}

pub struct Svc {
    pub hub: Hub,
}

type DownStream = Pin<Box<dyn Stream<Item = Result<pb::ScreenDown, Status>> + Send>>;

#[tonic::async_trait]
impl Screen for Svc {
    type StreamStream = DownStream;

    async fn stream(
        &self,
        req: Request<Streaming<pb::ScreenUp>>,
    ) -> Result<Response<Self::StreamStream>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let mut up = req.into_inner();
        let first = up
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("empty screen stream"))?;
        let sid = first.session_id.clone();
        self.hub
            .core()
            .owned_session(&node, &sid)
            .await
            .map_err(status)?;
        let (down, rx) = mpsc::channel(16);
        let (tx, _) = broadcast::channel(256);
        let st = Arc::new(State {
            node_id: node.clone(),
            replay: Mutex::new((Vec::new(), first.offset, None)),
            tx,
            down: down.clone(),
        });
        if let Some(old) = SCREENS.lock().unwrap().insert(sid.clone(), st.clone())
            && old.node_id != node
        {
            tracing::warn!(%sid, "screen taken over by another node");
        }
        // Nobody watches yet.
        let _ = down.try_send(Ok(pb::ScreenDown {
            body: Some(screen_down::Body::Pause(true)),
        }));
        tokio::spawn(async move {
            let mut next = Some(first);
            loop {
                let m = match next.take() {
                    Some(m) => m,
                    None => match up.message().await {
                        Ok(Some(m)) => m,
                        _ => break,
                    },
                };
                if m.session_id != sid && !m.session_id.is_empty() {
                    break;
                }
                match m.body {
                    Some(screen_up::Body::Open(o)) => {
                        let mut r = st.replay.lock().unwrap();
                        let keep = o.replay.len().saturating_sub(MAX_REPLAY);
                        r.0 = o.replay[keep..].to_vec();
                        r.1 = m.offset;
                        r.2 = o.size;
                    }
                    Some(screen_up::Body::Output(b)) if b.len() <= MAX_CHUNK => {
                        {
                            let mut r = st.replay.lock().unwrap();
                            r.0.extend_from_slice(&b);
                            let over = r.0.len().saturating_sub(MAX_REPLAY);
                            r.0.drain(..over);
                            r.1 = m.offset.max(r.1) + b.len() as u64;
                        }
                        if st.tx.send(b).is_err() {
                            let _ = st.down.try_send(Ok(pb::ScreenDown {
                                body: Some(screen_down::Body::Pause(true)),
                            }));
                        }
                    }
                    Some(screen_up::Body::Size(s)) => st.replay.lock().unwrap().2 = Some(s),
                    Some(screen_up::Body::Exit(_)) => break,
                    _ => {}
                }
            }
            let mut all = SCREENS.lock().unwrap();
            if all.get(&sid).is_some_and(|s| Arc::ptr_eq(s, &st)) {
                all.remove(&sid);
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

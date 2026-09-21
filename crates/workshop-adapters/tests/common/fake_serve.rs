//! An in-process stand-in for `opencode serve` that replays the SSE events
//! captured from the real server (see `tests/fixtures/opencode_serve_*.jsonl`).
//!
//! Endpoints mirror the subset the engine uses: health, providers, session
//! create/get/messages, prompt_async, abort, permissions, and the `/event`
//! stream. Every request is recorded for assertions.

#![allow(dead_code)]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::Frame;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};
use tokio_stream::wrappers::ReceiverStream;

pub const SESSION_ID: &str = "ses_fake0001";
pub const PROVIDERS: &str = include_str!("../fixtures/opencode_serve_providers.json");
pub const TURN: &str = include_str!("../fixtures/opencode_serve_turn.jsonl");
pub const ABORT: &str = include_str!("../fixtures/opencode_serve_abort.jsonl");

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub body: Option<Value>,
}

#[derive(Default)]
pub struct State {
    pub requests: Vec<Recorded>,
    subscribers: Vec<mpsc::Sender<Value>>,
    pub permission_replies: Vec<(String, String)>,
    /// Which fixture the next prompt replays: `turn` or `abort`.
    pub next_script: Script,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Script {
    #[default]
    Turn,
    /// Replays up to the first text delta, waits for `/abort`, then finishes.
    AbortAfterFirstDelta,
    /// Emits a permission ask before the turn events.
    PermissionThenTurn,
}

pub struct FakeServe {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<State>>,
    abort: Arc<Notify>,
    shutdown: Arc<Notify>,
}

impl FakeServe {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State::default()));
        let abort = Arc::new(Notify::new());
        let shutdown = Arc::new(Notify::new());
        let (st, ab, sd) = (state.clone(), abort.clone(), shutdown.clone());
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    a = listener.accept() => a,
                    _ = sd.notified() => break,
                };
                let Ok((stream, _)) = accepted else { break };
                let (st, ab) = (st.clone(), ab.clone());
                tokio::spawn(async move {
                    let svc = service_fn(move |req| handle(req, st.clone(), ab.clone()));
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), svc)
                        .await;
                });
            }
        });
        Self {
            addr,
            state,
            abort,
            shutdown,
        }
    }

    pub fn set_script(&self, script: Script) {
        self.state.lock().unwrap().next_script = script;
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.state.lock().unwrap().requests.clone()
    }

    pub fn stop(&self) {
        self.shutdown.notify_waiters();
    }
}

fn full(body: impl Into<Bytes>) -> BoxBody<Bytes, Infallible> {
    Full::new(body.into()).boxed()
}

fn json_response(status: StatusCode, v: Value) -> Response<BoxBody<Bytes, Infallible>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(full(v.to_string()))
        .unwrap()
}

fn fixture_events(raw: &str) -> Vec<Value> {
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("fixture line is JSON"))
        .collect()
}

async fn broadcast(state: &Arc<Mutex<State>>, event: Value) {
    let subs = state.lock().unwrap().subscribers.clone();
    for tx in subs {
        let _ = tx.send(event.clone()).await;
    }
}

async fn replay(state: Arc<Mutex<State>>, abort: Arc<Notify>, script: Script) {
    // Small delay so the client is between `prompt_async` and its first read,
    // like the real server.
    tokio::time::sleep(Duration::from_millis(30)).await;
    match script {
        Script::Turn => {
            for ev in fixture_events(TURN) {
                broadcast(&state, ev).await;
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }
        Script::PermissionThenTurn => {
            broadcast(
                &state,
                json!({"type": "session.status", "properties": {"sessionID": SESSION_ID, "status": {"type": "busy"}}}),
            )
            .await;
            broadcast(
                &state,
                json!({"type": "permission.updated", "properties": {
                    "id": "perm_1", "type": "bash", "pattern": ["rm -rf *"], "sessionID": SESSION_ID,
                    "messageID": "msg_x", "callID": "call_x", "title": "rm -rf build", "metadata": {}, "time": {"created": 1}
                }}),
            )
            .await;
            // Wait for the reply before continuing, like the real server.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while state.lock().unwrap().permission_replies.is_empty()
                && tokio::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            for ev in fixture_events(TURN) {
                broadcast(&state, ev).await;
            }
        }
        Script::AbortAfterFirstDelta => {
            let events = fixture_events(ABORT);
            let split = events
                .iter()
                .position(|e| e["type"] == "message.part.delta")
                .map(|i| i + 1)
                .unwrap_or(events.len());
            for ev in &events[..split] {
                broadcast(&state, ev.clone()).await;
            }
            let _ = tokio::time::timeout(Duration::from_secs(10), abort.notified()).await;
            for ev in &events[split..] {
                broadcast(&state, ev.clone()).await;
            }
        }
    }
}

async fn handle(
    req: Request<hyper::body::Incoming>,
    state: Arc<Mutex<State>>,
    abort: Arc<Notify>,
) -> Result<Response<BoxBody<Bytes, Infallible>>, Infallible> {
    let method = req.method().to_string();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|p| p.to_string())
        .unwrap_or_default();
    let path = req.uri().path().to_string();
    let authorization = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body_bytes = req
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let body: Option<Value> = serde_json::from_slice(&body_bytes).ok();
    state.lock().unwrap().requests.push(Recorded {
        method: method.clone(),
        path: path_and_query.clone(),
        authorization: authorization.clone(),
        body: body.clone(),
    });

    if authorization.is_none() {
        return Ok(json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": "Unauthorized"}),
        ));
    }
    // Every request must name the project directory, as the real server expects.
    if !path.starts_with("/global/") && !path_and_query.contains("directory=") {
        return Ok(json_response(
            StatusCode::BAD_REQUEST,
            json!({"error": "directory query missing"}),
        ));
    }

    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let resp = match (method.as_str(), segments.as_slice()) {
        ("GET", ["global", "health"]) => json_response(
            StatusCode::OK,
            json!({"healthy": true, "version": "1.18.31"}),
        ),
        ("GET", ["config", "providers"]) => {
            json_response(StatusCode::OK, serde_json::from_str(PROVIDERS).unwrap())
        }
        ("POST", ["session"]) => json_response(
            StatusCode::OK,
            json!({"id": SESSION_ID, "title": body.as_ref().and_then(|b| b["title"].as_str()).unwrap_or("untitled"), "directory": "/work"}),
        ),
        ("GET", ["session", id]) if *id == SESSION_ID => json_response(
            StatusCode::OK,
            json!({"id": SESSION_ID, "title": "fake", "directory": "/work"}),
        ),
        ("GET", ["session", _]) => {
            json_response(StatusCode::NOT_FOUND, json!({"error": "Session not found"}))
        }
        ("GET", ["session", id, "message"]) if *id == SESSION_ID => {
            json_response(StatusCode::OK, json!([]))
        }
        ("POST", ["session", id, "prompt_async"]) if *id == SESSION_ID => {
            let script = state.lock().unwrap().next_script;
            tokio::spawn(replay(state.clone(), abort.clone(), script));
            Response::builder()
                .status(StatusCode::NO_CONTENT)
                .body(full(""))
                .unwrap()
        }
        ("POST", ["session", id, "abort"]) if *id == SESSION_ID => {
            abort.notify_waiters();
            json_response(StatusCode::OK, json!(true))
        }
        ("POST", ["session", id, "permissions", pid]) if *id == SESSION_ID => {
            let response = body
                .as_ref()
                .and_then(|b| b["response"].as_str())
                .unwrap_or("")
                .to_string();
            state
                .lock()
                .unwrap()
                .permission_replies
                .push((pid.to_string(), response));
            json_response(StatusCode::OK, json!(true))
        }
        ("GET", ["event"]) => {
            let (tx, rx) = mpsc::channel::<Value>(256);
            state.lock().unwrap().subscribers.push(tx.clone());
            tokio::spawn(async move {
                let _ = tx
                    .send(json!({"type": "server.connected", "properties": {}}))
                    .await;
            });
            let frames = ReceiverStream::new(rx).map(|v| {
                let chunk = format!("data: {v}\n\n");
                Ok::<_, Infallible>(Frame::data(Bytes::from(chunk)))
            });
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .body(StreamBody::new(frames).boxed())
                .unwrap()
        }
        _ => json_response(
            StatusCode::NOT_FOUND,
            json!({"error": format!("no route {method} {path}")}),
        ),
    };
    Ok(resp)
}

use tokio_stream::StreamExt as _;

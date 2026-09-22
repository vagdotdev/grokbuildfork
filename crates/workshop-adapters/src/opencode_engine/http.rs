//! Minimal loopback HTTP/1.1 + SSE client for `opencode serve`.
//!
//! One TCP connection per request, `127.0.0.1` only, Basic auth with the
//! password Workshop generated for this server. No TLS by design: the engine
//! never talks to anything but its own child process.

use std::net::SocketAddr;

use base64::Engine as _;
use bytes::Bytes;
use http::{Method, Request, StatusCode, header};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("connect {addr}: {source}")]
    Connect {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("http: {0}")]
    Hyper(#[from] hyper::Error),
    #[error("request build: {0}")]
    Build(#[from] http::Error),
    #[error("{method} {path} -> {status}: {body}")]
    Status {
        method: Method,
        path: String,
        status: StatusCode,
        body: String,
    },
    #[error("{path}: response is not JSON: {0}", path = .1)]
    Json(serde_json::Error, String),
}

/// Address + credential for one running `opencode serve`.
#[derive(Clone, Debug)]
pub struct ServerClient {
    addr: SocketAddr,
    basic_auth: String,
}

impl ServerClient {
    pub fn new(addr: SocketAddr, username: &str, password: &str) -> Self {
        let token =
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
        Self {
            addr,
            basic_auth: format!("Basic {token}"),
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    async fn connect(
        &self,
    ) -> Result<hyper::client::conn::http1::SendRequest<Full<Bytes>>, HttpError> {
        let stream = tokio::net::TcpStream::connect(self.addr)
            .await
            .map_err(|source| HttpError::Connect {
                addr: self.addr,
                source,
            })?;
        let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!(error = %e, "opencode serve connection closed with error");
            }
        });
        Ok(sender)
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Request<Full<Bytes>>, HttpError> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, self.addr.to_string())
            .header(header::AUTHORIZATION, &self.basic_auth)
            .header(header::ACCEPT, "application/json, text/event-stream");
        let bytes = match body {
            Some(v) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Bytes::from(serde_json::to_vec(v).expect("serializable json"))
            }
            None => Bytes::new(),
        };
        Ok(builder.body(Full::new(bytes))?)
    }

    /// Send a request and collect the whole body. Non-2xx is an error.
    pub async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Bytes, HttpError> {
        let mut sender = self.connect().await?;
        let req = self.request(method.clone(), path, body)?;
        let resp = sender.send_request(req).await?;
        let status = resp.status();
        let bytes = resp.into_body().collect().await?.to_bytes();
        if !status.is_success() {
            return Err(HttpError::Status {
                method,
                path: path.to_string(),
                status,
                body: String::from_utf8_lossy(&bytes).chars().take(400).collect(),
            });
        }
        Ok(bytes)
    }

    pub async fn get_json(&self, path: &str) -> Result<Value, HttpError> {
        let bytes = self.call(Method::GET, path, None).await?;
        serde_json::from_slice(&bytes).map_err(|e| HttpError::Json(e, path.to_string()))
    }

    /// POST and parse JSON; a `204`/empty body yields `Value::Null`.
    pub async fn post_json(&self, path: &str, body: &Value) -> Result<Value, HttpError> {
        let bytes = self.call(Method::POST, path, Some(body)).await?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|e| HttpError::Json(e, path.to_string()))
    }

    /// Open a `text/event-stream` and forward each `data:` payload, parsed as
    /// JSON, until the server closes it or the receiver is dropped.
    pub async fn subscribe_sse(
        &self,
        path: &str,
        tx: mpsc::Sender<Value>,
    ) -> Result<(), HttpError> {
        let mut sender = self.connect().await?;
        let req = self.request(Method::GET, path, None)?;
        let resp = sender.send_request(req).await?;
        let status = resp.status();
        if !status.is_success() {
            let bytes = resp.into_body().collect().await?.to_bytes();
            return Err(HttpError::Status {
                method: Method::GET,
                path: path.to_string(),
                status,
                body: String::from_utf8_lossy(&bytes).chars().take(400).collect(),
            });
        }
        let mut body = resp.into_body();
        let mut buffer: Vec<u8> = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame?;
            let Some(data) = frame.data_ref() else {
                continue;
            };
            buffer.extend_from_slice(data);
            for event in drain_sse_events(&mut buffer) {
                if tx.send(event).await.is_err() {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

/// Split complete SSE events (terminated by a blank line) off the front of
/// `buffer`, returning the parsed JSON of each `data:` payload.
pub fn drain_sse_events(buffer: &mut Vec<u8>) -> Vec<Value> {
    let mut out = Vec::new();
    while let Some((block_len, sep_len)) = find_event_end(buffer) {
        let block = String::from_utf8_lossy(&buffer[..block_len]).into_owned();
        buffer.drain(..block_len + sep_len);
        let data: Vec<&str> = block
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|l| l.strip_prefix(' ').unwrap_or(l))
            .collect();
        if data.is_empty() {
            continue;
        }
        let payload = data.join("\n");
        match serde_json::from_str::<Value>(&payload) {
            Ok(v) => out.push(v),
            Err(e) => tracing::debug!(error = %e, "ignoring non-JSON SSE payload"),
        }
    }
    out
}

fn find_event_end(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2));
    let crlf = buffer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sse_events_across_chunks() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"data: {\"type\":\"a\"}\n\nevent: x\ndata: {\"ty");
        let first = drain_sse_events(&mut buf);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0]["type"], "a");
        buf.extend_from_slice(b"pe\":\"b\"}\n\n: comment\n\ndata: {\"type\":\"c\"}\r\n\r\n");
        let rest = drain_sse_events(&mut buf);
        assert_eq!(
            rest.iter()
                .map(|v| v["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["b", "c"]
        );
        assert!(buf.is_empty());
    }
}

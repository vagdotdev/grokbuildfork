//! OpenRouter OAuth PKCE (documented at `openrouter.ai/docs/use-cases/oauth-pkce`).
//!
//! 1. Generate a PKCE verifier and S256 challenge.
//! 2. Open `https://openrouter.ai/auth?callback_url=<url>&code_challenge=<c>&code_challenge_method=S256`
//!    — with a loopback `callback_url` on any port, or **headless** (no `callback_url`): OpenRouter
//!    shows the code and the user pastes it.
//! 3. `POST https://openrouter.ai/api/v1/auth/keys {code, code_verifier, code_challenge_method}`
//!    → `{"key": "sk-or-…"}`; the code is single-use and expires after ten minutes.
//!
//! The minted key is the user's; Workshop stores it in its own broker. A per-attempt `state`
//! token is embedded in the callback path so a stray request to the loopback port cannot complete
//! someone else's sign-in.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub const AUTHORIZE_URL: &str = "https://openrouter.ai/auth";
pub const EXCHANGE_URL: &str = "https://openrouter.ai/api/v1/auth/keys";
pub const KEY_LABEL: &str = "Workshop";

#[derive(Debug, thiserror::Error)]
pub enum PkceError {
    #[error("could not bind a loopback callback port: {0}")]
    Bind(#[source] std::io::Error),
    #[error("no callback arrived within {0:?}")]
    Timeout(Duration),
    #[error("callback request was not for this sign-in attempt (state mismatch)")]
    StateMismatch,
    #[error("callback carried no code")]
    MissingCode,
    #[error("malformed callback request")]
    Malformed,
    #[error("exchange failed: HTTP {status}: {body}")]
    Exchange { status: u16, body: String },
    #[error("exchange response carried no key")]
    NoKey,
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// RFC 7636 verifier + S256 challenge.
#[derive(Clone)]
pub struct PkcePair {
    verifier: String,
    challenge: String,
}

impl std::fmt::Debug for PkcePair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PkcePair")
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

fn random_urlsafe(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

impl PkcePair {
    /// 64 random bytes → 86-char verifier (RFC 7636 allows 43–128).
    pub fn generate() -> Self {
        Self::from_verifier(random_urlsafe(64))
    }

    pub fn from_verifier(verifier: impl Into<String>) -> Self {
        let verifier = verifier.into();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }

    pub fn challenge(&self) -> &str {
        &self.challenge
    }

    /// The secret half; only ever sent to the exchange endpoint.
    pub fn verifier(&self) -> &str {
        &self.verifier
    }
}

/// Build the authorize URL. `callback_url = None` selects OpenRouter's headless code display.
pub fn authorize_url(challenge: &str, callback_url: Option<&str>, key_label: &str) -> String {
    let mut url = url::Url::parse(AUTHORIZE_URL).expect("constant URL");
    {
        let mut q = url.query_pairs_mut();
        if let Some(cb) = callback_url {
            q.append_pair("callback_url", cb);
        }
        q.append_pair("code_challenge", challenge);
        q.append_pair("code_challenge_method", "S256");
        if !key_label.is_empty() {
            q.append_pair("key_label", key_label);
        }
    }
    url.to_string()
}

/// Parse the first line of the loopback callback request, e.g.
/// `GET /callback/<state>?code=abc HTTP/1.1`, verifying the embedded state.
pub fn parse_callback_request(
    request_line: &str,
    expected_state: &str,
) -> Result<String, PkceError> {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or(PkceError::Malformed)?;
    let target = parts.next().ok_or(PkceError::Malformed)?;
    if method != "GET" {
        return Err(PkceError::Malformed);
    }
    let url =
        url::Url::parse(&format!("http://127.0.0.1{target}")).map_err(|_| PkceError::Malformed)?;
    let expected_path = format!("/callback/{expected_state}");
    if url.path() != expected_path {
        return Err(PkceError::StateMismatch);
    }
    url.query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
        .filter(|c| !c.is_empty())
        .ok_or(PkceError::MissingCode)
}

/// Headless mode: the user pastes either the bare code or the whole URL OpenRouter showed.
pub fn parse_pasted_code(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(url) = url::Url::parse(s)
        && let Some((_, code)) = url.query_pairs().find(|(k, _)| k == "code")
        && !code.is_empty()
    {
        return Some(code.into_owned());
    }
    if let Some(rest) = s.strip_prefix("code=") {
        return Some(rest.to_string());
    }
    // A bare code has no whitespace or URL characters.
    (!s.contains(char::is_whitespace) && !s.contains(['/', '?', '&'])).then(|| s.to_string())
}

/// One-shot loopback HTTP listener for the PKCE callback.
pub struct CallbackServer {
    listener: TcpListener,
    addr: SocketAddr,
    state: String,
}

impl CallbackServer {
    /// Bind `127.0.0.1` on any free port.
    pub async fn bind() -> Result<Self, PkceError> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(PkceError::Bind)?;
        let addr = listener.local_addr().map_err(PkceError::Bind)?;
        Ok(Self {
            listener,
            addr,
            state: random_urlsafe(24),
        })
    }

    pub fn callback_url(&self) -> String {
        format!(
            "http://127.0.0.1:{}/callback/{}",
            self.addr.port(),
            self.state
        )
    }

    pub fn state(&self) -> &str {
        &self.state
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Accept requests until one carries this attempt's state and a code, or `timeout` elapses.
    /// Requests with the wrong state get a 404 and are ignored (fail closed, keep waiting).
    pub async fn wait_for_code(&self, timeout: Duration) -> Result<String, PkceError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(PkceError::Timeout(timeout));
            }
            let (mut stream, _) =
                match tokio::time::timeout(remaining, self.listener.accept()).await {
                    Ok(Ok(conn)) => conn,
                    Ok(Err(e)) => return Err(PkceError::Io(e)),
                    Err(_) => return Err(PkceError::Timeout(timeout)),
                };
            let mut buf = vec![0u8; 8192];
            let n = match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await
            {
                Ok(Ok(n)) => n,
                _ => continue,
            };
            let request = String::from_utf8_lossy(&buf[..n]);
            let first_line = request.lines().next().unwrap_or("");
            match parse_callback_request(first_line, &self.state) {
                Ok(code) => {
                    let body = "<!doctype html><title>Workshop</title><p>Signed in. You can return to Workshop.</p>";
                    let _ = stream
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        )
                        .await;
                    let _ = stream.shutdown().await;
                    return Ok(code);
                }
                Err(e) => {
                    let status = match e {
                        PkceError::MissingCode => "400 Bad Request",
                        _ => "404 Not Found",
                    };
                    let _ = stream
                        .write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes())
                        .await;
                    let _ = stream.shutdown().await;
                    if matches!(e, PkceError::MissingCode) {
                        return Err(e);
                    }
                }
            }
        }
    }
}

/// Exchange the code for a key at `exchange_url` (the constant, or a mock in tests).
pub async fn exchange_code(
    client: &reqwest::Client,
    exchange_url: &str,
    code: &str,
    verifier: &str,
) -> Result<String, PkceError> {
    let resp = client
        .post(exchange_url)
        .json(&serde_json::json!({
            "code": code,
            "code_verifier": verifier,
            "code_challenge_method": "S256",
        }))
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(PkceError::Exchange {
            status: status.as_u16(),
            body: body.chars().take(300).collect(),
        });
    }
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|_| PkceError::NoKey)?;
    v.get("key")
        .and_then(serde_json::Value::as_str)
        .filter(|k| !k.is_empty())
        .map(str::to_string)
        .ok_or(PkceError::NoKey)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInMode {
    /// Browser redirects to a loopback port Workshop listens on.
    Loopback,
    /// No callback: OpenRouter shows the code; the user pastes it (SSH, containers, no browser).
    Headless,
}

/// A sign-in attempt: hands the caller the URL to open, then completes with the code.
pub struct OpenRouterSignIn {
    pair: PkcePair,
    server: Option<CallbackServer>,
    exchange_url: String,
}

impl OpenRouterSignIn {
    pub async fn start(mode: SignInMode) -> Result<Self, PkceError> {
        let server = match mode {
            SignInMode::Loopback => Some(CallbackServer::bind().await?),
            SignInMode::Headless => None,
        };
        Ok(Self {
            pair: PkcePair::generate(),
            server,
            exchange_url: EXCHANGE_URL.to_string(),
        })
    }

    /// Point the exchange at a mock (tests).
    pub fn with_exchange_url(mut self, url: impl Into<String>) -> Self {
        self.exchange_url = url.into();
        self
    }

    pub fn mode(&self) -> SignInMode {
        if self.server.is_some() {
            SignInMode::Loopback
        } else {
            SignInMode::Headless
        }
    }

    /// The URL to open in the user's browser.
    pub fn authorize_url(&self) -> String {
        let cb = self.server.as_ref().map(CallbackServer::callback_url);
        authorize_url(self.pair.challenge(), cb.as_deref(), KEY_LABEL)
    }

    /// Loopback: wait for the browser redirect, then exchange.
    pub async fn complete_loopback(
        &self,
        client: &reqwest::Client,
        timeout: Duration,
    ) -> Result<String, PkceError> {
        let server = self.server.as_ref().ok_or(PkceError::Malformed)?;
        let code = server.wait_for_code(timeout).await?;
        exchange_code(client, &self.exchange_url, &code, self.pair.verifier()).await
    }

    /// Headless: exchange a pasted code (or pasted URL).
    pub async fn complete_headless(
        &self,
        client: &reqwest::Client,
        pasted: &str,
    ) -> Result<String, PkceError> {
        let code = parse_pasted_code(pasted).ok_or(PkceError::MissingCode)?;
        exchange_code(client, &self.exchange_url, &code, self.pair.verifier()).await
    }
}

/// HTTPS client for the exchange, under the workspace TLS policy.
pub fn exchange_client() -> reqwest::Result<reqwest::Client> {
    xai_grok_extra_ca::build_reqwest_client(|b| {
        b.timeout(Duration::from_secs(20))
            .user_agent("workshop-providers/0.1")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s256_challenge_matches_rfc_7636_appendix_b() {
        let pair = PkcePair::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(
            pair.challenge(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let generated = PkcePair::generate();
        assert!(generated.verifier().len() >= 43 && generated.verifier().len() <= 128);
        assert!(
            generated
                .verifier()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_ne!(PkcePair::generate().verifier(), generated.verifier());
        assert!(
            !format!("{generated:?}").contains(generated.verifier()),
            "debug must not leak the verifier"
        );
    }

    #[test]
    fn authorize_url_shapes() {
        let u = authorize_url(
            "CHAL",
            Some("http://127.0.0.1:4321/callback/st"),
            "Workshop",
        );
        assert!(u.starts_with("https://openrouter.ai/auth?"));
        assert!(u.contains("callback_url=http%3A%2F%2F127.0.0.1%3A4321%2Fcallback%2Fst"));
        assert!(
            u.contains("code_challenge=CHAL")
                && u.contains("code_challenge_method=S256")
                && u.contains("key_label=Workshop")
        );
        let headless = authorize_url("CHAL", None, "Workshop");
        assert!(
            !headless.contains("callback_url"),
            "headless omits the callback so OpenRouter shows the code"
        );
    }

    #[test]
    fn callback_parsing_verifies_state_and_code() {
        assert_eq!(
            parse_callback_request("GET /callback/st123?code=abc HTTP/1.1", "st123").unwrap(),
            "abc"
        );
        assert!(matches!(
            parse_callback_request("GET /callback/other?code=abc HTTP/1.1", "st123"),
            Err(PkceError::StateMismatch)
        ));
        assert!(matches!(
            parse_callback_request("GET /callback/st123 HTTP/1.1", "st123"),
            Err(PkceError::MissingCode)
        ));
        assert!(matches!(
            parse_callback_request("GET /callback/st123?code= HTTP/1.1", "st123"),
            Err(PkceError::MissingCode)
        ));
        assert!(matches!(
            parse_callback_request("POST /callback/st123?code=abc HTTP/1.1", "st123"),
            Err(PkceError::Malformed)
        ));
        assert!(matches!(
            parse_callback_request("GET /favicon.ico HTTP/1.1", "st123"),
            Err(PkceError::StateMismatch)
        ));
        assert!(matches!(
            parse_callback_request("", "st123"),
            Err(PkceError::Malformed)
        ));
    }

    #[test]
    fn pasted_code_accepts_code_or_url() {
        assert_eq!(parse_pasted_code("  abc123  ").as_deref(), Some("abc123"));
        assert_eq!(
            parse_pasted_code("https://openrouter.ai/auth/callback?code=xyz&foo=1").as_deref(),
            Some("xyz")
        );
        assert_eq!(parse_pasted_code("code=q").as_deref(), Some("q"));
        assert_eq!(parse_pasted_code(""), None);
        assert_eq!(parse_pasted_code("two words"), None);
        assert_eq!(
            parse_pasted_code("https://openrouter.ai/auth?nothing=1"),
            None
        );
    }

    #[tokio::test]
    async fn loopback_server_ignores_wrong_state_and_completes_on_the_right_one() {
        let server = CallbackServer::bind().await.unwrap();
        let port = server.port();
        let state = server.state().to_string();
        let cb = server.callback_url();
        assert_eq!(cb, format!("http://127.0.0.1:{port}/callback/{state}"));

        let attacker = tokio::spawn(async move {
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            s.write_all(b"GET /callback/wrong?code=evil HTTP/1.1\r\nHost: x\r\n\r\n")
                .await
                .unwrap();
            let mut buf = String::new();
            s.read_to_string(&mut buf).await.unwrap();
            buf
        });
        let good = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            s.write_all(
                format!("GET /callback/{state}?code=good-code HTTP/1.1\r\nHost: x\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
            let mut buf = String::new();
            s.read_to_string(&mut buf).await.unwrap();
            buf
        });
        let code = server.wait_for_code(Duration::from_secs(5)).await.unwrap();
        assert_eq!(code, "good-code");
        assert!(attacker.await.unwrap().starts_with("HTTP/1.1 404"));
        assert!(good.await.unwrap().starts_with("HTTP/1.1 200"));
    }

    #[tokio::test]
    async fn loopback_server_times_out() {
        let server = CallbackServer::bind().await.unwrap();
        let started = std::time::Instant::now();
        assert!(matches!(
            server.wait_for_code(Duration::from_millis(200)).await,
            Err(PkceError::Timeout(_))
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

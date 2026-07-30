use super::{OAuthTokens, generate_pkce, parse_authorization_input};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const AUTHORIZE_URL: &str = "https://openrouter.ai/auth";
const TOKEN_URL: &str = "https://openrouter.ai/api/v1/auth/keys";
const MODELS_URL: &str = "https://openrouter.ai/api/v1/models";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CALLBACK_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug)]
pub struct OpenRouterOAuth {
    authorize_url: String,
    token_url: String,
    client: reqwest::Client,
}

impl Default for OpenRouterOAuth {
    fn default() -> Self {
        Self::new(AUTHORIZE_URL, TOKEN_URL).expect("static OpenRouter OAuth URLs are valid")
    }
}

impl OpenRouterOAuth {
    pub fn new(authorize_url: impl Into<String>, token_url: impl Into<String>) -> Result<Self> {
        let authorize_url = authorize_url.into();
        let token_url = token_url.into();
        url::Url::parse(&authorize_url).context("invalid OAuth authorization URL")?;
        url::Url::parse(&token_url).context("invalid OAuth token URL")?;
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("build OAuth HTTP client")?;
        Ok(Self {
            authorize_url,
            token_url,
            client,
        })
    }

    pub async fn login(&self) -> Result<OAuthTokens> {
        let pkce = generate_pkce();
        let callback_path = format!("/oauth/callback/{}", uuid::Uuid::new_v4());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("listen for OpenRouter OAuth callback")?;
        let port = listener
            .local_addr()
            .context("read OpenRouter callback address")?
            .port();
        let callback_url = format!("http://127.0.0.1:{port}{callback_path}");
        let authorization_url = self.authorization_url(&callback_url, &pkce.challenge)?;

        eprintln!("Open this URL to sign in with OpenRouter:\n");
        eprintln!("{authorization_url}\n");
        eprintln!(
            "Waiting for the browser callback. On a remote machine, paste the final redirect URL or authorization code here:"
        );
        if let Err(error) = webbrowser::open(authorization_url.as_str()) {
            eprintln!("Could not open a browser automatically: {error}");
        }

        let manual = wait_for_manual_code(manual_code_receiver());
        let code = tokio::time::timeout(LOGIN_TIMEOUT, async {
            tokio::select! {
                callback = wait_for_callback(listener, &callback_path) => callback,
                manual = manual => manual,
            }
        })
        .await
        .map_err(|_| anyhow!("OpenRouter OAuth login timed out after five minutes"))??;

        eprintln!("Exchanging the authorization code...");
        self.exchange_code(&code, &pkce.verifier).await
    }

    fn authorization_url(&self, callback_url: &str, challenge: &str) -> Result<url::Url> {
        let mut url =
            url::Url::parse(&self.authorize_url).context("parse OAuth authorization URL")?;
        url.query_pairs_mut()
            .append_pair("callback_url", callback_url)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(url)
    }

    pub async fn exchange_code(&self, code: &str, verifier: &str) -> Result<OAuthTokens> {
        let response = self
            .client
            .post(&self.token_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&json!({
                "code": code,
                "code_verifier": verifier,
                "code_challenge_method": "S256"
            }))
            .send()
            .await
            .context("exchange OpenRouter OAuth authorization code")?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .context("OpenRouter OAuth returned invalid JSON")?;
        if !status.is_success() {
            let detail = error_detail(&body).unwrap_or("unknown error");
            bail!("OpenRouter OAuth key exchange failed (HTTP {status}): {detail}");
        }
        let key = body
            .get("key")
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty())
            .context("OpenRouter OAuth response carries no key")?;
        Ok(OAuthTokens {
            access: key.to_owned(),
            refresh: None,
            expires_at_ms: None,
            extra: std::collections::BTreeMap::new(),
        })
    }

    pub async fn model_context_window(access_token: &str, model: &str) -> Result<u64> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("build OpenRouter HTTP client")?;
        fetch_model_context_window(&client, MODELS_URL, access_token, model).await
    }
}

async fn fetch_model_context_window(
    client: &reqwest::Client,
    models_url: &str,
    access_token: &str,
    model: &str,
) -> Result<u64> {
    let response = client
        .get(models_url)
        .bearer_auth(access_token)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .context("fetch OpenRouter model catalog")?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .context("OpenRouter model catalog returned invalid JSON")?;
    if !status.is_success() {
        let detail = error_detail(&body).unwrap_or("unknown error");
        bail!("OpenRouter model catalog failed (HTTP {status}): {detail}");
    }
    let context = body
        .get("data")
        .and_then(Value::as_array)
        .and_then(|models| {
            models.iter().find_map(|entry| {
                (entry.get("id").and_then(Value::as_str) == Some(model))
                    .then(|| entry.get("context_length").and_then(Value::as_u64))
                    .flatten()
            })
        })
        .filter(|context| *context > 0)
        .with_context(|| {
            format!("OpenRouter did not return a context window for model {model:?}")
        })?;
    Ok(context)
}

fn manual_code_receiver() -> tokio::sync::oneshot::Receiver<String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut input = String::new();
        if std::io::stdin()
            .read_line(&mut input)
            .is_ok_and(|count| count > 0)
            && !input.trim().is_empty()
        {
            let _ = sender.send(input);
        }
    });
    receiver
}

async fn wait_for_manual_code(receiver: tokio::sync::oneshot::Receiver<String>) -> Result<String> {
    if let Ok(input) = receiver.await
        && let Some(code) = parse_authorization_input(&input)
    {
        return Ok(code);
    }
    std::future::pending().await
}

async fn wait_for_callback(
    listener: tokio::net::TcpListener,
    callback_path: &str,
) -> Result<String> {
    'connections: loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(connection) => connection,
            Err(error) => {
                return Err(error).context("accept OpenRouter OAuth callback");
            }
        };
        let mut request = Vec::with_capacity(2048);
        loop {
            if request.len() >= MAX_CALLBACK_BYTES {
                let _ =
                    send_response(&mut stream, 431, "OAuth callback request was too large.").await;
                continue 'connections;
            }
            let mut chunk = [0_u8; 1024];
            let count = match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
                .await
            {
                Ok(Ok(count)) => count,
                Ok(Err(error)) => {
                    eprintln!("Ignored malformed local OAuth callback: {error}");
                    continue 'connections;
                }
                Err(_) => {
                    let _ =
                        send_response(&mut stream, 408, "OAuth callback request timed out.").await;
                    continue 'connections;
                }
            };
            if count == 0 {
                continue 'connections;
            }
            request.extend_from_slice(&chunk[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let request = match std::str::from_utf8(&request) {
            Ok(request) => request,
            Err(_) => {
                let _ =
                    send_response(&mut stream, 400, "OAuth callback was not valid UTF-8.").await;
                continue;
            }
        };
        let Some(first_line) = request.lines().next() else {
            let _ = send_response(&mut stream, 400, "OAuth callback request was empty.").await;
            continue;
        };
        let mut parts = first_line.split_whitespace();
        let method = parts.next();
        let target = parts.next();
        if method != Some("GET") {
            let _ = send_response(&mut stream, 405, "OAuth callback must use GET.").await;
            continue;
        }
        let Some(target) = target else {
            let _ = send_response(&mut stream, 400, "OAuth callback URL was missing.").await;
            continue;
        };
        let callback = match url::Url::parse(&format!("http://127.0.0.1{target}")) {
            Ok(callback) => callback,
            Err(_) => {
                let _ = send_response(&mut stream, 400, "OAuth callback URL was invalid.").await;
                continue;
            }
        };
        if callback.path() != callback_path {
            let _ = send_response(&mut stream, 404, "OAuth callback route was not found.").await;
            continue;
        }
        if let Some(error) = callback
            .query_pairs()
            .find_map(|(key, value)| (key == "error").then(|| value.into_owned()))
        {
            let _ = send_response(&mut stream, 400, "OpenRouter authorization was denied.").await;
            bail!("OpenRouter authorization failed: {error}");
        }
        let Some(code) = callback.query_pairs().find_map(|(key, value)| {
            (key == "code" && !value.is_empty()).then(|| value.into_owned())
        }) else {
            let _ = send_response(
                &mut stream,
                400,
                "OpenRouter returned no authorization code.",
            )
            .await;
            continue;
        };
        let _ = send_response(
            &mut stream,
            200,
            "Authorization received. Return to Workshop to finish signing in.",
        )
        .await;
        return Ok(code);
    }
}

async fn send_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    message: &str,
) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        408 => "Request Timeout",
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    };
    let body =
        format!("<!doctype html><meta charset=\"utf-8\"><title>Workshop</title><h1>{message}</h1>");
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Cache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .context("write OAuth callback response")
}

fn error_detail(body: &Value) -> Option<&str> {
    body.get("error_description")
        .and_then(Value::as_str)
        .or_else(|| body.get("message").and_then(Value::as_str))
        .or_else(|| body.get("error").and_then(Value::as_str))
        .or_else(|| {
            body.get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn exchanges_code_for_openrouter_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/keys"))
            .and(body_json(json!({
                "code": "auth-code",
                "code_verifier": "verifier",
                "code_challenge_method": "S256"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"key": "sk-or-test"})))
            .mount(&server)
            .await;
        let oauth = OpenRouterOAuth::new(
            "https://openrouter.ai/auth",
            format!("{}/keys", server.uri()),
        )
        .unwrap();

        let tokens = oauth.exchange_code("auth-code", "verifier").await.unwrap();

        assert_eq!(tokens.access, "sk-or-test");
        assert_eq!(tokens.refresh, None);
        assert_eq!(tokens.expires_at_ms, None);
    }

    #[tokio::test]
    async fn reports_provider_exchange_error_without_leaking_code() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(json!({"error": "invalid_grant"})),
            )
            .mount(&server)
            .await;
        let oauth = OpenRouterOAuth::new(
            "https://openrouter.ai/auth",
            format!("{}/keys", server.uri()),
        )
        .unwrap();

        let error = oauth
            .exchange_code("sensitive-code", "sensitive-verifier")
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("invalid_grant"));
        assert!(!error.contains("sensitive-code"));
        assert!(!error.contains("sensitive-verifier"));
    }

    #[tokio::test]
    async fn resolves_context_window_from_model_catalog() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"id": "other/model", "context_length": 1000},
                    {"id": "author/model", "context_length": 131072}
                ]
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();

        let context = fetch_model_context_window(
            &client,
            &format!("{}/models", server.uri()),
            "token",
            "author/model",
        )
        .await
        .unwrap();

        assert_eq!(context, 131072);
    }

    #[test]
    fn authorization_url_contains_pkce_and_callback() {
        let oauth = OpenRouterOAuth::default();
        let url = oauth
            .authorization_url("http://127.0.0.1:1234/callback", "challenge")
            .unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(
            query.get("callback_url").map(|value| value.as_ref()),
            Some("http://127.0.0.1:1234/callback")
        );
        assert_eq!(
            query.get("code_challenge").map(|value| value.as_ref()),
            Some("challenge")
        );
        assert_eq!(
            query
                .get("code_challenge_method")
                .map(|value| value.as_ref()),
            Some("S256")
        );
    }

    #[tokio::test]
    async fn closed_manual_input_does_not_cancel_browser_callback() {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        drop(sender);

        assert!(
            tokio::time::timeout(Duration::from_millis(20), wait_for_manual_code(receiver))
                .await
                .is_err()
        );
    }
}

//! Live proofs (network-gated; each test prints `SKIP: …` and passes when its target is not
//! reachable): a streaming, tool-capable turn through `xai-grok-sampler` against
//!
//! * the keyless Kilo Gateway free pool (`kilo-auto/free` with the plan's fallback chain), and
//! * a local Ollama server, using the model the first-run default rule would pick.
//!
//! Plus the OpenRouter PKCE exchange endpoint (live: a bogus code must be rejected with 400, which
//! proves the request shape is accepted) and a full loopback PKCE round-trip against a mock.
//!
//! Run with `cargo test -p workshop-providers --test live_turns -- --nocapture` to see the
//! observed models, chunk counts, tool calls, and any 429s.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use workshop_providers::catalog::custom_row;
use workshop_providers::oauth::openrouter::{EXCHANGE_URL, exchange_client, exchange_code};
use workshop_providers::{
    Catalog, CatalogFetcher, CatalogModel, CredentialBroker, DefaultSelection, Freshness,
    KILO_DEFAULT_CHAIN, LocalServerStatus, MemorySecretStore, OpenRouterSignIn, ProviderError,
    SignInMode, manifest, probe_server, sampler_config_for, select_default,
};
use xai_grok_sampler::SamplingClient;
use xai_grok_sampling_types::{
    ChatCompletionRequest, ChatRequestMessage, FinishReason, ToolChoice, ToolDefinition,
};

fn tcp_reachable(host: &str, port: u16) -> bool {
    let Ok(mut addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    addrs.any(|a| TcpStream::connect_timeout(&a, Duration::from_secs(3)).is_ok())
}

fn read_file_tool() -> ToolDefinition {
    ToolDefinition::function(
        "read_file",
        Some("Read a file from the workspace and return its contents"),
        serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string", "description": "Path relative to the workspace root" } },
            "required": ["path"],
            "additionalProperties": false
        }),
    )
}

#[derive(Debug, Default)]
struct TurnSummary {
    chunks: usize,
    text: String,
    tool_name: Option<String>,
    tool_args: String,
    finish: Option<FinishReason>,
    usage_prompt_tokens: Option<u32>,
}

impl TurnSummary {
    fn produced_something(&self) -> bool {
        self.tool_name.is_some() || !self.text.trim().is_empty()
    }
}

/// One streaming turn with a `read_file` tool and a prompt that invites using it.
async fn streaming_tool_turn(
    model: &CatalogModel,
    broker: &CredentialBroker,
) -> Result<TurnSummary, String> {
    let handle = broker
        .resolve(&model.provider_id)
        .map_err(|e| e.to_string())?;
    let mut cfg = sampler_config_for(model, &handle).map_err(|e| e.to_string())?;
    cfg.max_completion_tokens = Some(200);
    cfg.idle_timeout_secs = Some(90);
    let client = SamplingClient::new(cfg).map_err(|e| e.to_string())?;

    let mut request = ChatCompletionRequest::new(
        model.model_id.clone(),
        vec![
            ChatRequestMessage::system(
                "You are a coding agent. When asked about a file, call the read_file tool instead of guessing.",
            ),
            ChatRequestMessage::user("What is in README.md? Use the read_file tool to read it."),
        ],
    );
    request.tools = Some(vec![read_file_tool()]);
    request.tool_choice = Some(ToolChoice::auto());
    request.temperature = Some(0.0);

    let (mut stream, _meta) = client
        .chat_completion_stream(request)
        .await
        .map_err(|e| e.to_string())?;
    let mut summary = TurnSummary::default();
    let deadline = Instant::now() + Duration::from_secs(120);
    while let Some(item) = stream.next().await {
        if Instant::now() > deadline {
            return Err("stream exceeded 120s".into());
        }
        let chunk = item.map_err(|e| e.to_string())?;
        summary.chunks += 1;
        if let Some(u) = &chunk.usage {
            summary.usage_prompt_tokens = Some(u.prompt_tokens);
        }
        for choice in chunk.choices {
            if let Some(c) = choice.delta.content {
                summary.text.push_str(&c);
            }
            for tc in choice.delta.tool_calls {
                if let Some(f) = tc.function {
                    if let Some(name) = f.name {
                        summary.tool_name = Some(name);
                    }
                    if let Some(args) = f.arguments {
                        summary.tool_args.push_str(&args);
                    }
                }
            }
            if choice.finish_reason.is_some() {
                summary.finish = choice.finish_reason;
            }
        }
    }
    Ok(summary)
}

fn anonymous_broker(tmp: &tempfile::TempDir) -> CredentialBroker {
    CredentialBroker::new(
        Arc::new(MemorySecretStore::default()),
        tmp.path().join("connections.json"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kilo_free_pool_streams_a_tool_capable_turn_keyless() {
    if !tcp_reachable("api.kilo.ai", 443) {
        eprintln!("SKIP: api.kilo.ai unreachable (offline)");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let broker = anonymous_broker(&tmp);
    let cat = Catalog::builtin();
    let started = Instant::now();
    let mut last_error = String::new();
    let mut rate_limited: Vec<String> = Vec::new();
    for model_id in KILO_DEFAULT_CHAIN {
        let model = cat.get(&format!("kilo:{model_id}")).unwrap().clone();
        assert!(
            model.is_keyless(),
            "the default chain must need no credential"
        );
        match streaming_tool_turn(&model, &broker).await {
            Ok(summary) => {
                eprintln!(
                    "LIVE kilo {model_id}: {} chunks in {:?}, finish={:?}, tool={:?} args={:?}, text={:?}, prompt_tokens={:?}",
                    summary.chunks,
                    started.elapsed(),
                    summary.finish,
                    summary.tool_name,
                    summary.tool_args.chars().take(120).collect::<String>(),
                    summary.text.chars().take(120).collect::<String>(),
                    summary.usage_prompt_tokens
                );
                if !rate_limited.is_empty() {
                    eprintln!("LIVE kilo: rate-limited/failed before success: {rate_limited:?}");
                }
                assert!(
                    summary.chunks > 1,
                    "expected a streamed response, got {} chunk(s)",
                    summary.chunks
                );
                assert!(
                    summary.produced_something(),
                    "no text and no tool call: {summary:?}"
                );
                if let Some(name) = &summary.tool_name {
                    assert_eq!(name, "read_file");
                    let args: serde_json::Value =
                        serde_json::from_str(&summary.tool_args).expect("tool args are JSON");
                    assert!(args.get("path").is_some(), "{args}");
                }
                return;
            }
            Err(e) => {
                eprintln!("LIVE kilo {model_id}: {e}");
                if e.contains("429") || e.to_lowercase().contains("rate") {
                    rate_limited.push(model_id.to_string());
                }
                last_error = e;
            }
        }
    }
    // A shared pool can be exhausted; report rather than fail CI on someone else's quota.
    if !rate_limited.is_empty() {
        eprintln!("SKIP: every Kilo free model in the chain was rate-limited: {rate_limited:?}");
        return;
    }
    panic!("no Kilo free model completed a turn: {last_error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kilo_catalog_fetches_live_and_caches() {
    if !tcp_reachable("api.kilo.ai", 443) {
        eprintln!("SKIP: api.kilo.ai unreachable (offline)");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = CatalogFetcher::new(tmp.path().join("cache"), Duration::from_secs(3600)).unwrap();
    let kilo = manifest("kilo").unwrap();
    let live = fetcher.fetch(&kilo).await;
    assert_eq!(live.freshness, Freshness::Live, "{:?}", live.error);
    assert!(
        live.rows.iter().any(|r| r.model_id == "kilo-auto/free"),
        "{:?}",
        live.rows.iter().map(|r| &r.model_id).collect::<Vec<_>>()
    );
    assert!(live.rows.iter().all(|r| r.is_keyless() && r.is_free()));
    eprintln!(
        "LIVE kilo catalog: {} free rows, e.g. {:?}",
        live.rows.len(),
        live.rows
            .iter()
            .take(5)
            .map(|r| r.model_id.as_str())
            .collect::<Vec<_>>()
    );
    let cached = fetcher.fetch(&kilo).await;
    assert_eq!(cached.freshness, Freshness::Cached);
    assert_eq!(cached.rows.len(), live.rows.len());

    let or = manifest("openrouter").unwrap();
    let or_live = fetcher.fetch(&or).await;
    assert_eq!(or_live.freshness, Freshness::Live, "{:?}", or_live.error);
    assert!(or_live.rows.iter().any(|r| r.model_id == "openrouter/free"));
    assert!(
        or_live.rows.iter().all(|r| !r.is_keyless()),
        "OpenRouter free rows need the signed-in key"
    );
    eprintln!("LIVE openrouter catalog: {} free rows", or_live.rows.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_ollama_streams_a_tool_capable_turn() {
    let ollama = manifest("ollama").unwrap();
    let status: LocalServerStatus = probe_server(&ollama, Duration::from_secs(10)).await;
    if !status.is_reachable() {
        eprintln!("SKIP: no Ollama on 127.0.0.1:11434");
        return;
    }
    eprintln!(
        "LIVE ollama {:?}: {} models: {:?}",
        status.version,
        status.models.len(),
        status
            .models
            .iter()
            .map(|m| format!(
                "{} tools={:?} params={:?}B ctx={:?}",
                m.id, m.tools, m.parameters_b, m.context_window
            ))
            .collect::<Vec<_>>()
    );
    if status.models.is_empty() {
        eprintln!("SKIP: Ollama has no models pulled");
        return;
    }
    // The first-run default rule: tool-capable and ≥ 7B.
    let selection = select_default(std::slice::from_ref(&status), false);
    eprintln!("LIVE ollama default selection: {selection:?}");
    let tmp = tempfile::tempdir().unwrap();
    let broker = anonymous_broker(&tmp);
    broker.add_local("ollama").unwrap();

    // Run every default-worthy model (largest first); fall back to any model for the wire test.
    let mut candidates: Vec<&workshop_providers::LocalModel> = status
        .models
        .iter()
        .filter(|m| m.default_worthy())
        .collect();
    candidates.sort_by(|a, b| {
        b.parameters_b
            .partial_cmp(&a.parameters_b)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let wire_only = candidates.is_empty();
    if wire_only {
        candidates.push(&status.models[0]);
    }
    let mut structured_tool_call = false;
    let mut text_only_tool_json: Vec<String> = Vec::new();
    for local in candidates {
        let mut model = custom_row("ollama", &local.id).unwrap();
        model.context_window = local.context_window;
        let started = Instant::now();
        let summary = streaming_tool_turn(&model, &broker)
            .await
            .expect("local streaming turn");
        eprintln!(
            "LIVE ollama {}: {} chunks in {:?}, finish={:?}, tool={:?} args={:?}, text={:?}",
            local.id,
            summary.chunks,
            started.elapsed(),
            summary.finish,
            summary.tool_name,
            summary.tool_args.chars().take(120).collect::<String>(),
            summary.text.chars().take(120).collect::<String>()
        );
        assert!(summary.chunks >= 1);
        assert!(summary.produced_something(), "{summary:?}");
        if summary.tool_name.as_deref() == Some("read_file") {
            let args: serde_json::Value =
                serde_json::from_str(&summary.tool_args).expect("tool args are JSON");
            assert!(
                args["path"].as_str().is_some_and(|p| p.contains("README")),
                "{args}"
            );
            assert_eq!(summary.finish, Some(FinishReason::ToolCalls));
            structured_tool_call = true;
        } else if summary.text.contains("read_file") {
            // The model wrote the call as text: Ollama's template for this model does not
            // produce structured tool calls even though `/api/show` advertises `tools`.
            text_only_tool_json.push(local.id.clone());
        }
    }
    if !text_only_tool_json.is_empty() {
        eprintln!(
            "LIVE ollama: models that emitted the tool call as plain text (not usable as a default): {text_only_tool_json:?}"
        );
    }
    if !wire_only {
        assert!(
            matches!(selection, DefaultSelection::Local { .. }),
            "a default-worthy local model must be preselected: {selection:?}"
        );
        assert!(
            structured_tool_call,
            "no default-worthy local model produced a structured tool call"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openrouter_exchange_endpoint_rejects_a_bogus_code() {
    if !tcp_reachable("openrouter.ai", 443) {
        eprintln!("SKIP: openrouter.ai unreachable (offline)");
        return;
    }
    let client = exchange_client().unwrap();
    let err = exchange_code(
        &client,
        EXCHANGE_URL,
        "bogus-code-for-workshop-test",
        "bogus-verifier",
    )
    .await
    .unwrap_err();
    eprintln!("LIVE openrouter PKCE exchange with bogus code: {err}");
    match err {
        workshop_providers::oauth::PkceError::Exchange { status, .. } => {
            assert!(
                (400..500).contains(&status),
                "expected a 4xx rejection, got {status}"
            );
        }
        other => panic!("unexpected: {other}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openrouter_pkce_loopback_round_trip_against_a_mock() {
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock = MockServer::start().await;
    let sign_in = OpenRouterSignIn::start(SignInMode::Loopback)
        .await
        .unwrap()
        .with_exchange_url(format!("{}/api/v1/auth/keys", mock.uri()));
    let auth_url = url::Url::parse(&sign_in.authorize_url()).unwrap();
    assert_eq!(auth_url.host_str(), Some("openrouter.ai"));
    let params: std::collections::HashMap<_, _> = auth_url.query_pairs().into_owned().collect();
    let callback = params
        .get("callback_url")
        .expect("loopback callback present")
        .clone();
    let challenge = params.get("code_challenge").unwrap().clone();
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert!(callback.starts_with("http://127.0.0.1:"));

    Mock::given(method("POST"))
        .and(path("/api/v1/auth/keys"))
        .and(body_partial_json(
            serde_json::json!({ "code": "the-code", "code_challenge_method": "S256" }),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "key": "sk-or-v1-minted" })),
        )
        .expect(1)
        .mount(&mock)
        .await;

    // The "browser" follows OpenRouter's redirect to the loopback callback.
    let browser = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        #[allow(clippy::disallowed_methods, reason = "loopback-only test client")]
        let client = reqwest::Client::builder().build().unwrap();
        client
            .get(format!("{callback}?code=the-code"))
            .send()
            .await
            .unwrap()
            .status()
    });
    #[allow(
        clippy::disallowed_methods,
        reason = "talks only to the wiremock server on loopback"
    )]
    let plain = reqwest::Client::builder().build().unwrap();
    let key = sign_in
        .complete_loopback(&plain, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(key, "sk-or-v1-minted");
    assert!(browser.await.unwrap().is_success());

    // The exchange carried the verifier that matches the challenge in the authorize URL.
    let received = mock.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    let verifier = body["code_verifier"].as_str().unwrap();
    let expected_challenge = {
        use base64::Engine;
        use sha2::Digest;
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(verifier.as_bytes()))
    };
    assert_eq!(expected_challenge, challenge);

    // The minted key lands in Workshop's own broker, bound to OpenRouter's host only.
    let tmp = tempfile::tempdir().unwrap();
    let broker = anonymous_broker(&tmp);
    broker.save_api_key("openrouter", &key).unwrap();
    let handle = broker.resolve("openrouter").unwrap();
    assert_eq!(
        handle
            .authorize("https://openrouter.ai/api/v1/chat/completions")
            .unwrap(),
        Some("sk-or-v1-minted")
    );
    assert!(matches!(
        handle.authorize("https://api.kilo.ai/api/gateway/chat/completions"),
        Err(ProviderError::HostNotAllowed { .. })
    ));
}

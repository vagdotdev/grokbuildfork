//! Local servers: loopback-only reachability, model enumeration, and the "good enough for the
//! default" rule (tool-capable, ≥ 7B). Presence-only GETs with short timeouts; never a LAN sweep.
//!
//! Live-verified shapes (2026-09-21): Ollama `GET /api/tags`, `POST /api/show` (capabilities,
//! `details.parameter_size`, `model_info.<arch>.context_length`); llama.cpp `GET /v1/models`
//! (`data[].meta.n_params`, `n_ctx_train`) and `GET /props` (`chat_template`); LM Studio / vLLM
//! `GET /v1/models` (`data[].id`).

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::catalog::{CatalogModel, CatalogSource, row_for};
use crate::manifest::{ProviderManifest, builtin_manifests};

/// Parameter floor for a model to be preselected as the first-run default.
pub const DEFAULT_MIN_PARAMETERS_B: f64 = 7.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalHealth {
    /// Something accepted a TCP connection on the loopback port.
    Reachable,
    Unreachable,
    /// The manifest's host is not loopback; Workshop does not probe it.
    NotLoopback,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalModel {
    pub id: String,
    /// Advertises tool calling (`None` = the server does not say).
    pub tools: Option<bool>,
    /// Billions of parameters, from the server or parsed from the name.
    pub parameters_b: Option<f64>,
    pub context_window: Option<u64>,
}

impl LocalModel {
    /// Meets the first-run default floor: tool-capable and at least [`DEFAULT_MIN_PARAMETERS_B`].
    pub fn default_worthy(&self) -> bool {
        self.tools == Some(true)
            && self
                .parameters_b
                .is_some_and(|p| p >= DEFAULT_MIN_PARAMETERS_B)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalServerStatus {
    pub provider_id: String,
    pub base_url: String,
    pub health: LocalHealth,
    pub version: Option<String>,
    pub models: Vec<LocalModel>,
}

impl LocalServerStatus {
    pub fn is_reachable(&self) -> bool {
        self.health == LocalHealth::Reachable
    }

    /// Best default candidate: tool-capable and largest.
    pub fn default_candidate(&self) -> Option<&LocalModel> {
        self.models
            .iter()
            .filter(|m| m.default_worthy())
            .max_by(|a, b| {
                a.parameters_b
                    .partial_cmp(&b.parameters_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }
}

fn loopback_addr(base_url: &str) -> Option<SocketAddr> {
    let url = Url::parse(base_url).ok()?;
    let host = url.host_str()?;
    let port = url.port_or_known_default()?;
    let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs().ok()?.collect();
    addrs.into_iter().find(|a| a.ip().is_loopback())
}

/// TCP connect to the manifest's loopback endpoint with `timeout` (sync, presence only).
pub fn probe_local(manifest: &ProviderManifest, timeout: Duration) -> LocalHealth {
    if !manifest.is_local() {
        return LocalHealth::NotLoopback;
    }
    match loopback_addr(&manifest.base_url) {
        None => LocalHealth::NotLoopback,
        Some(addr) => match TcpStream::connect_timeout(&addr, timeout) {
            Ok(_) => LocalHealth::Reachable,
            Err(_) => LocalHealth::Unreachable,
        },
    }
}

/// Probe every Local manifest for presence. Sequential, bounded by `timeout` each.
pub fn probe_all_local(timeout: Duration) -> Vec<(ProviderManifest, LocalHealth)> {
    builtin_manifests()
        .into_iter()
        .filter(|m| m.is_local())
        .map(|m| {
            let health = probe_local(&m, timeout);
            (m, health)
        })
        .collect()
}

/// `"7.6B"` / `"494.03M"` / `"27b"` → billions.
pub fn parse_parameter_size(s: &str) -> Option<f64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| c.is_ascii_alphabetic())?);
    let n: f64 = num.trim().parse().ok()?;
    match unit.chars().next()?.to_ascii_lowercase() {
        'b' => Some(n),
        'm' => Some(n / 1000.0),
        'k' => Some(n / 1_000_000.0),
        _ => None,
    }
}

/// Parameter count guessed from a model name like `qwen2.5-coder:7b` or `Qwen3-27B-Q4_K_M.gguf`.
pub fn parameters_from_name(name: &str) -> Option<f64> {
    let lower = name.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut best: Option<f64> = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            if i < bytes.len()
                && bytes[i] == b'b'
                && (i + 1 == bytes.len() || !bytes[i + 1].is_ascii_alphanumeric())
                && let Ok(n) = lower[start..i].parse::<f64>()
            {
                best = Some(best.map_or(n, |b: f64| b.max(n)));
            }
        } else {
            i += 1;
        }
    }
    best
}

/// Loopback-only reqwest client for local servers.
fn loopback_client(timeout: Duration) -> reqwest::Result<reqwest::Client> {
    #[allow(
        clippy::disallowed_methods,
        reason = "loopback-only client; no TLS policy applies to 127.0.0.1"
    )]
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout.min(Duration::from_secs(2)))
        .no_proxy()
        .build()
}

async fn get_json(client: &reqwest::Client, url: &str) -> Option<Value> {
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Value>().await.ok()
}

fn v1_models(v: &Value) -> Vec<LocalModel> {
    v.get("data")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let id = r.get("id")?.as_str()?.to_string();
                    let meta = r.get("meta");
                    let parameters_b = meta
                        .and_then(|m| m.get("n_params"))
                        .and_then(Value::as_f64)
                        .map(|n| n / 1e9)
                        .or_else(|| parameters_from_name(&id));
                    let context_window = meta
                        .and_then(|m| m.get("n_ctx_train"))
                        .and_then(Value::as_u64);
                    Some(LocalModel {
                        id,
                        tools: None,
                        parameters_b,
                        context_window,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn enumerate_ollama(
    client: &reqwest::Client,
    base: &str,
) -> (Option<String>, Vec<LocalModel>) {
    let root = base.trim_end_matches("/v1");
    let version = get_json(client, &format!("{root}/api/version"))
        .await
        .and_then(|v| v.get("version").and_then(Value::as_str).map(str::to_string));
    let mut models = Vec::new();
    if let Some(tags) = get_json(client, &format!("{root}/api/tags")).await
        && let Some(list) = tags.get("models").and_then(Value::as_array)
    {
        for item in list.iter().take(32) {
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                continue;
            };
            let mut model = LocalModel {
                id: name.to_string(),
                tools: None,
                parameters_b: item
                    .pointer("/details/parameter_size")
                    .and_then(Value::as_str)
                    .and_then(parse_parameter_size)
                    .or_else(|| parameters_from_name(name)),
                context_window: None,
            };
            // `/api/show` reports capabilities and the architecture's context length.
            if let Ok(resp) = client
                .post(format!("{root}/api/show"))
                .json(&serde_json::json!({ "model": name }))
                .send()
                .await
                && resp.status().is_success()
                && let Ok(show) = resp.json::<Value>().await
            {
                if let Some(caps) = show.get("capabilities").and_then(Value::as_array) {
                    model.tools = Some(caps.iter().any(|c| c.as_str() == Some("tools")));
                }
                if let Some(info) = show.get("model_info").and_then(Value::as_object) {
                    model.context_window = info
                        .iter()
                        .find(|(k, _)| k.ends_with(".context_length"))
                        .and_then(|(_, v)| v.as_u64());
                }
                if model.parameters_b.is_none() {
                    model.parameters_b = show
                        .pointer("/details/parameter_size")
                        .and_then(Value::as_str)
                        .and_then(parse_parameter_size);
                }
            }
            models.push(model);
        }
    }
    (version, models)
}

async fn enumerate_llamacpp(
    client: &reqwest::Client,
    base: &str,
) -> (Option<String>, Vec<LocalModel>) {
    let root = base.trim_end_matches("/v1");
    let mut models = match get_json(client, &format!("{base}/models")).await {
        Some(v) => v1_models(&v),
        None => Vec::new(),
    };
    let props = get_json(client, &format!("{root}/props")).await;
    let version = props.as_ref().and_then(|p| {
        p.get("build_info")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    if let Some(p) = &props {
        let template_has_tools = p
            .get("chat_template")
            .and_then(Value::as_str)
            .map(|t| t.contains("tool"));
        let n_ctx = p
            .pointer("/default_generation_settings/n_ctx")
            .and_then(Value::as_u64);
        for m in &mut models {
            if m.tools.is_none() {
                m.tools = template_has_tools;
            }
            if m.context_window.is_none() {
                m.context_window = n_ctx;
            }
        }
    }
    (version, models)
}

async fn enumerate_openai_compatible(client: &reqwest::Client, base: &str) -> Vec<LocalModel> {
    match get_json(client, &format!("{base}/models")).await {
        Some(v) => v1_models(&v),
        None => Vec::new(),
    }
}

/// Reachability plus model enumeration for one Local manifest.
pub async fn probe_server(manifest: &ProviderManifest, timeout: Duration) -> LocalServerStatus {
    let mut status = LocalServerStatus {
        provider_id: manifest.id.clone(),
        base_url: manifest.base_url.clone(),
        health: probe_local(manifest, timeout.min(Duration::from_millis(800))),
        version: None,
        models: Vec::new(),
    };
    if status.health != LocalHealth::Reachable {
        return status;
    }
    let Ok(client) = loopback_client(timeout) else {
        return status;
    };
    let (version, models) = match manifest.id.as_str() {
        "ollama" => enumerate_ollama(&client, &manifest.base_url).await,
        "llamacpp" => enumerate_llamacpp(&client, &manifest.base_url).await,
        _ => (
            None,
            enumerate_openai_compatible(&client, &manifest.base_url).await,
        ),
    };
    status.version = version;
    status.models = models;
    status
}

/// Probe and enumerate every Local manifest concurrently.
pub async fn probe_all_local_servers(timeout: Duration) -> Vec<LocalServerStatus> {
    let manifests: Vec<ProviderManifest> = builtin_manifests()
        .into_iter()
        .filter(|m| m.is_local())
        .collect();
    let futures: Vec<_> = manifests.iter().map(|m| probe_server(m, timeout)).collect();
    let mut out = Vec::with_capacity(futures.len());
    for f in futures {
        out.push(f.await);
    }
    out
}

/// Catalog rows for a detected server's models (`Free · Local · Offline`).
pub fn local_rows(
    manifest: &ProviderManifest,
    status: &LocalServerStatus,
    as_of: &str,
) -> Vec<CatalogModel> {
    status
        .models
        .iter()
        .map(|m| {
            let mut row = row_for(
                manifest,
                &m.id,
                &m.id,
                CatalogSource {
                    name: format!(
                        "{} ({})",
                        status.base_url,
                        status.version.clone().unwrap_or_else(|| "local".into())
                    ),
                    as_of: as_of.into(),
                },
            );
            row.tools = m.tools;
            row.context_window = m.context_window;
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn parameter_parsing() {
        assert_eq!(parse_parameter_size("7.6B"), Some(7.6));
        assert!((parse_parameter_size("494.03M").unwrap() - 0.49403).abs() < 1e-9);
        assert_eq!(parse_parameter_size("garbage"), None);
        assert_eq!(parameters_from_name("qwen2.5-coder:7b"), Some(7.0));
        assert_eq!(parameters_from_name("Qwen3-27B-Q4_K_M.gguf"), Some(27.0));
        assert_eq!(parameters_from_name("gpt-oss:120b"), Some(120.0));
        assert_eq!(parameters_from_name("qwen2.5:0.5b"), Some(0.5));
        assert_eq!(parameters_from_name("llama3"), None);
        assert_eq!(parameters_from_name("model-q4_0-b16"), None);
    }

    #[test]
    fn default_floor_requires_tools_and_size() {
        let small = LocalModel {
            id: "qwen2.5:0.5b".into(),
            tools: Some(true),
            parameters_b: Some(0.5),
            context_window: None,
        };
        let big_no_tools = LocalModel {
            id: "x".into(),
            tools: Some(false),
            parameters_b: Some(70.0),
            context_window: None,
        };
        let unknown = LocalModel {
            id: "y".into(),
            tools: None,
            parameters_b: Some(8.0),
            context_window: None,
        };
        let good = LocalModel {
            id: "qwen2.5-coder:7b".into(),
            tools: Some(true),
            parameters_b: Some(7.6),
            context_window: Some(32768),
        };
        assert!(
            !small.default_worthy() && !big_no_tools.default_worthy() && !unknown.default_worthy()
        );
        assert!(good.default_worthy());
        let status = LocalServerStatus {
            provider_id: "ollama".into(),
            base_url: "http://127.0.0.1:11434/v1".into(),
            health: LocalHealth::Reachable,
            version: None,
            models: vec![
                small,
                good.clone(),
                LocalModel {
                    id: "big".into(),
                    tools: Some(true),
                    parameters_b: Some(32.0),
                    context_window: None,
                },
            ],
        };
        assert_eq!(status.default_candidate().unwrap().id, "big");
    }

    #[test]
    fn v1_models_reads_llamacpp_meta() {
        let v: Value = serde_json::json!({"data":[{"id":"/models/Qwen3-27B-Q4.gguf","object":"model","meta":{"n_params":27_000_000_000u64,"n_ctx_train":40960}}]});
        let models = v1_models(&v);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].parameters_b, Some(27.0));
        assert_eq!(models[0].context_window, Some(40960));
        let v: Value =
            serde_json::json!({"data":[{"id":"qwen2.5-coder-7b-instruct","object":"model"}]});
        assert_eq!(v1_models(&v)[0].parameters_b, Some(7.0));
    }

    #[test]
    fn detects_a_listening_loopback_port_and_nothing_else() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut m = crate::manifest::manifest("ollama").unwrap();
        m.base_url = format!("http://127.0.0.1:{port}/v1");
        assert_eq!(
            probe_local(&m, Duration::from_millis(300)),
            LocalHealth::Reachable
        );
        drop(listener);
        let started = std::time::Instant::now();
        assert_eq!(
            probe_local(&m, Duration::from_millis(300)),
            LocalHealth::Unreachable
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        let direct = crate::manifest::manifest("openai").unwrap();
        assert_eq!(
            probe_local(&direct, Duration::from_millis(300)),
            LocalHealth::NotLoopback
        );
    }

    #[test]
    fn non_loopback_hosts_are_never_probed() {
        let mut m = crate::manifest::manifest("ollama").unwrap();
        m.base_url = "http://192.168.1.50:11434/v1".into();
        assert_eq!(
            probe_local(&m, Duration::from_millis(100)),
            LocalHealth::NotLoopback
        );
    }

    #[tokio::test]
    async fn unreachable_server_yields_no_models_quickly() {
        let mut m = crate::manifest::manifest("vllm").unwrap();
        m.base_url = "http://127.0.0.1:9/v1".into();
        let started = std::time::Instant::now();
        let status = probe_server(&m, Duration::from_secs(1)).await;
        assert_eq!(status.health, LocalHealth::Unreachable);
        assert!(status.models.is_empty());
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}

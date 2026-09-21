//! Gate 3 — `PRODUCTION_ENDPOINTS` (`gate:no-xai`).
//!
//! The compiled production endpoint set, the shell's auxiliary API default,
//! and the bundled default model catalog must not point at `grok.com` / `x.ai`.
//! Workshop's idle defaults are reserved `.invalid` placeholders (fail at DNS)
//! until a configured connection replaces them.

use workshop_gates::{HermeticEnv, url_has_forbidden_host};
use xai_grok_env::{
    GrokBuildEnvironment, PROD_ASSET_SERVER_URL, PROD_CLI_CHAT_PROXY_BASE_URL,
    PROD_GATEWAY_WS_URL, PROD_RELAY_WS_URL, PROD_WS_ORIGIN,
};

#[test]
fn compiled_production_endpoints_have_no_xai_hosts() {
    let endpoints = GrokBuildEnvironment::Production.endpoints();
    let all = [
        ("cli_chat_proxy_base_url", endpoints.cli_chat_proxy_base_url),
        ("asset_server_url", endpoints.asset_server_url),
        ("relay_ws_url", endpoints.relay_ws_url),
        ("gateway_ws_url", endpoints.gateway_ws_url),
        ("ws_origin", endpoints.ws_origin),
    ];
    let offenders: Vec<String> = all
        .iter()
        .filter(|(_, url)| url_has_forbidden_host(url))
        .map(|(name, url)| format!("{name} = {url}"))
        .collect();
    assert!(
        offenders.is_empty(),
        "PRODUCTION_ENDPOINTS still target xAI infrastructure:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn exported_prod_constants_match_and_are_not_xai() {
    for url in [
        PROD_CLI_CHAT_PROXY_BASE_URL,
        PROD_ASSET_SERVER_URL,
        PROD_RELAY_WS_URL,
        PROD_GATEWAY_WS_URL,
        PROD_WS_ORIGIN,
    ] {
        assert!(!url_has_forbidden_host(url), "{url}");
        assert!(
            url::Url::parse(url).is_ok(),
            "{url} must stay a parseable URL so startup code does not panic"
        );
    }
}

#[test]
fn resolved_production_urls_without_env_overrides_are_not_xai() {
    let _env = HermeticEnv::production();
    let env = GrokBuildEnvironment::Production;
    for url in [
        env.cli_chat_proxy_base_url(),
        env.asset_server_url(),
        env.relay_ws_url(),
        env.gateway_ws_url(),
        env.ws_origin(),
    ] {
        assert!(!url_has_forbidden_host(&url), "{url}");
    }
}

#[test]
fn shell_auxiliary_api_default_is_not_xai() {
    // Web search / image describe / voice fall back to this when no model
    // carries its own base URL. It must not be a hidden xAI fallback.
    assert!(
        !url_has_forbidden_host(xai_grok_shell::agent::config::XAI_API_BASE_URL_DEFAULT),
        "XAI_API_BASE_URL_DEFAULT = {}",
        xai_grok_shell::agent::config::XAI_API_BASE_URL_DEFAULT
    );
    assert!(
        !url_has_forbidden_host(xai_grok_shell_base::env::PROD_COMPUTER_HUB_WS_URL),
        "PROD_COMPUTER_HUB_WS_URL = {}",
        xai_grok_shell_base::env::PROD_COMPUTER_HUB_WS_URL
    );
    // The shell's own proxy fallback (used when `[endpoints] cli_chat_proxy_base_url` is unset)
    // is a separate constant from PRODUCTION_ENDPOINTS and must be neutral too.
    assert!(
        !url_has_forbidden_host(xai_grok_shell::agent::config::CLI_CHAT_PROXY_BASE_URL_DEFAULT),
        "CLI_CHAT_PROXY_BASE_URL_DEFAULT = {}",
        xai_grok_shell::agent::config::CLI_CHAT_PROXY_BASE_URL_DEFAULT
    );
}

#[test]
fn default_model_catalog_is_not_grok_via_the_proxy() {
    let default = xai_grok_models::default_model();
    assert!(
        !default.to_ascii_lowercase().starts_with("grok"),
        "default model is still a Grok model routed through cli-chat-proxy: {default}"
    );
    for aux in [
        xai_grok_models::default_web_search_model(),
        xai_grok_models::default_image_description_model(),
        xai_grok_models::default_session_summary_model(),
    ] {
        assert!(
            !aux.to_ascii_lowercase().starts_with("grok"),
            "aux tool model is still Grok: {aux}"
        );
    }
    let json: serde_json_lite::Value =
        serde_json_lite::parse(xai_grok_models::DEFAULT_MODELS_JSON);
    for id in json.model_ids() {
        assert!(
            !id.to_ascii_lowercase().contains("grok"),
            "bundled catalog still ships a Grok model: {id}"
        );
    }
}

/// Minimal JSON reader for the bundled catalog, so this gate does not add a
/// serde_json dev-dependency that could shift feature unification.
mod serde_json_lite {
    pub struct Value(String);

    pub fn parse(src: &str) -> Value {
        Value(src.to_string())
    }

    impl Value {
        /// Every `"model": "<id>"` value in the catalog, by a tolerant scan.
        pub fn model_ids(&self) -> Vec<String> {
            let mut out = Vec::new();
            let mut rest = self.0.as_str();
            while let Some(pos) = rest.find("\"model\"") {
                let after = rest.get(pos + "\"model\"".len()..).unwrap_or("");
                let after = after.trim_start().trim_start_matches(':').trim_start();
                if let Some(stripped) = after.strip_prefix('"')
                    && let Some(end) = stripped.find('"')
                {
                    out.push(stripped.get(..end).unwrap_or("").to_string());
                }
                rest = after;
            }
            out
        }
    }
}

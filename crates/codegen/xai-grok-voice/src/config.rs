use serde::{Deserialize, Serialize};

use crate::error::VoiceError;

/// Default STT capture rate (Hz). Shared with the `__mic-capture` helper's argv default so parent and child agree when `--rate` is omitted.
pub const DEFAULT_SAMPLE_RATE: u32 = 16_000;

/// Which speech-to-text engine `/voice` talks to.
///
/// Workshop overlay: the default is the local `voice-engine` helper (whisper.cpp, no account, no
/// network after install). The inherited xAI WebSocket client is opt-in: `provider = "xai"` plus an
/// explicit `api_base` and the user's own key; a normal install never constructs it.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VoiceProvider {
    #[default]
    Local,
    Xai,
}

/// `[voice]` config. For `provider = "xai"`, prefer an https `api_base` (same shape as chat);
/// [`Self::stt_ws_url`] derives `wss://` and, when `[voice].api_base` is unset, inherits
/// `[endpoints].xai_api_base_url` so enterprise proxies need no second knob. The local provider
/// ignores `api_base`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct VoiceConfig {
    pub provider: VoiceProvider,
    /// Workshop overlay: force a Whisper model tier (`turbo` / `small` / `base`). Unset = chosen
    /// per machine (voice-spec §9.6). Undocumented on purpose.
    pub model: Option<String>,
    /// Workshop overlay: path of the `voice-engine` helper; unset = beside the running binary.
    pub engine_path: Option<String>,
    /// HTTPS API root (or bare host) for `provider = "xai"`. Empty for a normal launch.
    /// Bases may end in `/v1` or `/xai/v1`; the default STT path de-duplicates a leading `v1/` so both become `…/v1/stt`.
    pub api_base: String,
    pub stt_ws_path: String,
    /// Preferred STT language (catalog code or `"auto"`). [`crate::language_for_api`] resolves it at connect time.
    pub language: String,
    pub sample_rate: u32,
    pub stt_endpointing_ms: u32,
    pub stt_interim_results: bool,

    /// The pager stamps this request identity; `serde(skip)` keeps user config from setting it.
    #[serde(skip)]
    pub client_identifier: String,
    #[serde(skip)]
    pub user_agent: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            provider: VoiceProvider::Local,
            model: None,
            engine_path: None,
            // No hosted endpoint by default (gate:no-xai): the xAI provider requires an explicit base.
            api_base: String::new(),
            stt_ws_path: "/v1/stt".into(),
            language: "en".into(),
            sample_rate: DEFAULT_SAMPLE_RATE,
            stt_endpointing_ms: 400,
            stt_interim_results: true,
            client_identifier: String::new(),
            user_agent: String::new(),
        }
    }
}

impl VoiceConfig {
    /// Streaming STT WebSocket URL. Rejects plaintext `http://` / `ws://`.
    pub fn stt_ws_url(&self) -> Result<String, VoiceError> {
        ws_url(&self.api_base, &self.stt_ws_path)
    }

    /// `api_base` for `provider = "xai"`: non-empty `[voice].api_base`, else `[endpoints].xai_api_base_url` from `root`,
    /// else `resolved_endpoints_base`. The local provider keeps `api_base` empty: no hosted endpoint is ever derived.
    ///
    /// `resolved_endpoints_base` carries the caller's env/CLI overrides; it ranks below the raw table so config keeps beating env (shell precedence).
    pub fn from_config_table(root: &toml::Table, resolved_endpoints_base: Option<&str>) -> Self {
        let voice_table = root.get("voice").and_then(|v| v.as_table());
        let mut cfg: Self = voice_table
            .and_then(|t| toml::Value::Table(t.clone()).try_into().ok())
            .unwrap_or_default();

        // Read `[voice].api_base` from the raw table, not `cfg`: serde default makes "unset" and an explicit value indistinguishable
        let explicit = non_empty_str(
            voice_table
                .and_then(|t| t.get("api_base"))
                .and_then(|v| v.as_str()),
        );
        cfg.api_base = match cfg.provider {
            VoiceProvider::Xai => explicit
                .or_else(|| {
                    non_empty_str(
                        root.get("endpoints")
                            .and_then(|e| e.get("xai_api_base_url"))
                            .and_then(|v| v.as_str()),
                    )
                })
                .or_else(|| non_empty_str(resolved_endpoints_base))
                .map(|base| base.trim_end_matches('/').to_owned())
                .unwrap_or_default(),
            VoiceProvider::Local => String::new(),
        };
        cfg
    }
}

fn non_empty_str(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// `strip_prefix` ignoring ASCII case: RFC 3986 schemes are case-insensitive.
/// `HTTP://` must hit the plaintext rejection and `HTTPS://` must work.
fn strip_scheme<'a>(s: &'a str, scheme: &str) -> Option<&'a str> {
    s.get(..scheme.len())
        .filter(|p| p.eq_ignore_ascii_case(scheme))
        .and_then(|_| s.get(scheme.len()..))
}

fn ws_url(api_base: &str, path: &str) -> Result<String, VoiceError> {
    let base = api_base.trim().trim_end_matches('/');
    let path = path.trim().trim_start_matches('/');
    if base.is_empty() {
        return Err(VoiceError::Config(
            "voice provider \"xai\" needs [voice].api_base (or [endpoints].xai_api_base_url); \
             Workshop sets no hosted speech endpoint by default"
                .into(),
        ));
    }
    if strip_scheme(base, "http://").is_some() || strip_scheme(base, "ws://").is_some() {
        return Err(VoiceError::Config(format!(
            "insecure voice api_base {api_base:?}: voice requires a TLS endpoint \
             (https:// / wss://). Refusing to send the bearer token over a \
             plaintext connection."
        )));
    }
    let rest = strip_scheme(base, "https://")
        .or_else(|| strip_scheme(base, "wss://"))
        .unwrap_or(base);
    // The default path is `/v1/stt`; bases often end in `/v1` or `/xai/v1`
    let path = match (rest.ends_with("/v1"), path.strip_prefix("v1/")) {
        (true, Some(rest_path)) => rest_path,
        _ => path,
    };
    Ok(format!("wss://{rest}/{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Workshop overlay (gate:no-xai): a default launch has no hosted speech endpoint at all.
    #[test]
    fn default_is_local_with_no_hosted_endpoint() {
        let cfg = VoiceConfig::default();
        assert_eq!(cfg.provider, VoiceProvider::Local);
        assert!(cfg.api_base.is_empty());
        assert!(cfg.model.is_none());
        assert!(matches!(cfg.stt_ws_url(), Err(VoiceError::Config(_))));
        let cfg =
            VoiceConfig::from_config_table(&toml::Table::new(), Some("https://env.example.com"));
        assert_eq!(cfg.provider, VoiceProvider::Local);
        assert!(
            cfg.api_base.is_empty(),
            "local ignores every endpoint fallback"
        );
    }

    fn xai(base: &str) -> VoiceConfig {
        VoiceConfig {
            provider: VoiceProvider::Xai,
            api_base: base.into(),
            ..VoiceConfig::default()
        }
    }

    #[test]
    fn scheme_less_and_wss_bases() {
        for base in [
            "stt.example.com",
            "wss://stt.example.com",
            "HTTPS://stt.example.com",
        ] {
            assert_eq!(
                xai(base).stt_ws_url().unwrap(),
                "wss://stt.example.com/v1/stt"
            );
        }
    }

    #[test]
    fn v1_base_dedupes_default_path() {
        assert_eq!(
            xai("https://proxy.example.com/v1").stt_ws_url().unwrap(),
            "wss://proxy.example.com/v1/stt"
        );
    }

    #[test]
    fn xai_v1_base_preserves_prefix() {
        assert_eq!(
            xai("https://proxy.example.com/xai/v1")
                .stt_ws_url()
                .unwrap(),
            "wss://proxy.example.com/xai/v1/stt"
        );
    }

    #[test]
    fn rejects_plaintext_bases() {
        for base in [
            "http://localhost:8080",
            "ws://localhost:8080",
            "HTTP://localhost:8080",
            "Ws://localhost:8080",
        ] {
            assert!(matches!(xai(base).stt_ws_url(), Err(VoiceError::Config(_))));
        }
    }

    #[test]
    fn xai_provider_inherits_endpoints_when_voice_api_base_unset() {
        let table: toml::Table = toml::from_str(
            r#"
[endpoints]
xai_api_base_url = "https://proxy.example.com/xai/v1"
[voice]
provider = "xai"
"#,
        )
        .unwrap();
        let cfg = VoiceConfig::from_config_table(&table, None);
        assert_eq!(cfg.provider, VoiceProvider::Xai);
        assert_eq!(cfg.api_base, "https://proxy.example.com/xai/v1");
        assert_eq!(
            cfg.stt_ws_url().unwrap(),
            "wss://proxy.example.com/xai/v1/stt"
        );
    }

    #[test]
    fn local_provider_never_inherits_endpoints() {
        let table: toml::Table = toml::from_str(
            r#"
[endpoints]
xai_api_base_url = "https://proxy.example.com/xai/v1"
[voice]
language = "fr"
"#,
        )
        .unwrap();
        let cfg = VoiceConfig::from_config_table(&table, Some("https://env.example.com"));
        assert_eq!(cfg.provider, VoiceProvider::Local);
        assert!(cfg.api_base.is_empty());
        assert_eq!(cfg.language, "fr");
    }

    #[test]
    fn xai_empty_voice_api_base_still_inherits_endpoints() {
        let table: toml::Table = toml::from_str(
            r#"
[endpoints]
xai_api_base_url = "https://proxy.example.com/xai/v1"
[voice]
provider = "xai"
api_base = "  "
language = "fr"
"#,
        )
        .unwrap();
        let cfg = VoiceConfig::from_config_table(&table, None);
        assert_eq!(cfg.api_base, "https://proxy.example.com/xai/v1");
        assert_eq!(cfg.language, "fr");
    }

    #[test]
    fn xai_without_any_base_has_no_endpoint() {
        let table: toml::Table = toml::from_str(
            r#"
[voice]
provider = "xai"
api_base = "  "
"#,
        )
        .unwrap();
        let cfg = VoiceConfig::from_config_table(&table, None);
        assert!(cfg.api_base.is_empty());
        assert!(matches!(cfg.stt_ws_url(), Err(VoiceError::Config(_))));
    }

    #[test]
    fn xai_resolved_endpoints_base_used_when_table_has_none() {
        let table: toml::Table = toml::from_str("[voice]\nprovider = \"xai\"\n").unwrap();
        let cfg = VoiceConfig::from_config_table(&table, Some("https://proxy.example.com/v1/"));
        assert_eq!(cfg.api_base, "https://proxy.example.com/v1");
        assert_eq!(cfg.stt_ws_url().unwrap(), "wss://proxy.example.com/v1/stt");

        // Whitespace-only resolved base leaves the endpoint empty.
        let cfg = VoiceConfig::from_config_table(&table, Some("  "));
        assert!(cfg.api_base.is_empty());
    }

    /// config.toml beats the env/CLI fallback, matching the shell's endpoints precedence.
    #[test]
    fn table_endpoints_beat_resolved_endpoints_base() {
        let table: toml::Table = toml::from_str(
            r#"
[endpoints]
xai_api_base_url = "https://config.example.com"
[voice]
provider = "xai"
"#,
        )
        .unwrap();
        let cfg = VoiceConfig::from_config_table(&table, Some("https://env.example.com"));
        assert_eq!(cfg.api_base, "https://config.example.com");
    }

    #[test]
    fn voice_api_base_overrides_endpoints() {
        let table: toml::Table = toml::from_str(
            r#"
[endpoints]
xai_api_base_url = "https://proxy.example.com/xai/v1"
[voice]
provider = "xai"
api_base = "https://stt.example.com"
language = "es"
"#,
        )
        .unwrap();
        let cfg = VoiceConfig::from_config_table(&table, None);
        assert_eq!(cfg.api_base, "https://stt.example.com");
        assert_eq!(cfg.language, "es");
        assert_eq!(cfg.stt_ws_url().unwrap(), "wss://stt.example.com/v1/stt");
    }

    #[test]
    fn workshop_keys_parse_and_unknown_or_identity_fields_are_ignored() {
        let table: toml::Table = toml::from_str(
            r#"
[voice]
enabled = false
client_identifier = "spoofed"
user_agent = "malicious/9.9"
language = "es"
model = "small"
engine_path = "/opt/workshop/voice-engine"
"#,
        )
        .unwrap();
        let cfg = VoiceConfig::from_config_table(&table, None);
        assert_eq!(cfg.language, "es");
        assert_eq!(cfg.model.as_deref(), Some("small"));
        assert_eq!(
            cfg.engine_path.as_deref(),
            Some("/opt/workshop/voice-engine")
        );
        assert!(cfg.client_identifier.is_empty());
        assert!(cfg.user_agent.is_empty());
    }
}
